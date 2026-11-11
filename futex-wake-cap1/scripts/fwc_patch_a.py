#!/usr/bin/env python3
"""FUTEX-WAKE-CAP1 part A: per-queue user-slot reservations in the scheduler.
Usage: fwc_patch_a.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:90], s.count(old))
    p.write_text(s.replace(old, new))
S = "src/kernel/scheduler.rs"
patch(S, '''    membership_state: [u8; MEMBERSHIP_SLOTS],
    membership_tracking_exhausted: bool,
}

impl Default for PriorityScheduler {''', '''    membership_state: [u8; MEMBERSHIP_SLOTS],
    membership_tracking_exhausted: bool,
    /// FUTEX-WAKE-CAP1 — user slots promised per priority queue and not yet filled. A wake that
    /// must not mutate its waiter until a placement is guaranteed reserves first; every user push
    /// counts these as occupied, so the promise cannot be spent by anyone else. Bounded by
    /// [`MAX_RUN_QUEUE`] per queue, because a reservation is refused once
    /// `len + reserved` reaches it.
    reserved: [usize; 3],
}

impl Default for PriorityScheduler {''')
patch(S, '''            membership_state: [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS],
            membership_tracking_exhausted: false,
        }
    }
}''', '''            membership_state: [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS],
            membership_tracking_exhausted: false,
            reserved: [0; 3],
        }
    }
}''')
patch(S, '''    pub fn enqueue_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        self.queues[Self::priority_index(priority)].push(tid)?;''', '''    pub fn enqueue_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        // FUTEX-WAKE-CAP1: a reserved slot is occupied for every user push but the one it was
        // reserved for.
        if !crate::kernel::kthread::is_kthread_tid(tid.0) && !self.user_room(priority) {
            return Err(SchedulerError::QueueFull);
        }
        self.push_unchecked_capacity(tid, priority)
    }

    /// FUTEX-WAKE-CAP1 — whether one more user entry fits at `priority`, counting reservations.
    fn user_room(&self, priority: TaskPriority) -> bool {
        let p = Self::priority_index(priority);
        self.queues[p].len + self.reserved[p] < MAX_RUN_QUEUE
    }

    /// FUTEX-WAKE-CAP1 — promise one user slot at `priority`. Refused (`QueueFull`) exactly when
    /// an ordinary user push would be; nothing is mutated on a refusal.
    pub(crate) fn reserve_user_slot(&mut self, priority: TaskPriority) -> Result<(), SchedulerError> {
        if !self.user_room(priority) {
            return Err(SchedulerError::QueueFull);
        }
        self.reserved[Self::priority_index(priority)] += 1;
        Ok(())
    }

    /// FUTEX-WAKE-CAP1 — give back a promise that will not be used.
    pub(crate) fn release_user_slot(&mut self, priority: TaskPriority) {
        let p = Self::priority_index(priority);
        debug_assert!(self.reserved[p] > 0, "released a user slot nobody reserved");
        self.reserved[p] = self.reserved[p].saturating_sub(1);
    }

    /// FUTEX-WAKE-CAP1 — the user slots currently promised at `priority`.
    pub(crate) fn reserved_user_slots(&self, priority: TaskPriority) -> usize {
        self.reserved[Self::priority_index(priority)]
    }

    /// FUTEX-WAKE-CAP1 — fill a promised slot. The reservation is consumed whatever the outcome;
    /// the only refusal is `AlreadyQueued` (the task is already placed here). Capacity cannot
    /// refuse: at the reservation `len + reserved < MAX_RUN_QUEUE`, every later user push keeps
    /// `len + reserved <= MAX_RUN_QUEUE`, and kernel threads add at most
    /// `KTHREAD_CAPACITY` entries, so `len < MAX_QUEUED_PER_PRIORITY` holds here.
    pub(crate) fn enqueue_reserved_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.release_user_slot(priority);
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        self.push_unchecked_capacity(tid, priority)
    }

    /// The push itself, after the caller's capacity rule. The ring's physical bound is still
    /// checked; for a user TID it is the kernel-thread headroom that a reserved fill may use.
    fn push_unchecked_capacity(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.queues[Self::priority_index(priority)].push_within(tid, MAX_QUEUED_PER_PRIORITY)?;''')
patch(S, '''    fn push(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        // KTHREAD1-ACCEPTANCE §1: the user limit is unchanged; kernel threads have reserved room.
        let limit = if crate::kernel::kthread::is_kthread_tid(tid.0) {
            MAX_QUEUED_PER_PRIORITY
        } else {
            MAX_RUN_QUEUE
        };
        if self.len >= limit {''', '''    /// Push within the ring's physical bound. The user limit ([`MAX_RUN_QUEUE`], counting
    /// reservations) is the caller's: `PriorityScheduler::enqueue_with_priority` applies it to
    /// every user push, and a reserved fill was admitted by it when it reserved. KTHREAD1-ACCEPTANCE
    /// §1: kernel threads keep their reserved room.
    fn push_within(&mut self, tid: ThreadId, limit: usize) -> Result<(), SchedulerError> {
        if self.len >= limit.min(MAX_QUEUED_PER_PRIORITY) {''')
# CpuView wrappers.
patch(S, '''    pub fn enqueue(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_with_priority(tid, TaskPriority::Normal)
    }''', '''    pub fn enqueue(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_with_priority(tid, TaskPriority::Normal)
    }

    /// FUTEX-WAKE-CAP1 — promise one user slot on this CPU at `priority`, under its 1L. Refused
    /// for exactly the CPU states an enqueue is refused for (offline, wake-only) and for a full
    /// queue; nothing is mutated on a refusal.
    pub(crate) fn reserve_user_slot(&mut self, priority: TaskPriority) -> Result<(), SchedulerError> {
        self.validate_online()?;
        if self.wake_only() {
            return Err(SchedulerError::WakeOnly);
        }
        self.sched.rq.reserve_user_slot(priority)
    }

    /// FUTEX-WAKE-CAP1 — give back an unused promise on this CPU.
    pub(crate) fn release_user_slot(&mut self, priority: TaskPriority) {
        self.sched.rq.release_user_slot(priority);
    }

    /// FUTEX-WAKE-CAP1 — the promised slots on this CPU at `priority` (tests and witnesses).
    pub(crate) fn reserved_user_slots(&self, priority: TaskPriority) -> usize {
        self.sched.rq.reserved_user_slots(priority)
    }

    /// FUTEX-WAKE-CAP1 — fill a slot promised by [`Self::reserve_user_slot`] on this CPU. The CPU
    /// was online and not wake-only when the promise was made, and neither changes back.
    pub(crate) fn enqueue_reserved_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.sched.rq.enqueue_reserved_with_priority(tid, priority)
    }''')
print("patch A applied")
