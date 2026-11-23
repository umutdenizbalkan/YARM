// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
use crate::arch::platform_constants;
#[cfg(test)]
use crate::arch::topology;
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler_timer::Timer;
use crate::kernel::ticket_lock::{TicketLockIrq, TicketLockIrqGuard};
use crate::kernel::time::{Tick, TickInstant};
use crate::kernel::topology::CpuTopology;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};
pub use yarm_kernel::scheduler::{CpuId, SchedulerError, TaskPriority};

pub const MAX_RUN_QUEUE: usize = 64;
pub const MAX_CPUS: usize = platform_constants::MAX_CPUS;
const _: () = assert!(MAX_RUN_QUEUE.is_power_of_two());
/// KTHREAD1-ACCEPTANCE §1 — the most entries one priority queue can hold. A user push is refused
/// once a queue holds [`MAX_RUN_QUEUE`] entries (kernel threads included), exactly as before; a
/// kernel-thread push may use [`KTHREAD_CAPACITY`](crate::kernel::kthread::KTHREAD_CAPACITY) more.
/// A kernel thread is queued only on its home CPU and at most once there (`AlreadyQueued`), so a
/// queue never holds more than that many kernel-thread entries, and a kernel thread's enqueue —
/// its wake, its yield, its creation — is never refused for capacity.
///
/// SCHED-TIME-FOUNDATION1 A (SCHED-FULL-ROT1): plus ONE entry for the returning occupant. The
/// rotation owners return the outgoing current task to its own queue without the admission check
/// ([`PriorityScheduler::requeue_current_with_priority`]); that is what lets a CPU whose queue is
/// at the user limit keep rotating. Admission still stops every other user push at
/// [`MAX_RUN_QUEUE`], so a queue holds at most `MAX_RUN_QUEUE + 1` user entries (proof at
/// `requeue_current_with_priority`) beside at most `KTHREAD_CAPACITY` kernel threads.
pub const ROTATION_SLOTS: usize = 1;
pub const MAX_QUEUED_PER_PRIORITY: usize =
    MAX_RUN_QUEUE + ROTATION_SLOTS + crate::kernel::kthread::KTHREAD_CAPACITY;

/// RT-SCHED1A — the priority levels the run queues hold (SC-PRI-10: 32, level 0 highest).
pub const PRIORITY_LEVELS: usize = TaskPriority::LEVELS;
const _: () = assert!(PRIORITY_LEVELS == 32);
/// RT-SCHED1A — the user entries (queued + reserved) ONE CPU admits over all its levels. A user
/// push is refused once `user queued + reserved` reaches this, beside the unchanged per-level rule
/// (`len + reserved < MAX_RUN_QUEUE`).
///
/// **It never refuses a push the per-level rule admits in a GP configuration** (three levels):
/// SCHED-FULL-ROT1's bound gives `V[p] <= MAX_RUN_QUEUE + 1` for each level, so
/// `Σ (queued + reserved) <= 3 × (MAX_RUN_QUEUE + 1)`, and reaching it means every level holds
/// `MAX_RUN_QUEUE + 1 > MAX_RUN_QUEUE - 1` entries, which the per-level rule already refuses. In
/// an RT configuration (up to 32 levels) it is what bounds the CPU's storage: without it the
/// per-level rule alone would admit `32 × 64` entries.
pub const MAX_RUN_QUEUE_TOTAL: usize = 3 * (MAX_RUN_QUEUE + ROTATION_SLOTS);
/// RT-SCHED1A — the most entries one CPU's queues can hold, all levels together:
/// * `MAX_RUN_QUEUE_TOTAL` admitted user entries (exited entries that stay queued until a dispatch
///   discards them count here: they were admitted and are still entries);
/// * `ROTATION_SLOTS` for the returning occupant (the current task requeued without admission);
/// * `KTHREAD_CAPACITY` kernel threads (each queued only on its home, at most once);
/// * one for tid 0, the AP idle placeholder installed rather than admitted.
///
/// Reservations hold no entry until filled, and their fill was admitted at reservation.
pub const MAX_QUEUED_PER_CPU: usize =
    MAX_RUN_QUEUE_TOTAL + ROTATION_SLOTS + crate::kernel::kthread::KTHREAD_CAPACITY + 1;
/// RT-SCHED1A — the entry pool every level of one CPU links its FIFO through.
const POOL_SLOTS: usize = 256;
const _: () = assert!(POOL_SLOTS >= MAX_QUEUED_PER_CPU && POOL_SLOTS < NIL as usize);
/// The end of a level's list, and of the free list.
const NIL: u16 = u16::MAX;

/// Per-CPU slot for the recv-timeout split-read optimization.
///
/// The arch trap-entry seam writes a pre-computed absolute deadline here before
/// acquiring the global `SharedKernel` lock.  The recv-timeout syscall handler
/// reads and clears the slot (consuming it atomically with `swap`); if the
/// value is non-zero the handler calls `ipc_recv_until_deadline` directly,
/// skipping the redundant tick read that would otherwise occur inside the lock.
///
/// Zero means "no pre-read deadline available."  Since deadlines are computed as
/// `now.wrapping_add(timeout_ticks)` with `timeout_ticks > 0`, a zero result is
/// theoretically possible but astronomically rare; in that case the handler falls
/// back to the normal `ipc_recv_with_deadline` path, which is always correct.
pub(crate) static SPLIT_RECV_TIMEOUT_DEADLINE: [core::sync::atomic::AtomicU64; MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; MAX_CPUS];
const MEMBERSHIP_SLOTS: usize = 256;
const MEMBERSHIP_EMPTY: u8 = 0;
const MEMBERSHIP_TOMBSTONE: u8 = 1;
const MEMBERSHIP_FULL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledTask {
    tid: ThreadId,
    priority: TaskPriority,
}

/// RT-SCHED1A — one CPU's run queues: [`PRIORITY_LEVELS`] FIFO lists linked through ONE pool of
/// [`POOL_SLOTS`] entries, with a bitmap of the non-empty levels.
///
/// Storage per CPU: `POOL_SLOTS × (8 + 2)` bytes of entries and links plus `PRIORITY_LEVELS × 6`
/// bytes of heads, tails and lengths — about 2.8 KiB, against the 3 × 1040 bytes of the three
/// 128-slot rings it replaces. Thirty-two such rings would have been 33 KiB per CPU (2.1 MiB over
/// `MAX_CPUS`), for capacity no admission rule could ever fill.
///
/// Every operation is bounded by occupancy, never by the pool: push at either end and pop at the
/// head are O(1); removing a given entry walks only its own level (≤ that level's length); the
/// highest non-empty level is one `trailing_zeros` of the bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunQueues {
    tids: [ThreadId; POOL_SLOTS],
    next: [u16; POOL_SLOTS],
    free: u16,
    head: [u16; PRIORITY_LEVELS],
    tail: [u16; PRIORITY_LEVELS],
    len: [u16; PRIORITY_LEVELS],
    /// Entries that are not kernel threads, over every level (the total admission's count).
    user: u16,
    /// Bit `l` set iff level `l` holds an entry.
    nonempty: u32,
}

/// Stage 199D-WA3A-R1 — what a dispatch step actually did, produced **inside** the
/// authoritative scheduler mutation.
///
/// Provenance is not derivable after the fact: `dispatch_next` returns the existing `current`
/// without dequeuing whenever one is set and is not idle, so `outgoing != incoming`, TID equality
/// and "which helper was called" all fail to distinguish a genuine dequeue from a queue-neutral
/// continuation. Getting that wrong is not cosmetic — undoing a "dequeue" that never happened
/// enqueues the current task, which for a `Blocked(EndpointReceive)` current is exactly the
/// unarbitrated wake Stage 199D exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchSelection {
    /// A runqueue entry was **removed** and installed as `current`. Undoing this means
    /// `preempt_reenqueue_only`.
    Dequeued { tid: ThreadId },
    /// The existing `current` was returned. **No** runqueue entry was removed, so there is
    /// nothing to undo and `preempt_reenqueue_only` must never be called for it.
    ContinuedCurrent { tid: ThreadId },
    /// Nothing was selected.
    Idle,
}

impl DispatchSelection {
    /// The selected TID, if any.
    pub fn tid(self) -> Option<ThreadId> {
        match self {
            Self::Dequeued { tid } | Self::ContinuedCurrent { tid } => Some(tid),
            Self::Idle => None,
        }
    }

    /// A short stable name for markers.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Dequeued { .. } => "dequeued",
            Self::ContinuedCurrent { .. } => "continued_current",
            Self::Idle => "idle",
        }
    }
}

/// U9-DISPATCH-CPU1 §1 — what an ACCEPTANCE-FILTERED dispatch step found.
///
/// The plain [`DispatchSelection`] cannot express the difference that matters to a post-lock
/// drain: "there is nothing to run" and "there are things to run and none of them can actually be
/// marked `Running`" are different facts with different settlements, and both used to arrive as
/// `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptedSelection {
    /// A candidate the predicate ACCEPTED was removed from its exact queue position and installed
    /// as `current` — or the existing `current` was continued.
    Selected(DispatchSelection),
    /// There were no runqueue entries at all on this CPU. Idling is justified.
    Empty,
    /// Every runqueue entry was examined and the predicate rejected each one. `examined` is the
    /// count, so "exhausted" is a measurement of what was tried rather than an inference from a
    /// retry budget. Nothing was dequeued and no entry moved.
    NoneAcceptable { examined: usize },
}

impl AcceptedSelection {
    /// The selection, when one was made.
    pub fn selection(self) -> Option<DispatchSelection> {
        match self {
            Self::Selected(s) => Some(s),
            _ => None,
        }
    }

    /// A short stable name for markers.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Selected(s) => s.marker(),
            Self::Empty => "empty",
            Self::NoneAcceptable { .. } => "none_acceptable",
        }
    }
}

impl RunQueues {
    const fn new() -> Self {
        let mut next = [NIL; POOL_SLOTS];
        let mut i = 0;
        while i + 1 < POOL_SLOTS {
            next[i] = (i + 1) as u16;
            i += 1;
        }
        Self {
            tids: [ThreadId(0); POOL_SLOTS],
            next,
            free: 0,
            head: [NIL; PRIORITY_LEVELS],
            tail: [NIL; PRIORITY_LEVELS],
            len: [0; PRIORITY_LEVELS],
            user: 0,
            nonempty: 0,
        }
    }

    fn len(&self, level: usize) -> usize {
        self.len[level] as usize
    }

    /// Every entry, all levels.
    fn total(&self) -> usize {
        self.len.iter().map(|&l| l as usize).sum()
    }

    /// The highest-priority (lowest-numbered) non-empty level.
    fn best_level(&self) -> Option<usize> {
        if self.nonempty == 0 {
            None
        } else {
            Some(self.nonempty.trailing_zeros() as usize)
        }
    }

    /// The non-empty levels, highest priority first.
    fn levels(&self) -> impl Iterator<Item = usize> + use<> {
        let mut bits = self.nonempty;
        core::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let l = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            Some(l)
        })
    }

    /// The entries of `level`, in FIFO order.
    fn iter(&self, level: usize) -> impl Iterator<Item = ThreadId> + '_ {
        let mut at = self.head[level];
        core::iter::from_fn(move || {
            if at == NIL {
                return None;
            }
            let tid = self.tids[at as usize];
            at = self.next[at as usize];
            Some(tid)
        })
    }

    fn contains(&self, level: usize, tid: ThreadId) -> bool {
        self.iter(level).any(|t| t == tid)
    }

    /// Number of entries of `level` holding `tid`. A scan only.
    fn count_tid(&self, level: usize, tid: ThreadId) -> usize {
        self.iter(level).filter(|&t| t == tid).count()
    }

    /// KTHREAD1 — the entries of `level` that are user tasks (kernel-thread ids excluded).
    fn user_len(&self, level: usize) -> usize {
        self.iter(level)
            .filter(|t| !crate::kernel::kthread::is_kthread_tid(t.0))
            .count()
    }

    /// KTHREAD1 — the first entry of `level` that is not a kernel thread, in FIFO order.
    fn first_user(&self, level: usize) -> Option<ThreadId> {
        self.iter(level)
            .find(|t| !crate::kernel::kthread::is_kthread_tid(t.0))
    }

    /// Non-mutating peek of `level`'s head.
    fn peek(&self, level: usize) -> Option<ThreadId> {
        let h = self.head[level];
        (h != NIL).then(|| self.tids[h as usize])
    }

    /// Take a free entry for `tid` within `level`'s physical bound (`limit`, never above
    /// [`MAX_QUEUED_PER_PRIORITY`]) and the pool's.
    fn alloc(&mut self, level: usize, tid: ThreadId, limit: usize) -> Result<u16, SchedulerError> {
        if self.len(level) >= limit.min(MAX_QUEUED_PER_PRIORITY) || self.free == NIL {
            return Err(SchedulerError::QueueFull);
        }
        let at = self.free;
        self.free = self.next[at as usize];
        self.tids[at as usize] = tid;
        self.next[at as usize] = NIL;
        self.len[level] += 1;
        self.nonempty |= 1 << level;
        if !crate::kernel::kthread::is_kthread_tid(tid.0) {
            self.user += 1;
        }
        Ok(at)
    }

    /// Append at `level`'s TAIL (a wake, a yield, a rotation).
    fn push_back(
        &mut self,
        level: usize,
        tid: ThreadId,
        limit: usize,
    ) -> Result<(), SchedulerError> {
        let at = self.alloc(level, tid, limit)?;
        match self.tail[level] {
            NIL => self.head[level] = at,
            t => self.next[t as usize] = at,
        }
        self.tail[level] = at;
        Ok(())
    }

    /// Insert at `level`'s HEAD (SC-PRI-12 (a): a task preempted by a strictly higher priority).
    fn push_front(
        &mut self,
        level: usize,
        tid: ThreadId,
        limit: usize,
    ) -> Result<(), SchedulerError> {
        let at = self.alloc(level, tid, limit)?;
        self.next[at as usize] = self.head[level];
        if self.head[level] == NIL {
            self.tail[level] = at;
        }
        self.head[level] = at;
        Ok(())
    }

    /// Unlink entry `at` of `level`, whose predecessor is `prev` (`NIL` for the head), and
    /// return it to the pool.
    fn unlink(&mut self, level: usize, prev: u16, at: u16) -> ThreadId {
        let after = self.next[at as usize];
        match prev {
            NIL => self.head[level] = after,
            p => self.next[p as usize] = after,
        }
        if self.tail[level] == at {
            self.tail[level] = prev;
        }
        let tid = self.tids[at as usize];
        self.next[at as usize] = self.free;
        self.free = at;
        self.len[level] -= 1;
        if self.len[level] == 0 {
            self.nonempty &= !(1 << level);
        }
        if !crate::kernel::kthread::is_kthread_tid(tid.0) {
            self.user -= 1;
        }
        tid
    }

    fn pop_front(&mut self, level: usize) -> Option<ThreadId> {
        let h = self.head[level];
        (h != NIL).then(|| self.unlink(level, NIL, h))
    }

    /// Remove the first entry of `level` holding `tid`, keeping every other entry's order.
    fn remove_tid(&mut self, level: usize, tid: ThreadId) -> bool {
        let mut prev = NIL;
        let mut at = self.head[level];
        while at != NIL {
            if self.tids[at as usize] == tid {
                self.unlink(level, prev, at);
                return true;
            }
            prev = at;
            at = self.next[at as usize];
        }
        false
    }
}

/// U9-RECV-BLOCK2 §1 — the DISTINCT placements a task can hold, as answered by
/// [`SmpScheduler::placement_of`].
///
/// Four states, because the settlement that consumes them owes a different thing in each and the
/// single boolean it used to read could not tell them apart. `Queued` is waiting for a dispatch
/// and its saved context is the one it will resume from; `Current` is EXECUTING and its saved
/// context belongs to whoever is running it; `Nowhere` is neither, which for a `Runnable` task is
/// a scheduler invariant break; `CurrentAndQueued` is a double placement and is named rather than
/// folded into either, so a marker can say which invariant broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPlacement {
    /// On no run queue and in no CPU's current slot.
    Nowhere,
    /// Queued on this CPU, and current nowhere.
    Queued(CpuId),
    /// This CPU's current task, and on no run queue.
    Current(CpuId),
    /// Both at once — a scheduler invariant break. The CPU named is the one running it.
    CurrentAndQueued(CpuId),
}

/// Stage 199D: outcome of a non-dispatching runqueue withdrawal.
///
/// `bool` would be genuinely ambiguous here — a bare `false` would conflate "the TID was not
/// queued", "it is the CPU's current task", "it appears more than once" and "that CPU is not
/// online", which are four different facts with four different correct responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithdrawOutcome {
    /// Exactly one queued incarnation was removed.
    Removed,
    /// The TID holds no queued slot on that CPU. Nothing was mutated.
    NotQueued,
    /// The TID is that CPU's `current` task — including the scheduler-owned tid-0 idle
    /// placeholder. Refused BEFORE any mutation; withdrawal never touches `current`.
    RefusedCurrent,
    /// The TID occupies more than one queued slot on that CPU. Fail closed: no queue is
    /// modified, because removing "one of them" would be an arbitrary choice.
    RefusedDuplicate,
    /// The CPU id is out of range or not online.
    InvalidCpu,
}

#[derive(Debug)]
pub struct PriorityScheduler {
    /// RT-SCHED1A — the [`PRIORITY_LEVELS`] FIFO levels, linked through one per-CPU pool.
    queues: RunQueues,
    current: Option<ScheduledTask>,
    membership_keys: [ThreadId; MEMBERSHIP_SLOTS],
    membership_state: [u8; MEMBERSHIP_SLOTS],
    membership_tracking_exhausted: bool,
    /// FUTEX-WAKE-CAP1 — user slots promised per priority queue and not yet filled. A wake that
    /// must not mutate its waiter until a placement is guaranteed reserves first; every user push
    /// counts these as occupied, so the promise cannot be spent by anyone else. Bounded by
    /// [`MAX_RUN_QUEUE`] per queue, because a reservation is refused once
    /// `len + reserved` reaches it.
    reserved: [u16; PRIORITY_LEVELS],
    /// RT-SCHED1A — every level's reservations together (the total admission's count).
    reserved_total: u16,
    /// RT-SCHED1B — this CPU's scheduling contexts and accounting interval (§24). Inert unless the
    /// domain runs the RT profile with budgets enabled.
    budget: crate::kernel::sched_budget::BudgetLedger,
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            queues: RunQueues::new(),
            current: None,
            membership_keys: [ThreadId(0); MEMBERSHIP_SLOTS],
            membership_state: [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS],
            membership_tracking_exhausted: false,
            reserved: [0; PRIORITY_LEVELS],
            reserved_total: 0,
            budget: crate::kernel::sched_budget::BudgetLedger::default(),
        }
    }
}

impl PriorityScheduler {
    fn membership_hash(tid: ThreadId) -> usize {
        tid.0 as usize & (MEMBERSHIP_SLOTS - 1)
    }

    fn membership_contains(&self, tid: ThreadId) -> bool {
        let mut idx = Self::membership_hash(tid);
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_EMPTY => return false,
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => return true,
                _ => idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1),
            }
        }
        false
    }

    fn membership_insert(&mut self, tid: ThreadId) -> Result<(), ()> {
        let mut idx = Self::membership_hash(tid);
        let mut first_tombstone: Option<usize> = None;
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => return Ok(()),
                MEMBERSHIP_TOMBSTONE => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                MEMBERSHIP_EMPTY => {
                    let insert_idx = first_tombstone.unwrap_or(idx);
                    self.membership_keys[insert_idx] = tid;
                    self.membership_state[insert_idx] = MEMBERSHIP_FULL;
                    return Ok(());
                }
                _ => {}
            }
            idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1);
        }
        if let Some(insert_idx) = first_tombstone {
            self.membership_keys[insert_idx] = tid;
            self.membership_state[insert_idx] = MEMBERSHIP_FULL;
            return Ok(());
        }
        Err(())
    }

    fn membership_remove(&mut self, tid: ThreadId) {
        let mut idx = Self::membership_hash(tid);
        for _ in 0..MEMBERSHIP_SLOTS {
            match self.membership_state[idx] {
                MEMBERSHIP_EMPTY => return,
                MEMBERSHIP_FULL if self.membership_keys[idx] == tid => {
                    self.membership_state[idx] = MEMBERSHIP_TOMBSTONE;
                    return;
                }
                _ => idx = (idx + 1) & (MEMBERSHIP_SLOTS - 1),
            }
        }
    }

    fn linear_contains_tid(&self, tid: ThreadId) -> bool {
        if self.current.is_some_and(|task| task.tid == tid) {
            return true;
        }
        // RT-SCHED1B: a parked (exhausted, Ready) thread is placed here without a queue position.
        if self.budget.get(tid.0).is_some_and(|c| c.parked()) {
            return true;
        }
        self.queues
            .levels()
            .any(|level| self.queues.contains(level, tid))
    }

    fn rebuild_membership_table(&mut self) -> bool {
        self.membership_keys = [ThreadId(0); MEMBERSHIP_SLOTS];
        self.membership_state = [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS];

        let mut exhausted = false;
        if let Some(current) = self.current
            && self.membership_insert(current.tid).is_err()
        {
            exhausted = true;
        }

        for level in self.queues.levels() {
            let mut at = self.queues.head[level];
            while at != NIL {
                let tid = self.queues.tids[at as usize];
                at = self.queues.next[at as usize];
                if self.membership_insert(tid).is_err() {
                    exhausted = true;
                }
            }
        }
        for i in 0..crate::kernel::sched_budget::CTX_PER_CPU {
            if let Some(c) = self.budget.ctx[i]
                && c.parked()
                && self.membership_insert(ThreadId(c.tid)).is_err()
            {
                exhausted = true;
            }
        }

        exhausted
    }

    fn priority_index(priority: TaskPriority) -> usize {
        priority.level()
    }

    fn contains_tid(&self, tid: ThreadId) -> bool {
        if self.membership_tracking_exhausted {
            self.linear_contains_tid(tid)
        } else {
            self.membership_contains(tid)
        }
    }

    /// Stage 199A2D2C2B1: true iff `tid` is present on this CPU either as the
    /// dispatched `current` task or somewhere in a priority run queue. Used to
    /// prove a blocked recv-v2 server is ABSENT from every runqueue before its
    /// authoritative blocked-server acknowledgement is published (no premature
    /// re-selection). Read-only.
    pub fn contains_or_current(&self, tid: ThreadId) -> bool {
        if let Some(current) = self.current {
            if current.tid == tid {
                return true;
            }
        }
        self.contains_tid(tid)
    }

    pub fn enqueue_with_priority(
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
        // RT-SCHED1B: every thread placed on a budgeted CPU is accounted to a context; a wake of an
        // exhausted one keeps it Ready but grants no position and no budget (SC-CTX-8).
        self.budget_bind_default(tid)?;
        if self.budget_exhausted(tid) {
            self.park(tid, priority, false);
            return Ok(());
        }
        self.push_unchecked_capacity(tid, priority)
    }

    /// SCHED-TIME-FOUNDATION1 A (SCHED-FULL-ROT1) — return the OUTGOING current task to the tail
    /// of its own priority queue. Used by every rotation owner (`on_preempt_selection`,
    /// `preempt_reenqueue_only`, `on_preempt_prefer_selection`) and by nothing else.
    ///
    /// It is not an admission. The task already holds a placement on this CPU (it was current a
    /// moment ago), so it is NOT refused for user capacity; that refusal is what kept a full CPU
    /// from ever rotating (FUTEX-TIME1 `LT-x86_64-1`: 47 120 refused yields). Only the duplicate
    /// refusal (`AlreadyQueued`) and the ring's physical bound remain.
    ///
    /// **Why it always fits.** Let `V[p]` = user entries queued at `p` + reservations at `p` + 1
    /// if the current task is a user task at `p`. An admitted push requires
    /// `len[p] + reserved[p] < MAX_RUN_QUEUE`, so after it `V[p] <= MAX_RUN_QUEUE + 1`. A dequeue
    /// into `current`, a rotation (this function) and a reserved fill leave `V[p]` unchanged; a
    /// block or exit lowers it; `restore_exact_current` only undoes a `block_current_exact` and no
    /// admission can succeed in between at `V[p] = MAX_RUN_QUEUE + 1`. Hence at most
    /// `MAX_RUN_QUEUE + 1` user entries plus at most `KTHREAD_CAPACITY` kernel threads, which is
    /// exactly [`MAX_QUEUED_PER_PRIORITY`] — never more, so the push below cannot hit the bound.
    fn requeue_current_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        // RT-SCHED1B: an exhausted outgoing task takes NO position (SC-PRI-12 (f)): it is parked
        // on the slot it was just using, so this cannot fail for capacity either.
        if self.budget_exhausted(tid) {
            self.park(tid, priority, false);
            return Ok(());
        }
        let pushed = self.push_unchecked_capacity(tid, priority);
        // The one current that is installed rather than admitted is an AP's idle (tid 0,
        // `install_ap_idle_current`), so the bound does not cover it; at the physical bound its
        // push is refused and the caller keeps it current, exactly the pre-repair behaviour.
        debug_assert!(
            pushed.is_ok() || tid.0 == 0,
            "SCHED-FULL-ROT1: the returning occupant exceeded MAX_QUEUED_PER_PRIORITY"
        );
        pushed
    }

    /// RT-SCHED1A — [`Self::requeue_current_with_priority`] at the HEAD of its level: SC-PRI-12 (a),
    /// the outgoing current preempted by a strictly higher priority. The same returning occupant
    /// under the same proof, so the same bound: it cannot hit the physical limit either.
    fn requeue_current_at_head(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        // RT-SCHED1B: exhaustion overrides the head placement of (a) until a replenishment.
        if self.budget_exhausted(tid) {
            self.park(tid, priority, false);
            return Ok(());
        }
        let pushed = self.push_front_unchecked_capacity(tid, priority);
        debug_assert!(
            pushed.is_ok() || tid.0 == 0,
            "SCHED-FULL-ROT1: the returning occupant exceeded MAX_QUEUED_PER_PRIORITY"
        );
        pushed
    }

    /// FUTEX-WAKE-CAP1 — whether one more user entry fits at `priority`, counting reservations.
    ///
    /// RT-SCHED1A: the per-level rule is unchanged; the CPU-wide [`MAX_RUN_QUEUE_TOTAL`] rule
    /// beside it never refuses what the per-level rule admits with three levels (proof at the
    /// constant), and is what bounds a 32-level CPU's entries by [`MAX_QUEUED_PER_CPU`].
    fn user_room(&self, priority: TaskPriority) -> bool {
        let p = Self::priority_index(priority);
        self.queues.len(p) + (self.reserved[p] as usize) < MAX_RUN_QUEUE
            && (self.queues.user as usize) + (self.reserved_total as usize) < MAX_RUN_QUEUE_TOTAL
    }

    /// FUTEX-WAKE-CAP1 — promise one user slot at `priority`. Refused (`QueueFull`) exactly when
    /// an ordinary user push would be; nothing is mutated on a refusal.
    pub(crate) fn reserve_user_slot(
        &mut self,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if !self.user_room(priority) {
            return Err(SchedulerError::QueueFull);
        }
        self.reserved[Self::priority_index(priority)] += 1;
        self.reserved_total += 1;
        Ok(())
    }

    /// FUTEX-WAKE-CAP1 — give back a promise that will not be used.
    pub(crate) fn release_user_slot(&mut self, priority: TaskPriority) {
        let p = Self::priority_index(priority);
        debug_assert!(self.reserved[p] > 0, "released a user slot nobody reserved");
        if self.reserved[p] > 0 {
            self.reserved[p] -= 1;
            self.reserved_total -= 1;
        }
    }

    /// FUTEX-WAKE-CAP1 — the user slots currently promised at `priority`.
    #[cfg(test)]
    pub(crate) fn reserved_user_slots(&self, priority: TaskPriority) -> usize {
        self.reserved[Self::priority_index(priority)] as usize
    }

    /// SCHED-SAT-EVIDENCE1 (witness only) — the queue's entries and promised slots at `priority`,
    /// the two numbers [`Self::reserve_user_slot`]'s refusal compares with [`MAX_RUN_QUEUE`].
    #[cfg(any(test, feature = "sched-time-witness", feature = "futex-time-witness"))]
    pub(crate) fn witness_occupancy(&self, priority: TaskPriority) -> (usize, usize) {
        let p = Self::priority_index(priority);
        (self.queues.len(p), self.reserved[p] as usize)
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
        if self.contains_tid(tid) {
            self.release_user_slot(priority);
            return Err(SchedulerError::AlreadyQueued);
        }
        if let Err(e) = self.budget_bind_default(tid) {
            self.release_user_slot(priority);
            return Err(e);
        }
        // RT-SCHED1B: an exhausted waiter keeps the reservation it was woken into as its parked
        // slot — Ready, no position, and its rejoin can never be refused.
        if self.budget_exhausted(tid) {
            self.park(tid, priority, true);
            return Ok(());
        }
        self.release_user_slot(priority);
        self.push_unchecked_capacity(tid, priority)
    }

    /// The push itself, after the caller's capacity rule. The ring's physical bound is still
    /// checked; for a user TID it is the kernel-thread headroom that a reserved fill may use.
    fn push_unchecked_capacity(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.queues
            .push_back(Self::priority_index(priority), tid, MAX_QUEUED_PER_PRIORITY)?;
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        Ok(())
    }

    /// RT-SCHED1A — [`Self::push_unchecked_capacity`] at the HEAD of the level (SC-PRI-12 (a)).
    fn push_front_unchecked_capacity(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.queues
            .push_front(Self::priority_index(priority), tid, MAX_QUEUED_PER_PRIORITY)?;
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        Ok(())
    }

    /// The unfiltered dequeue. KTHREAD1: it never returns a kernel thread — only the
    /// acceptance-filtered selection, under an authority whose caller can run one, does — so a
    /// queued kernel thread is skipped in place here and keeps its FIFO position.
    ///
    /// RT-SCHED1A (§19.3): it never takes a user task from BELOW a level that holds only kernel
    /// threads. Such a level's threads are of strictly higher priority than any user task this
    /// dequeue could return, so it answers `None` and the CPU's next filtered selection — the one
    /// owner that can run a kernel thread — decides; a rotation cannot bypass them.
    fn dequeue_highest(&mut self) -> Option<ScheduledTask> {
        let level = self.queues.levels().next()?;
        let tid = self.queues.first_user(level)?;
        if self.queues.peek(level) == Some(tid) {
            self.queues.pop_front(level);
        } else {
            self.queues.remove_tid(level, tid);
        }
        let priority = TaskPriority::from_level(level)?;
        Some(ScheduledTask { tid, priority })
    }

    /// Non-mutating twin of [`dequeue_highest`]: return the TID that would be
    /// dispatched next from an idle/cleared current (highest-priority queue head),
    /// WITHOUT dequeuing it. Read-only — same priority scan order as `dequeue_highest`.
    fn peek_highest(&self) -> Option<ThreadId> {
        let level = self.queues.levels().next()?;
        self.queues.first_user(level)
    }

    /// RT-SCHED1A — the highest priority any entry of this CPU's queues holds (kernel threads
    /// included: the filtered selection can run them). Read-only, O(1).
    pub fn best_queued_priority(&self) -> Option<TaskPriority> {
        self.queues.best_level().and_then(TaskPriority::from_level)
    }

    /// RT-SCHED1A — SC-PRI-13's predicate: a queued entry has STRICTLY higher priority than this
    /// CPU's current task. An equal-priority entry never qualifies (SC-PRI-11: no time-slicing
    /// among equals), and neither does an empty slot or the tid-0 idle placeholder, whose CPU is
    /// advanced by the idle paths instead. Read-only, O(1).
    pub fn rt_preempt_wanted(&self) -> bool {
        // RT-SCHED1B: an exhausted current is ineligible whatever its priority (SC-CTX-3): the next
        // scheduling point must take the CPU from it even when nothing else is queued.
        if let Some(cur) = self.current
            && cur.tid.0 != 0
            && self.budget_exhausted(cur.tid)
        {
            return true;
        }
        match (self.current, self.best_queued_priority()) {
            (Some(cur), Some(best)) => cur.tid.0 != 0 && best.is_higher_than(cur.priority),
            _ => false,
        }
    }

    /// Provenance-preserving form of [`Self::dispatch_next`]. The `Dequeued` / `ContinuedCurrent`
    /// distinction is decided here, where the queue is actually manipulated.
    pub fn dispatch_next_selection(&mut self) -> DispatchSelection {
        if let Some(current) = self.current {
            if current.tid.0 == 0 && self.runnable_count() > 0 {
                let Some(next) = self.dequeue_highest() else {
                    return DispatchSelection::Idle;
                };
                self.current = Some(next);
                // Remove idle (tid=0) from the membership table so it can be
                // re-enqueued later without hitting the AlreadyQueued guard.
                if !self.membership_tracking_exhausted {
                    self.membership_remove(current.tid);
                }
                return DispatchSelection::Dequeued { tid: next.tid };
            }
            return DispatchSelection::ContinuedCurrent { tid: current.tid };
        }
        // No current task: idle state. Pick the next runnable task if any.
        match self.dequeue_highest() {
            Some(next) => {
                self.current = Some(next);
                DispatchSelection::Dequeued { tid: next.tid }
            }
            None => DispatchSelection::Idle,
        }
    }

    /// U9-DISPATCH-CPU1 §1 — [`Self::dispatch_next_selection`], but it only dequeues a candidate
    /// the caller's `accept` predicate admits.
    ///
    /// # Why this exists, and why it is not a second selection policy
    ///
    /// The order is unchanged: `ContinuedCurrent` first when a non-idle `current` is set, then
    /// High → Normal → Low, FIFO within each. What changes is that a rejected entry is SKIPPED in
    /// place rather than dequeued and rolled back.
    ///
    /// The rollback form could not make bounded progress. `preempt_reenqueue_only` returns a
    /// refused task to the tail of ITS OWN priority queue, so a refused `High` task is picked
    /// again ahead of every `Normal` one: a budget of "N attempts" spends all N rotating within
    /// `High` and never examines `Normal` at all. "At most N attempts" is not "every candidate
    /// tried", which is the claim a drain needs before it may report that nothing is runnable.
    ///
    /// Skipping in place gives that claim directly, and gives it for free: no entry is removed
    /// unless it is the one being dispatched, so nothing is reprioritised, nothing is duplicated,
    /// membership tracking sees exactly one removal, and a rejected task keeps its exact FIFO
    /// position for the next dispatch.
    ///
    /// `accept` is consulted under the caller's rank-1 acquisition and is expected to read the
    /// task table (rank 2) — the canonical ascending direction.
    pub fn dispatch_next_accepted_selection(
        &mut self,
        mut accept: impl FnMut(ThreadId) -> bool,
    ) -> AcceptedSelection {
        // Exactly the `dispatch_next_selection` preamble: a non-idle `current` continues, and an
        // idle `current` with runnable work falls through to the scan.
        let idle_current = match self.current {
            Some(current) if current.tid.0 == 0 && self.runnable_count() > 0 => Some(current),
            Some(current) => {
                return AcceptedSelection::Selected(DispatchSelection::ContinuedCurrent {
                    tid: current.tid,
                });
            }
            None => None,
        };
        let mut examined = 0usize;
        // RT-SCHED1A: every non-empty level, highest priority first, FIFO within each. `accept`
        // cannot reach this scheduler (it is borrowed mutably here), so the walk reads the list in
        // place: the only mutation is the one accepted entry's unlink, after which it returns.
        for level in self.queues.levels() {
            let Some(priority) = TaskPriority::from_level(level) else {
                continue;
            };
            let mut prev = NIL;
            let mut at = self.queues.head[level];
            while at != NIL {
                let tid = self.queues.tids[at as usize];
                let after = self.queues.next[at as usize];
                examined += 1;
                if !accept(tid) {
                    prev = at;
                    at = after;
                    continue;
                }
                self.queues.unlink(level, prev, at);
                // U9-DISPATCH-CPU1 D1 — the selected task's membership is RETAINED, exactly as
                // `dispatch_next_selection` retains it.
                //
                // Membership tracks BOTH queued and current tasks: it is what makes
                // `enqueue_with_priority` answer `AlreadyQueued` for a task that already holds a
                // slot. Removing it here — which an earlier form of this scan did — would have let
                // a task that is CURRENT be enqueued a second time, i.e. duplicate placement of a
                // running task, with the duplicate‑check silently weakened rather than removed.
                // The only membership this step drops is the OUTGOING IDLE's, below.
                if let Some(current) = idle_current
                    && !self.membership_tracking_exhausted
                {
                    // The idle task leaves `current`; drop its membership exactly as
                    // `dispatch_next_selection` does, so it can be re-enqueued later.
                    self.membership_remove(current.tid);
                }
                self.current = Some(ScheduledTask { tid, priority });
                return AcceptedSelection::Selected(DispatchSelection::Dequeued { tid });
            }
        }
        if examined == 0 {
            AcceptedSelection::Empty
        } else {
            AcceptedSelection::NoneAcceptable { examined }
        }
    }

    pub fn dispatch_next(&mut self) -> Option<ThreadId> {
        self.dispatch_next_selection().tid()
    }

    pub fn on_preempt(&mut self) -> Option<ThreadId> {
        self.on_preempt_selection().tid()
    }

    /// Provenance-preserving form of [`Self::on_preempt`]. The re-enqueue-failed branch keeps the
    /// existing `current` and dequeues nothing, so it is `ContinuedCurrent`, not `Dequeued`.
    pub fn on_preempt_selection(&mut self) -> DispatchSelection {
        if let Some(running) = self.current.take() {
            if !self.membership_tracking_exhausted {
                self.membership_remove(running.tid);
            }
            if let Err(err) = self.requeue_current_with_priority(running.tid, running.priority) {
                if err != SchedulerError::AlreadyQueued && self.runnable_count() != 0 {
                    crate::yarm_log!(
                        "scheduler inconsistency: failed to re-enqueue preempted task {:?}; preserving current task",
                        err
                    );
                }
                if !self.membership_tracking_exhausted {
                    let _ = self.membership_insert(running.tid);
                }
                self.current = Some(running);
                return DispatchSelection::ContinuedCurrent { tid: running.tid };
            }
        }
        self.dispatch_next_selection()
    }

    /// Stage 192B: the RE-ENQUEUE half of [`on_preempt`] — re-enqueue the current task at
    /// the tail of its priority queue and CLEAR the current slot, WITHOUT dispatching. The
    /// out-of-lock trap-entry drain then runs the authoritative `dispatch_next` (queue-
    /// advancing) with the global lock dropped.
    ///
    /// Returns `Some(tid)` of the re-enqueued task (current now `None`) on success — the
    /// caller records a deferral and skips the in-lock dispatch. Returns `None` (leaving
    /// `current` UNCHANGED) when there is no current task, or the re-enqueue failed
    /// (e.g. `AlreadyQueued`); the caller then falls back to the legacy in-lock `on_preempt`.
    pub fn preempt_reenqueue_only(&mut self) -> Option<ThreadId> {
        let running = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(running.tid);
        }
        if let Err(_err) = self.requeue_current_with_priority(running.tid, running.priority) {
            // Re-enqueue failed (`AlreadyQueued` only: SCHED-FULL-ROT1 removed the capacity
            // refusal) — restore as current and signal the caller to fall back.
            if !self.membership_tracking_exhausted {
                let _ = self.membership_insert(running.tid);
            }
            self.current = Some(running);
            return None;
        }
        // `current` is now None; the task is re-enqueued exactly once and awaits the
        // out-of-lock queue-advancing dispatch.
        Some(running.tid)
    }

    /// RT-SCHED1A — [`Self::preempt_reenqueue_only`] for a task PREEMPTED by a strictly higher
    /// priority: the outgoing current returns to the HEAD of its own level (SC-PRI-12 (a)), so it
    /// is the next of its level to run once the higher-priority work leaves the CPU. Same
    /// contract otherwise: `Some(tid)` with `current` cleared, or `None` with `current` restored.
    pub fn preempt_reenqueue_head_only(&mut self) -> Option<ThreadId> {
        let running = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(running.tid);
        }
        if self
            .requeue_current_at_head(running.tid, running.priority)
            .is_err()
        {
            if !self.membership_tracking_exhausted {
                let _ = self.membership_insert(running.tid);
            }
            self.current = Some(running);
            return None;
        }
        Some(running.tid)
    }

    /// Like `on_preempt`, but prefers dispatching `preferred` as the next task.
    ///
    /// Re-enqueues the current task at the tail of its priority queue, then:
    /// - If `preferred` is in any queue: removes it and makes it current directly
    ///   (bypassing FIFO order), returning `Some(preferred)`.
    /// - Otherwise: falls back to `dispatch_next()` (FIFO head of highest-priority queue).
    ///
    /// Used by `yield_current_to` to implement one-shot cooperative handoff without
    /// the busy-loop of `switch_to_runnable_tid`.
    pub fn on_preempt_prefer(&mut self, preferred: ThreadId) -> Option<ThreadId> {
        self.on_preempt_prefer_selection(preferred).tid()
    }

    /// Provenance-preserving form of [`Self::on_preempt_prefer`]. Removing `preferred` from a
    /// queue and installing it as `current` IS a dequeue.
    pub fn on_preempt_prefer_selection(&mut self, preferred: ThreadId) -> DispatchSelection {
        // Re-enqueue current (same logic as on_preempt).
        if let Some(running) = self.current.take() {
            if !self.membership_tracking_exhausted {
                self.membership_remove(running.tid);
            }
            if let Err(err) = self.requeue_current_with_priority(running.tid, running.priority) {
                if err != SchedulerError::AlreadyQueued && self.runnable_count() != 0 {
                    crate::yarm_log!(
                        "scheduler inconsistency: failed to re-enqueue preempted task {:?}; preserving current task",
                        err
                    );
                }
                if !self.membership_tracking_exhausted {
                    let _ = self.membership_insert(running.tid);
                }
                self.current = Some(running);
                return DispatchSelection::ContinuedCurrent { tid: running.tid };
            }
        }
        // Scan queues in priority order for the preferred TID. KTHREAD1: a kernel thread is never
        // a preferred handoff target (it is dispatched only by the filtered selection).
        if !crate::kernel::kthread::is_kthread_tid(preferred.0) {
            for level in self.queues.levels() {
                if self.queues.remove_tid(level, preferred) {
                    let Some(priority) = TaskPriority::from_level(level) else {
                        break;
                    };
                    self.current = Some(ScheduledTask {
                        tid: preferred,
                        priority,
                    });
                    return DispatchSelection::Dequeued { tid: preferred };
                }
            }
        }
        // Preferred not in any queue; fall back to normal FIFO dispatch.
        self.dispatch_next_selection()
    }

    /// Stage 199D: remove exactly one queued incarnation of `tid` from THIS CPU's queues.
    ///
    /// Non-dispatching by construction: it never reads or writes `current`, never selects a next
    /// task, and calls no context-switch primitive. It touches only the priority queues and the
    /// membership table that mirrors them.
    ///
    /// The membership update is load-bearing and is why this cannot simply call
    /// `RunQueues::remove_tid`: the one existing caller (`on_preempt_prefer`) moves the task from
    /// a queue to `current`, so the task stays present in this scheduler and membership must NOT
    /// change. Withdrawal removes it from the scheduler entirely, so membership must be cleared
    /// or a later `enqueue_with_priority` would refuse the TID as already queued.
    pub(crate) fn withdraw_queued_tid(&mut self, tid: ThreadId) -> WithdrawOutcome {
        // (3)(4) current-task protection, before any mutation. This also protects the
        // scheduler-owned tid-0 idle current installed on a wake-only CPU.
        if self.current.is_some_and(|task| task.tid == tid) {
            return WithdrawOutcome::RefusedCurrent;
        }
        // RT-SCHED1B: a parked thread is withdrawn by giving back its rejoin slot.
        if self.unpark(tid) {
            if !self.membership_tracking_exhausted {
                self.membership_remove(tid);
            }
            return WithdrawOutcome::Removed;
        }
        // (5) exact-one rule: count first across every priority queue, mutate only if the count
        // is exactly 1. A duplicate fails closed with zero mutation.
        let mut total = 0usize;
        for level in self.queues.levels() {
            total += self.queues.count_tid(level, tid);
        }
        match total {
            0 => return WithdrawOutcome::NotQueued,
            1 => {}
            _ => return WithdrawOutcome::RefusedDuplicate,
        }
        for level in self.queues.levels() {
            if self.queues.remove_tid(level, tid) {
                // The TID has left this scheduler entirely; keep the membership mirror honest.
                if !self.membership_tracking_exhausted {
                    self.membership_remove(tid);
                }
                return WithdrawOutcome::Removed;
            }
        }
        // Unreachable while the count above is authoritative; fail closed rather than claim a
        // removal that did not happen.
        WithdrawOutcome::NotQueued
    }

    pub fn block_current(&mut self) -> Option<ThreadId> {
        let current = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(current.tid);
        }
        Some(current.tid)
    }

    /// U9-EXIT3 §1/§2 — COMPARE-and-clear, returning the removed task's exact priority.
    ///
    /// [`Self::block_current`] clears whatever is current. A caller that means "clear MY task"
    /// therefore clears somebody else's slot whenever the scheduler has moved on, and the task it
    /// removed is then current nowhere and queued nowhere. This form mutates nothing unless the
    /// current slot names exactly `tid`.
    ///
    /// The returned priority is the restoration authority: it is the only record of what the
    /// removed task's placement was, and without it a later restore would have to invent one.
    pub fn block_current_exact(&mut self, tid: ThreadId) -> Option<TaskPriority> {
        if self.current.map(|task| task.tid) != Some(tid) {
            return None;
        }
        let current = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(current.tid);
        }
        Some(current.priority)
    }

    /// U9-EXIT3 §2 — the exact inverse of [`Self::block_current_exact`].
    ///
    /// Restores `tid` as this CPU's current at the priority it was removed with. Refuses — and
    /// mutates nothing — unless the slot is still empty and the task is in no queue on this CPU,
    /// so a restore can never displace a task the scheduler chose in the meantime nor create a
    /// second placement for one that was re-queued.
    pub fn restore_exact_current(&mut self, tid: ThreadId, priority: TaskPriority) -> bool {
        if self.current.is_some() || self.contains_tid(tid) {
            return false;
        }
        self.current = Some(ScheduledTask { tid, priority });
        true
    }

    pub fn current_tid(&self) -> Option<ThreadId> {
        self.current.map(|task| task.tid)
    }

    pub fn current_priority(&self) -> Option<TaskPriority> {
        self.current.map(|task| task.priority)
    }

    pub fn runnable_count(&self) -> usize {
        self.queues.total()
    }

    /// RT-SCHED1A — every queued entry in dispatch order: highest priority first, FIFO within a
    /// level. Read-only.
    fn for_each_queued(&self, f: &mut impl FnMut(TaskPriority, ThreadId)) {
        for level in self.queues.levels() {
            let Some(priority) = TaskPriority::from_level(level) else {
                continue;
            };
            for tid in self.queues.iter(level) {
                f(priority, tid);
            }
        }
    }

    /// KTHREAD1 — user tasks queued or current here: balanced placement's load measure. A kernel
    /// thread is pinned to its home and runs cooperatively, so its passing presence in a queue
    /// does not steer the permanent home balanced placement chooses for a user task. With no
    /// kernel thread present this equals queued + current.
    fn user_load(&self) -> usize {
        let queued = self.queues.user as usize;
        let current = self
            .current
            .is_some_and(|t| !crate::kernel::kthread::is_kthread_tid(t.tid.0));
        queued + usize::from(current) + self.parked_users()
    }

    // ── RT-SCHED1B: scheduling contexts (§24; `kernel::sched_budget`) ───────────────────────────

    /// Parked user threads (each still occupies its home's load).
    fn parked_users(&self) -> usize {
        self.budget
            .ctx
            .iter()
            .flatten()
            .filter(|c| {
                matches!(
                    c.parked,
                    Some((crate::kernel::sched_budget::Park::Reserved, _))
                )
            })
            .count()
    }

    /// Bring this CPU's ledger up to `now`: SC-CTX-7 steps 1–3 — charge the interval since the
    /// previous section to whoever was current, then apply every due release in ascending context
    /// id (rejoining parked contexts left with budget, at their level's tail). Called at the start
    /// of every 1L section of a budgeted CPU and again by the clock owner's tick after it advances
    /// the clock. A no-op (GP, RT without budgets) when `enabled` is false.
    pub(crate) fn budget_sync(&mut self, now: u64, enabled: bool, cpu: u8) {
        if !enabled {
            return;
        }
        if !self.budget.enabled {
            // The first section of a budgeted CPU opens its accounting interval here.
            self.budget.enabled = true;
            self.budget.acct = now;
        }
        self.budget.cpu = cpu;
        if now < self.budget.now {
            return;
        }
        self.budget.now = now;
        self.budget_account(cpu);
        self.budget_replenish(cpu);
    }

    fn budget_account(&mut self, cpu: u8) {
        let now = self.budget.now;
        let elapsed = now.saturating_sub(self.budget.acct);
        self.budget.acct = now;
        if elapsed == 0 {
            return;
        }
        match self.current {
            Some(t) if t.tid.0 != 0 => match self.budget.get_mut(t.tid.0) {
                Some(c) => {
                    let r = crate::kernel::sched_budget::charge(c, elapsed);
                    #[cfg(all(feature = "rt-sched-witness", not(feature = "hosted-dev")))]
                    if r.newly_exhausted || r.overrun > 0 {
                        crate::kernel::rt_witness::note_budget_exhausted(cpu, c, now, r.overrun);
                    }
                    let _ = (r, cpu);
                }
                None => self.budget.unaccounted = self.budget.unaccounted.saturating_add(elapsed),
            },
            _ => self.budget.idle = self.budget.idle.saturating_add(elapsed),
        }
    }

    fn budget_replenish(&mut self, cpu: u8) {
        let now = self.budget.now;
        if now < self.budget.earliest {
            return;
        }
        for i in 0..crate::kernel::sched_budget::CTX_PER_CPU {
            let Some(c) = self.budget.ctx[i].as_mut() else {
                continue;
            };
            let Some(r) = crate::kernel::sched_budget::replenish_due(c, now) else {
                continue;
            };
            let rejoin = if c.exhausted() { None } else { c.parked.take() };
            let tid = ThreadId(c.tid);
            #[cfg(all(feature = "rt-sched-witness", not(feature = "hosted-dev")))]
            crate::kernel::rt_witness::note_budget_replenished(
                cpu,
                c,
                now,
                r.release,
                r.count,
                rejoin.is_some(),
            );
            #[cfg(feature = "futex-seal-witness")]
            crate::kernel::seal_witness::note_replenished(
                now,
                tid.0,
                r.release,
                rejoin.is_some(),
                cpu,
            );
            let _ = (r, cpu);
            if let Some((kind, level)) = rejoin {
                self.rejoin(tid, kind, level);
            }
        }
        self.budget.recompute_earliest();
    }

    /// A parked context left with budget joins its level's TAIL, once (SC-PRI-12 (f), SC-CTX-7).
    /// Its slot was guaranteed when it parked, so this push cannot be refused for capacity.
    fn rejoin(&mut self, tid: ThreadId, kind: crate::kernel::sched_budget::Park, level: u8) {
        let Some(priority) = TaskPriority::from_level(level as usize) else {
            return;
        };
        if kind == crate::kernel::sched_budget::Park::Reserved {
            self.release_user_slot(priority);
        }
        let pushed = self.push_unchecked_capacity(tid, priority);
        debug_assert!(pushed.is_ok(), "RT-SCHED1B: a parked rejoin found no slot");
    }

    /// Is `tid` bound to an exhausted context on this budgeted CPU?
    fn budget_exhausted(&self, tid: ThreadId) -> bool {
        self.budget.enabled && self.budget.get(tid.0).is_some_and(|c| c.exhausted())
    }

    /// Bind `tid` to the default context if this budgeted CPU has none for it yet: every thread
    /// placed here is accounted (the deployment configuration binds explicit budgets first, at the
    /// incarnation's first placement). A full pool refuses the placement.
    fn budget_bind_default(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        if !self.budget.enabled || tid.0 == 0 || self.budget.slot_of(tid.0).is_some() {
            return Ok(());
        }
        self.budget
            .bind(tid.0, 0, crate::kernel::sched_budget::BudgetParams::DEFAULT)
            .map(|_| ())
            .ok_or(SchedulerError::ContextPoolFull)
    }

    /// Park exhausted, Ready `tid` at `priority`: no queue position. A user thread holds one
    /// reservation at its level (`already_reserved`: the one it was woken into); a kernel thread
    /// uses the kernel-thread headroom. Its membership stays, so nothing places it twice.
    fn park(&mut self, tid: ThreadId, priority: TaskPriority, already_reserved: bool) {
        use crate::kernel::sched_budget::Park;
        let kind = if crate::kernel::kthread::is_kthread_tid(tid.0) {
            // Kernel threads are placed without admission; a reservation one was filled from
            // (none today) is given back rather than held twice.
            if already_reserved {
                self.release_user_slot(priority);
            }
            Park::KernelThread
        } else {
            if !already_reserved {
                let p = Self::priority_index(priority);
                self.reserved[p] += 1;
                self.reserved_total += 1;
            }
            Park::Reserved
        };
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        let now = self.budget.now;
        if let Some(c) = self.budget.get_mut(tid.0) {
            c.parked = Some((kind, priority.level() as u8));
            #[cfg(all(feature = "rt-sched-witness", not(feature = "hosted-dev")))]
            crate::kernel::rt_witness::note_budget_parked(now, c);
            #[cfg(feature = "futex-seal-witness")]
            crate::kernel::seal_witness::note_parked(now, c.tid, c.next_release(), self.budget.cpu);
        }
        let _ = now;
    }

    /// Give back a parked thread's rejoin slot. `false`: it was not parked here.
    fn unpark(&mut self, tid: ThreadId) -> bool {
        let Some(c) = self.budget.get_mut(tid.0) else {
            return false;
        };
        let Some((kind, level)) = c.parked.take() else {
            return false;
        };
        if kind == crate::kernel::sched_budget::Park::Reserved
            && let Some(priority) = TaskPriority::from_level(level as usize)
        {
            self.release_user_slot(priority);
        }
        true
    }

    /// RT-SCHED1B — bind `tid`'s incarnation `stamp` to a context with the configuration's
    /// `params`, at its first placement on this (home) CPU. A binding of an earlier incarnation of
    /// the TID is replaced (nothing is inherited); a full pool is a typed refusal.
    pub(crate) fn bind_context(
        &mut self,
        tid: ThreadId,
        stamp: u64,
        params: crate::kernel::sched_budget::BudgetParams,
    ) -> Result<(), SchedulerError> {
        if !self.budget.enabled {
            return Ok(());
        }
        let slot = self
            .budget
            .bind(tid.0, stamp, params)
            .ok_or(SchedulerError::ContextPoolFull)?;
        #[cfg(all(feature = "rt-sched-witness", not(feature = "hosted-dev")))]
        if let Some(c) = self.budget.ctx[slot].as_ref() {
            crate::kernel::rt_witness::note_budget_bound(self.budget.now, slot, c);
        }
        let _ = slot;
        Ok(())
    }

    /// RT-SCHED1B — release `tid`'s context when its incarnation `stamp` (`0`: any) is retired
    /// (reap, kernel-thread reclaim). A parked thread gives back its slot first. `false`: none.
    pub(crate) fn release_context(&mut self, tid: ThreadId, stamp: u64) -> bool {
        let Some(i) = self.budget.slot_of(tid.0) else {
            return false;
        };
        let matches =
            self.budget.ctx[i].is_some_and(|c| stamp == 0 || c.stamp == 0 || c.stamp == stamp);
        if !matches {
            return false;
        }
        if self.unpark(tid) && !self.membership_tracking_exhausted {
            self.membership_remove(tid);
        }
        self.budget.ctx[i] = None;
        self.budget.recompute_earliest();
        true
    }

    /// RT-SCHED1B — `tid`'s context on this CPU (observation).
    pub(crate) fn context_of(
        &self,
        tid: ThreadId,
    ) -> Option<crate::kernel::sched_budget::SchedContext> {
        self.budget.get(tid.0).copied()
    }

    /// RT-SCHED1B — this CPU's ledger: (idle, unaccounted, essential ticks, essential arrivals,
    /// contexts bound).
    pub(crate) fn budget_ledger(&self) -> (u64, u64, u64, u64, usize) {
        (
            self.budget.idle,
            self.budget.unaccounted,
            self.budget.essential_ticks,
            self.budget.essential_arrivals,
            self.budget.ctx.iter().flatten().count(),
        )
    }

    fn note_essential(&mut self, tick: bool) {
        if self.budget.enabled {
            if tick {
                self.budget.essential_ticks += 1;
            } else {
                self.budget.essential_arrivals += 1;
            }
        }
    }
}

/// SCHED-LOCK1 — the clock owner: the one CPU whose timer interrupts advance the scheduler tick.
pub(crate) const CLOCK_OWNER_CPU: CpuId = CpuId(platform_constants::BOOTSTRAP_CPU_ID);

/// SCHED-LOCK1 — ONE CPU's scheduling state: its run queues, its current slot (both in `rq`) and
/// its quantum. Owned by that CPU's own lock (rank 1L, `doc/SCHEDULING_CONTRACT.md` §16.2) and
/// reachable only through a [`CpuView`] built from that lock's guard or from a [`SchedAll`] that
/// holds every 1L.
#[derive(Debug)]
pub struct CpuSched {
    rq: PriorityScheduler,
    quantum: Timer,
    /// SCHED-LOCK-FAIR1: scheduler-only diagnostics raised inside a 1L section, emitted by the
    /// releasing guard AFTER 1L is released (synchronous printk is not done under 1L).
    deferred: DeferredDiags,
}

/// SCHED-LOCK-FAIR1 — a scheduler-only diagnostic whose facts are captured under 1L and whose
/// line is printed after the release, in the order raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredDiag {
    /// An explicit placement refused on a wake-only CPU (Stage 183.5 / 195D).
    WakeOnlyDenied { cpu: CpuId, tid: ThreadId },
}

/// At most `DEFERRED_MAX` diagnostics per section are kept; any more are counted and reported
/// as one `SCHED_DEFERRED_DIAG_DROPPED` line, so nothing is silently lost.
const DEFERRED_MAX: usize = 4;

#[derive(Debug, Clone, Copy, Default)]
struct DeferredDiags {
    diags: [Option<DeferredDiag>; DEFERRED_MAX],
    len: usize,
    dropped: u32,
}

impl DeferredDiags {
    fn push(&mut self, diag: DeferredDiag) {
        if self.len < DEFERRED_MAX {
            self.diags[self.len] = Some(diag);
            self.len += 1;
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Hand the pending diagnostics out of the locked state (still under 1L).
    fn take(&mut self) -> Self {
        core::mem::take(self)
    }

    /// Print them — called with NO scheduler lock held.
    fn emit(self) {
        for diag in self.diags.iter().take(self.len).flatten() {
            match *diag {
                DeferredDiag::WakeOnlyDenied { cpu, tid } => {
                    crate::yarm_log!(
                        "SCHED_ENQUEUE_DENIED_WAKE_ONLY cpu={} tid={} reason=no_ap_dispatcher_yet",
                        cpu.0,
                        tid.0
                    );
                    // Stage 195D (BSP DISPATCH AFFINITY): an actual prevented placement of a
                    // runnable user task onto a wake-only AP queue. Emitted ONLY on a real
                    // rejection (never fabricated); the caller reroutes to the BSP dispatcher.
                    #[cfg(target_arch = "aarch64")]
                    crate::yarm_log!(
                        "AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED tid={} cpu={}",
                        tid.0,
                        cpu.0
                    );
                }
            }
        }
        if self.dropped != 0 {
            crate::yarm_log!("SCHED_DEFERRED_DIAG_DROPPED count={}", self.dropped);
        }
    }
}

impl CpuSched {
    fn new(quantum_ticks: u64) -> Self {
        Self {
            rq: PriorityScheduler::default(),
            quantum: Timer::new(quantum_ticks),
            deferred: DeferredDiags::default(),
        }
    }

    /// Balanced placement's load: the user tasks queued or current here (KTHREAD1).
    fn load(&self) -> usize {
        self.rq.user_load()
    }

    /// SCHED-LOCK1 witness only: the current slot, read-only.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_current(&self) -> Option<ThreadId> {
        self.rq.current_tid()
    }

    /// SCHED-LOCK1 witness only: every queued entry, read-only, in dispatch order.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_for_each_queued(&self, mut f: impl FnMut(TaskPriority, ThreadId)) {
        self.rq.for_each_queued(&mut f);
    }

    /// Where `tid` is on THIS CPU: (current here, queued here). `current` is checked separately
    /// from the queues because the current slot is the authority on what is executing.
    fn placement_here(&self, tid: ThreadId) -> (bool, bool) {
        (
            self.rq.current.is_some_and(|task| task.tid == tid),
            self.rq
                .queues
                .levels()
                .any(|level| self.rq.queues.count_tid(level, tid) > 0)
                // RT-SCHED1B: a parked thread is Ready and placed here (its rejoin slot is held),
                // without a queue position: every placement reader sees it as placed, not absent.
                || self.rq.budget.get(tid.0).is_some_and(|c| c.parked()),
        )
    }
}

/// SCHED-LOCK1 — the published, lock-free summary of one CPU's state: its current task (tid + 1,
/// 0 = none) and its load (queued + current). Written ONLY by the holder of that CPU's 1L, when
/// the guard is released; read by anyone (SeqCst). See §16.4 for what a reader may conclude.
#[derive(Debug, Default)]
struct CpuMirror {
    current: AtomicU64,
    load: AtomicU64,
}

/// One CPU's cell: its lock-owned state and its mirror. Cache-line aligned so one CPU's lock and
/// mirror traffic does not share a line with its neighbour's.
#[derive(Debug)]
#[repr(align(64))]
pub struct CpuCell {
    /// Rank 1L. SCHED-LOCK-FAIR1: a FIFO ticket lock (`kernel::ticket_lock`), not the TTAS
    /// `SpinLockIrq`.
    lock: TicketLockIrq<CpuSched>,
    mirror: CpuMirror,
}

impl CpuCell {
    fn new(quantum_ticks: u64, _idx: usize) -> Self {
        Self {
            #[cfg(not(feature = "sched-lock1-witness"))]
            lock: TicketLockIrq::new(CpuSched::new(quantum_ticks)),
            #[cfg(feature = "sched-lock1-witness")]
            lock: TicketLockIrq::new_fair(CpuSched::new(quantum_ticks), _idx as u8),
            mirror: CpuMirror::default(),
        }
    }
}

/// Publish `sched` into `mirror`. Called only by a holder of that CPU's 1L (or of the exclusive
/// domain), after its last mutation and before the release.
fn publish(mirror: &CpuMirror, sched: &CpuSched) {
    let current = sched.rq.current.map_or(0, |t| t.tid.0.wrapping_add(1));
    mirror.current.store(current, Ordering::SeqCst);
    mirror.load.store(sched.load() as u64, Ordering::SeqCst);
}

/// RT-SCHED1A — the scheduling profile of the ONE active scheduling domain
/// (`doc/SCHEDULING_CONTRACT.md` §23). Both profiles select through the same owners — the one
/// selection owner, the one enqueue planner, the one transition owner and the one mark token
/// (SC-GP-2) — over the same 32-level queues; the profile changes only the three points where
/// RT-P1 differs from GP:
///
/// * a quantum tick does not rotate: it preempts only for a strictly higher-priority entry
///   (SC-PRI-11, no time-slicing among equals);
/// * a task preempted by a strictly higher priority returns to the HEAD of its level
///   (SC-PRI-12 (a)); GP returns it to the tail;
/// * a wake is a scheduling point: the woken CPU is told, and the arrival on a running CPU
///   re-checks priority and switches when it must (SC-PRI-13); GP returns to the interrupted task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SchedProfile {
    /// The default: three levels by class, quantum round-robin within a level.
    GeneralPurpose = 0,
    /// RT-P1 fixed-priority preemptive selection (no budgets, windows or kernel preemption yet).
    RealTime = 1,
}

impl SchedProfile {
    pub fn marker(self) -> &'static str {
        match self {
            Self::GeneralPurpose => "gp",
            Self::RealTime => "rt",
        }
    }
}

/// RT-SCHED1A — the ONE active scheduling domain's profile, mirrored for the notification owner
/// (`hal::notify_enqueued`), which runs after every 1L is released and holds no domain reference.
/// Written only by [`SchedDomain::set_profile`].
static RT_PROFILE_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// RT-SCHED1A — is the active domain's profile RT-P1? (See [`RT_PROFILE_ACTIVE`].)
pub(crate) fn rt_profile_active() -> bool {
    RT_PROFILE_ACTIVE.load(Ordering::Acquire)
}

/// SCHED-LOCK1 — the published admission bitmaps. Written only under 1G **and** the affected
/// CPU's 1L (so a holder of 1L[c] sees `c`'s bits stable); read lock-free with Acquire.
#[derive(Debug, Default)]
pub(crate) struct SchedTopo {
    present: AtomicU64,
    online: AtomicU64,
    wake_only: AtomicU64,
    balance_excluded: AtomicU64,
    /// RT-SCHED1A — the domain's scheduling profile ([`SchedProfile`]), set once by the boot
    /// configuration before any CPU dispatches. `0` (the default) is general-purpose.
    profile: AtomicU8,
    /// RT-SCHED1B — budget enforcement (`yarm.sched_budget=on`), honoured only with the RT profile.
    budget: core::sync::atomic::AtomicBool,
}

impl SchedTopo {
    /// RT-SCHED1A — is this domain configured with the RT-P1 profile?
    pub(crate) fn rt(&self) -> bool {
        self.profile.load(Ordering::Acquire) == SchedProfile::RealTime as u8
    }
    /// RT-SCHED1B — are budgets enforced? Only ever under the RT profile.
    pub(crate) fn budget(&self) -> bool {
        self.rt() && self.budget.load(Ordering::Acquire)
    }
    pub(crate) fn present(&self) -> u64 {
        self.present.load(Ordering::Acquire)
    }
    pub(crate) fn online(&self) -> u64 {
        self.online.load(Ordering::Acquire)
    }
    pub(crate) fn wake_only(&self) -> u64 {
        self.wake_only.load(Ordering::Acquire)
    }
    pub(crate) fn balance_excluded(&self) -> u64 {
        self.balance_excluded.load(Ordering::Acquire)
    }
    /// The DISPATCHING CPUs: online and not wake-only — THE masked expression every dispatching
    /// count uses. `online` is loaded first: a CPU is marked wake-only before it is onlined, so an
    /// online bit observed here carries (Acquire/Release) its wake-only bit with it, and the mask
    /// can never count a CPU that is still wake-only.
    pub(crate) fn dispatching(&self) -> u64 {
        let online = self.online();
        online & !self.wake_only()
    }

    fn cpu_online(&self, idx: usize) -> bool {
        idx < MAX_CPUS && self.online() & (1u64 << idx) != 0
    }
    fn set_bit(word: &AtomicU64, idx: usize, on: bool) {
        if on {
            word.fetch_or(1u64 << idx, Ordering::AcqRel);
        } else {
            word.fetch_and(!(1u64 << idx), Ordering::AcqRel);
        }
    }
    fn publish_topology(&self, topology: &CpuTopology) {
        self.present
            .store(topology.present_cpu_bitmap(), Ordering::Release);
        self.online
            .store(topology.online_cpu_bitmap(), Ordering::Release);
    }

    /// The least-loaded online CPU that is neither wake-only nor balance-excluded, scanning from
    /// `start` and keeping the first minimum — `SmpScheduler::least_loaded_online_cpu`'s policy,
    /// over whichever load source the caller holds the authority to read.
    fn least_loaded(
        &self,
        start: usize,
        load: impl Fn(usize) -> usize,
    ) -> Result<CpuId, SchedulerError> {
        let excluded = self.wake_only() | self.balance_excluded();
        let online = self.online();
        let mut best: Option<(usize, CpuId)> = None;
        for offset in 0..MAX_CPUS {
            let idx = (start + offset) % MAX_CPUS;
            // Stage 183.5: wake-only CPUs accept no placement; QEMU-SMP2: nor a pinned-only one.
            if excluded & (1u64 << idx) != 0 || online & (1u64 << idx) == 0 {
                continue;
            }
            let l = load(idx);
            if best.map_or(true, |(best_load, _)| l < best_load) {
                best = Some((l, CpuId(idx as u8)));
            }
        }
        best.map(|(_, cpu)| cpu).ok_or(SchedulerError::CpuOffline)
    }
}

/// SCHED-LOCK1 — genuinely global placement state: topology and the balanced-placement rotation.
/// Behind rank 1G; never taken on an ordinary dispatch path.
#[derive(Debug)]
pub(crate) struct Admission {
    topology: CpuTopology,
    next_balance_cpu: usize,
}

/// SCHED-LOCK1 — the scheduler domain. There is NO lock around the whole: each CPU's state has
/// its own lock (rank 1L), admission has its own (rank 1G), and the clock, the admission bitmaps
/// and the per-CPU mirrors are atomics. `doc/SCHEDULING_CONTRACT.md` §16.
#[derive(Debug)]
pub struct SchedDomain {
    cpus: SchedCells,
    /// Rank 1G. SCHED-LOCK-FAIR1: a FIFO ticket lock, like every 1L.
    admission: TicketLockIrq<Admission>,
    topo: SchedTopo,
    /// The scheduler tick. Single writer: [`CpuView::tick`] on [`CLOCK_OWNER_CPU`], under its 1L.
    clock: AtomicU64,
}

#[cfg(feature = "hosted-dev")]
type SchedCells = alloc::boxed::Box<[CpuCell; MAX_CPUS]>;
#[cfg(not(feature = "hosted-dev"))]
type SchedCells = [CpuCell; MAX_CPUS];

impl SchedDomain {
    /// A domain with `present` CPUs present, only the boot CPU online, every quantum
    /// `quantum_ticks`, and the clock at 0.
    /// RT-SCHED1A — configure this domain's scheduling profile. A boot/deployment decision,
    /// made before any CPU dispatches: it is read by every CPU's tick, preemption and wake
    /// decisions from then on.
    pub fn set_profile(&self, profile: SchedProfile) {
        self.topo.profile.store(profile as u8, Ordering::Release);
        RT_PROFILE_ACTIVE.store(profile == SchedProfile::RealTime, Ordering::Release);
        #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
        crate::arch::x86_64::smp::set_rt_user_resched_handoff(profile == SchedProfile::RealTime);
    }

    /// RT-SCHED1B — enable budget enforcement. Fails closed: refused (`false`, nothing changed)
    /// unless the domain runs the RT profile. A boot decision, made before any CPU dispatches.
    pub fn set_budget(&self, on: bool) -> bool {
        if on && !self.topo.rt() {
            return false;
        }
        self.topo.budget.store(on, Ordering::Release);
        true
    }

    /// RT-SCHED1B — are budgets enforced in this domain?
    pub fn budget_enabled(&self) -> bool {
        self.topo.budget()
    }

    /// RT-SCHED1A — this domain's scheduling profile.
    pub fn profile(&self) -> SchedProfile {
        if self.topo.rt() {
            SchedProfile::RealTime
        } else {
            SchedProfile::GeneralPurpose
        }
    }

    pub fn new(present: u64, quantum_ticks: u64) -> Self {
        let cells: [CpuCell; MAX_CPUS] =
            core::array::from_fn(|idx| CpuCell::new(quantum_ticks, idx));
        #[cfg(feature = "hosted-dev")]
        let cells = alloc::boxed::Box::new(cells);
        let topology = CpuTopology::from_present_bitmap(present);
        let topo = SchedTopo::default();
        topo.publish_topology(&topology);
        Self {
            cpus: cells,
            #[cfg(not(feature = "sched-lock1-witness"))]
            admission: TicketLockIrq::new(Admission {
                topology,
                next_balance_cpu: 0,
            }),
            #[cfg(feature = "sched-lock1-witness")]
            admission: TicketLockIrq::new_fair(
                Admission {
                    topology,
                    next_balance_cpu: 0,
                },
                crate::kernel::ticket_lock::FAIR_ID_1G,
            ),
            topo,
            clock: AtomicU64::new(0),
        }
    }

    /// The published admission bitmaps (lock-free).
    pub(crate) fn topo(&self) -> &SchedTopo {
        &self.topo
    }

    /// The scheduler tick (lock-free; Acquire).
    pub fn clock_now(&self) -> Tick {
        TickInstant(self.clock.load(Ordering::Acquire))
    }

    /// SCHED-LOCK-FAIR1 witness only: tickets outstanding (holder + waiters) on CPU `idx`'s 1L.
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub(crate) fn witness_cpu_outstanding(&self, idx: usize) -> u32 {
        self.cpus[idx].lock.outstanding()
    }

    /// SCHED-LOCK-FAIR1 witness only: tickets outstanding on 1G.
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub(crate) fn witness_admission_outstanding(&self) -> u32 {
        self.admission.outstanding()
    }

    /// SCHED-LOCK-FAIR1 witness only: 1G alone, for the fair-admission probe (no 1L is taken).
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub(crate) fn witness_lock_admission(&self) -> TicketLockIrqGuard<'_, Admission> {
        self.admission.lock()
    }

    /// SCHED-LOCK1 witness only: CPU `idx`'s cell and its lock, as addresses — the authoritative
    /// CPU → lock-instance map the witness records under the coherent view.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_cell_addrs(&self, idx: usize) -> (usize, usize) {
        let cell = &self.cpus[idx];
        (
            cell as *const CpuCell as usize,
            &cell.lock as *const TicketLockIrq<CpuSched> as usize,
        )
    }

    /// CPU `idx`'s published current task (see [`CpuMirror`]). `None` for an out-of-range CPU.
    pub(crate) fn current_mirror(&self, idx: usize) -> Option<ThreadId> {
        let raw = self.cpus.get(idx)?.mirror.current.load(Ordering::SeqCst);
        (raw != 0).then(|| ThreadId(raw - 1))
    }

    /// SCHED-LOCK1 §16.4 — does any ONLINE CPU other than `cpu` publish `tid` as its current?
    /// Lock-free (SeqCst mirror loads). The caller holds `cpu`'s own 1L.
    pub(crate) fn current_elsewhere(&self, cpu: CpuId, tid: ThreadId) -> bool {
        let online = self.topo.online();
        (0..MAX_CPUS).any(|idx| {
            idx != cpu.0 as usize
                && online & (1u64 << idx) != 0
                && self.current_mirror(idx) == Some(tid)
        })
    }

    /// CPU `idx`'s published load (user tasks queued + current; KTHREAD1 excludes kernel threads).
    pub(crate) fn load_mirror(&self, idx: usize) -> usize {
        self.cpus
            .get(idx)
            .map_or(0, |c| c.mirror.load.load(Ordering::SeqCst) as usize)
    }

    /// Rank 1L — lock exactly CPU `cpu`'s state. `InvalidCpu` (nothing locked) when out of range.
    pub fn lock_cpu(&self, cpu: CpuId) -> Result<CpuSchedGuard<'_>, SchedulerError> {
        let idx = cpu.0 as usize;
        let cell = self.cpus.get(idx).ok_or(SchedulerError::InvalidCpu)?;
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::before_acquire(idx);
        let guard = cell.lock.lock();
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::after_acquire(idx, &guard);
        Ok(CpuSchedGuard {
            guard: Some(guard),
            cell,
            topo: &self.topo,
            clock: &self.clock,
            idx,
        })
    }

    /// Rank 1G then rank 1L — an admission transition of CPU `cpu`.
    pub(crate) fn lock_cpu_admission(
        &self,
        cpu: CpuId,
    ) -> Result<CpuAdmissionGuard<'_>, SchedulerError> {
        let idx = cpu.0 as usize;
        if idx >= MAX_CPUS {
            return Err(SchedulerError::InvalidCpu);
        }
        let admission = self.admission.lock();
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::admission_acquired();
        let cpu = self.lock_cpu(cpu)?;
        Ok(CpuAdmissionGuard { cpu, admission })
    }

    /// Rank 1G alone — the balanced choice over the load mirrors, rotating the next starting
    /// point exactly as the base did. Any CPU returned is admitted for balanced placement; the
    /// enqueue onto it re-checks admission under that CPU's own 1L.
    pub(crate) fn balanced_choice_and_note(&self) -> Result<CpuId, SchedulerError> {
        let mut admission = self.admission.lock();
        let cpu = self
            .topo
            .least_loaded(admission.next_balance_cpu, |idx| self.load_mirror(idx))?;
        admission.next_balance_cpu = (cpu.0 as usize + 1) % MAX_CPUS;
        Ok(cpu)
    }

    /// SCHED-LOCK1 §16.4 — the per-CPU scan for a FROZEN subject: each CPU examined under its own
    /// 1L, one at a time, never two held. Equal to the coherent answer whenever `tid`'s placement
    /// cannot change during the scan; the caller states why it cannot.
    ///
    /// SCHED-LOCK2B: reached only from the x86_64 SMP oracle's armed fact collection and hosted
    /// tests; no functional IPC or reap path calls it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn present_anywhere_scan(&self, tid: ThreadId) -> bool {
        (0..MAX_CPUS).any(|idx| {
            let cell = &self.cpus[idx];
            let guard = cell.lock.lock();
            guard.rq.contains_or_current(tid)
        })
    }

    /// Rank 1G, then every 1L in ascending order — the ONE coherent whole-domain view. Rare paths
    /// only (§16.1). Released in descending order, 1G last.
    pub fn lock_all(&self) -> SchedAllGuard<'_> {
        let admission = self.admission.lock();
        let mut guards: [Option<TicketLockIrqGuard<'_, CpuSched>>; MAX_CPUS] =
            [const { None }; MAX_CPUS];
        for (idx, slot) in guards.iter_mut().enumerate() {
            *slot = Some(self.cpus[idx].lock.lock());
        }
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::all_acquired();
        SchedAllGuard {
            domain: self,
            guards,
            admission: Some(admission),
        }
    }

    /// Exclusive access (no other reference to the domain exists): the whole view without
    /// locking. Construction and hosted unit tests only. Mirrors are republished when the view is
    /// dropped.
    pub fn exclusive(&mut self) -> SchedAll<'_> {
        let mut cpus: [Option<&mut CpuSched>; MAX_CPUS] = [const { None }; MAX_CPUS];
        let mut mirrors: [Option<&CpuMirror>; MAX_CPUS] = [None; MAX_CPUS];
        let now = self.clock.load(Ordering::Acquire);
        let budget = self.topo.budget();
        for (idx, cell) in self.cpus.iter_mut().enumerate() {
            let CpuCell { lock, mirror } = cell;
            let sched = lock.get_mut();
            // RT-SCHED1B: the whole-domain view brings every CPU's ledger up to date first.
            sched.rq.budget_sync(now, budget, idx as u8);
            cpus[idx] = Some(sched);
            mirrors[idx] = Some(&*mirror);
        }
        SchedAll {
            cpus: cpus.map(|c| c.expect("every cell")),
            mirrors: mirrors.map(|m| m.expect("every cell")),
            admission: self.admission.get_mut(),
            topo: &self.topo,
            clock: &self.clock,
            // No lock is held: the view's own drop prints what its sections deferred.
            emit_deferred: true,
        }
    }
}

/// SCHED-LOCK1 — a held 1L on one CPU. Projects ONLY that CPU's [`CpuSched`]; republishes the
/// CPU's mirror when released (before the lock itself is released).
pub struct CpuSchedGuard<'a> {
    /// `Some` until the drop releases it — so the drop can publish the mirror while 1L is held,
    /// release, and only then emit the section's deferred diagnostics.
    guard: Option<TicketLockIrqGuard<'a, CpuSched>>,
    cell: &'a CpuCell,
    topo: &'a SchedTopo,
    clock: &'a AtomicU64,
    idx: usize,
}

impl CpuSchedGuard<'_> {
    /// SCHED-LOCK-FAIR1 witness and tests: the ticket this 1L guard holds.
    #[cfg(any(test, feature = "sched-lock1-witness"))]
    pub(crate) fn witness_ticket(&self) -> u32 {
        self.guard.as_ref().map_or(0, |g| g.ticket())
    }

    pub fn view(&mut self) -> CpuView<'_> {
        let sched = self
            .guard
            .as_mut()
            .expect("the 1L guard is held until the drop");
        // RT-SCHED1B: every section of a budgeted CPU starts with its ledger up to date.
        sched.rq.budget_sync(
            self.clock.load(Ordering::Acquire),
            self.topo.budget(),
            self.idx as u8,
        );
        CpuView {
            idx: self.idx,
            sched,
            topo: self.topo,
            clock: self.clock,
            #[cfg(feature = "sched-lock1-witness")]
            cell: Some(self.cell),
        }
    }
}

impl Drop for CpuSchedGuard<'_> {
    fn drop(&mut self) {
        let Some(mut guard) = self.guard.take() else {
            return;
        };
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::before_release(
            self.idx,
            self.cell as *const CpuCell as usize,
            &guard,
        );
        // The mirror is published while 1L is still held, then 1L is released, and only then
        // are the section's scheduler-only diagnostics printed (SCHED-LOCK-FAIR1).
        publish(&self.cell.mirror, &guard);
        let deferred = guard.deferred.take();
        drop(guard);
        deferred.emit();
    }
}

/// SCHED-LOCK1 — 1G and one CPU's 1L, for that CPU's admission transition.
pub(crate) struct CpuAdmissionGuard<'a> {
    // Field order is drop order: the 1L guard is released before 1G.
    cpu: CpuSchedGuard<'a>,
    admission: TicketLockIrqGuard<'a, Admission>,
}

#[cfg(feature = "sched-lock1-witness")]
impl Drop for CpuAdmissionGuard<'_> {
    fn drop(&mut self) {
        crate::kernel::schedlock1_witness::admission_released();
    }
}

impl CpuAdmissionGuard<'_> {
    pub(crate) fn admit_pinned_dispatch(&mut self) -> Result<(), SchedulerError> {
        let _ = &mut self.admission;
        let mut view = self.cpu.view();
        admit(&mut view, true)
    }

    pub(crate) fn admit_dispatch(&mut self) -> Result<(), SchedulerError> {
        let _ = &mut self.admission;
        let mut view = self.cpu.view();
        admit(&mut view, false)
    }
}

/// The wake-only → dispatching transition (`admit_pinned_dispatch` / `admit_dispatch`): ONE
/// mutation under 1G + the CPU's 1L, refused (changing nothing) unless the CPU is online,
/// wake-only, running nothing but the idle placeholder and holding no queued work.
fn admit(view: &mut CpuView<'_>, pinned_only: bool) -> Result<(), SchedulerError> {
    if !view.online() {
        return Err(SchedulerError::CpuOffline);
    }
    let bit = 1u64 << view.idx;
    if view.topo.wake_only() & bit == 0 {
        return Err(SchedulerError::CpuOffline);
    }
    match view.sched.rq.current_tid() {
        None | Some(ThreadId(0)) => {}
        Some(_) => return Err(SchedulerError::AlreadyQueued),
    }
    if view.sched.rq.runnable_count() != 0 {
        return Err(SchedulerError::AlreadyQueued);
    }
    view.sched.rq.current = None;
    SchedTopo::set_bit(&view.topo.balance_excluded, view.idx, pinned_only);
    SchedTopo::set_bit(&view.topo.wake_only, view.idx, false);
    Ok(())
}

/// SCHED-LOCK1 — one CPU's state under an authority that owns it (a [`CpuSchedGuard`] or a
/// [`SchedAll`]). Every method is the per-CPU half of the base `SmpScheduler::*_on`, with the
/// same refusal for a CPU that is not online.
pub struct CpuView<'a> {
    idx: usize,
    sched: &'a mut CpuSched,
    topo: &'a SchedTopo,
    clock: &'a AtomicU64,
    /// SCHED-LOCK1 witness only: the cell whose 1L this view holds (`None` under `SchedAll`).
    #[cfg(feature = "sched-lock1-witness")]
    cell: Option<&'a CpuCell>,
}

impl CpuView<'_> {
    pub fn cpu(&self) -> CpuId {
        CpuId(self.idx as u8)
    }

    fn online(&self) -> bool {
        self.topo.cpu_online(self.idx)
    }

    pub fn validate_online(&self) -> Result<(), SchedulerError> {
        if self.online() {
            Ok(())
        } else {
            Err(SchedulerError::CpuOffline)
        }
    }

    pub fn wake_only(&self) -> bool {
        self.topo.wake_only() & (1u64 << self.idx) != 0
    }

    pub fn enqueue_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.validate_online()?;
        let cpu = self.cpu();
        // Stage 183.5: a wake-only online CPU runs no dispatcher — a task placed on
        // its queue would strand forever. Deny placement explicitly (nothing pins
        // work to APs today; 183.6 lifts this per CPU when the AP dispatcher lands).
        if self.wake_only() {
            // `SCHED_ENQUEUE_DENIED_WAKE_ONLY … reason=no_ap_dispatcher_yet` (and, on AArch64,
            // `AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED`): SCHED-LOCK-FAIR1 captures the facts here
            // and the releasing guard prints them after 1L is released (`DeferredDiags::emit`).
            self.sched
                .deferred
                .push(DeferredDiag::WakeOnlyDenied { cpu, tid });
            // Stage 199D: `WakeOnly`, not `CpuOffline`. The CPU is up; it refuses work. A
            // direct-IPC wake must be able to tell those apart — collapsing them is what let
            // `sr_enqueue_committed_receiver_split` report a placement that never happened.
            return Err(SchedulerError::WakeOnly);
        }
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "ENQUEUE_QUEUE_INDEX tid={} requested_cpu={} queue_index={}",
                tid.0,
                cpu.0,
                self.idx
            );
        }
        self.sched.rq.enqueue_with_priority(tid, priority).map(|_| {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("ENQUEUE_COMMIT tid={} queue_cpu={}", tid.0, cpu.0);
            }
        })
    }

    pub fn enqueue(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_with_priority(tid, TaskPriority::Normal)
    }

    /// FUTEX-WAKE-CAP1 — promise one user slot on this CPU at `priority`, under its 1L. Refused
    /// for exactly the CPU states an enqueue is refused for (offline, wake-only) and for a full
    /// queue; nothing is mutated on a refusal.
    pub(crate) fn reserve_user_slot(
        &mut self,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
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

    /// FUTEX-WAKE-CAP1 — the promised slots on this CPU at `priority` (tests).
    #[cfg(test)]
    pub(crate) fn reserved_user_slots(&self, priority: TaskPriority) -> usize {
        self.sched.rq.reserved_user_slots(priority)
    }

    /// SCHED-SAT-EVIDENCE1 (witness only) — this CPU's queue occupancy at `priority`.
    #[cfg(any(test, feature = "sched-time-witness", feature = "futex-time-witness"))]
    pub(crate) fn witness_occupancy(&self, priority: TaskPriority) -> (usize, usize) {
        self.sched.rq.witness_occupancy(priority)
    }

    /// FUTEX-WAKE-CAP1 — fill a slot promised by [`Self::reserve_user_slot`] on this CPU. The CPU
    /// was online and not wake-only when the promise was made, and neither changes back.
    pub(crate) fn enqueue_reserved_with_priority(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.sched.rq.enqueue_reserved_with_priority(tid, priority)
    }

    /// Provenance-preserving form of [`Self::dispatch_next`].
    pub fn dispatch_next_selection(&mut self) -> DispatchSelection {
        if !self.online() {
            return DispatchSelection::Idle;
        }
        self.sched.rq.dispatch_next_selection()
    }

    /// U9-DISPATCH-CPU1 §1 — an offline CPU yields `Empty`.
    pub fn dispatch_next_accepted_selection(
        &mut self,
        accept: impl FnMut(ThreadId) -> bool,
    ) -> AcceptedSelection {
        if !self.online() {
            return AcceptedSelection::Empty;
        }
        self.sched.rq.dispatch_next_accepted_selection(accept)
    }

    pub fn on_preempt_selection(&mut self) -> DispatchSelection {
        if !self.online() {
            return DispatchSelection::Idle;
        }
        self.sched.rq.on_preempt_selection()
    }

    pub fn on_preempt_prefer_selection(&mut self, preferred: ThreadId) -> DispatchSelection {
        if !self.online() {
            return DispatchSelection::Idle;
        }
        self.sched.rq.on_preempt_prefer_selection(preferred)
    }

    pub fn dispatch_next(&mut self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        let cpu = self.idx;
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "SCHED cpu={} idle_tid={} runq_len={}",
                cpu,
                self.sched.rq.current_tid().unwrap_or(ThreadId(0)).0,
                self.sched.rq.runnable_count()
            );
        }
        let final_tid = self.sched.rq.dispatch_next();
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("SCHED cpu={} final_tid={:?}", cpu, final_tid);
        }
        final_tid
    }

    pub fn on_preempt(&mut self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.on_preempt()
    }

    /// Stage 192B: re-enqueue the current task and clear the slot WITHOUT dispatching.
    pub fn preempt_reenqueue_only(&mut self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.preempt_reenqueue_only()
    }

    /// RT-SCHED1A — [`Self::preempt_reenqueue_only`] for a task PREEMPTED by a strictly higher
    /// priority: it returns to the HEAD of its own level (SC-PRI-12 (a)).
    pub fn preempt_reenqueue_head_only(&mut self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.preempt_reenqueue_head_only()
    }

    pub fn on_preempt_prefer(&mut self, preferred: ThreadId) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.on_preempt_prefer(preferred)
    }

    pub fn block_current(&mut self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.block_current()
    }

    /// U9-EXIT3 §2 — compare-and-clear. See [`PriorityScheduler::block_current_exact`].
    pub fn block_current_exact(&mut self, tid: ThreadId) -> Option<TaskPriority> {
        if !self.online() {
            return None;
        }
        self.sched.rq.block_current_exact(tid)
    }

    /// U9-EXIT3 §2 — restore. Refuses only on this CPU; "on no OTHER CPU" is the caller's proof.
    pub fn restore_exact_current(&mut self, tid: ThreadId, priority: TaskPriority) -> bool {
        if !self.online() {
            return false;
        }
        self.sched.rq.restore_exact_current(tid, priority)
    }

    /// Stage 199D: withdraw one queued incarnation of `tid` from THIS CPU's queue only.
    pub(crate) fn withdraw_queued_tid(&mut self, tid: ThreadId) -> WithdrawOutcome {
        if !self.online() {
            return WithdrawOutcome::InvalidCpu;
        }
        self.sched.rq.withdraw_queued_tid(tid)
    }

    pub fn current_tid(&self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.current_tid()
    }

    pub fn current_priority(&self) -> Option<TaskPriority> {
        if !self.online() {
            return None;
        }
        self.sched.rq.current_priority()
    }

    pub fn runnable_count(&self) -> usize {
        if !self.online() {
            return 0;
        }
        self.sched.rq.runnable_count()
    }

    /// Non-mutating peek of the next-runnable candidate (highest-priority queue head).
    pub fn peek_next_runnable(&self) -> Option<ThreadId> {
        if !self.online() {
            return None;
        }
        self.sched.rq.peek_highest()
    }

    /// Is `tid` this CPU's current task or in one of its queues (online or not)?
    pub fn contains_or_current(&self, tid: ThreadId) -> bool {
        self.sched.rq.contains_or_current(tid)
    }

    /// SCHED-LOCK2B — hosted FIXTURE only: queue a second entry for `tid` even while it is this
    /// CPU's current (the membership mirror covers the current slot, so `enqueue_with_priority`
    /// refuses that). It builds the current-and-queued invariant break the placement readers must
    /// report as a second placement; production has no way to reach this state.
    #[cfg(test)]
    pub(crate) fn plant_second_placement_for_test(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.sched.rq.push_unchecked_capacity(tid, priority)
    }

    /// SCHED-LOCK2B — [`SmpScheduler::placement_of`] restricted to THIS CPU, under this CPU's 1L
    /// only. For a TID whose home is this CPU it is the whole answer (H-HOME, §20.5): the four
    /// shapes the coherent read distinguished, with every `CpuId` naming this CPU.
    pub(crate) fn placement_here_of(&self, tid: ThreadId) -> TaskPlacement {
        let cpu = self.cpu();
        match self.sched.placement_here(tid) {
            (false, false) => TaskPlacement::Nowhere,
            (false, true) => TaskPlacement::Queued(cpu),
            (true, false) => TaskPlacement::Current(cpu),
            (true, true) => TaskPlacement::CurrentAndQueued(cpu),
        }
    }

    /// Visit every queued entry in dispatch order (High → Normal → Low, FIFO within each).
    #[cfg(any(test, feature = "hosted-dev"))]
    pub fn for_each_queued(&self, mut f: impl FnMut(TaskPriority, ThreadId)) {
        if !self.online() {
            return;
        }
        self.sched.rq.for_each_queued(&mut f);
    }

    /// One timer interrupt on this CPU: the clock owner advances the scheduler tick; every CPU
    /// consumes only its own quantum. Returns `(scheduler tick, this tick preempts)`.
    ///
    /// GP: the tick preempts when this CPU's quantum expired. RT-SCHED1A, RT profile: the quantum
    /// still counts, but a tick preempts only when a strictly higher-priority entry is queued
    /// ([`Self::rt_preempt_wanted`]): an ordinary quantum expiry never rotates an RT task
    /// (SC-PRI-11). The tick is then a scheduling point for a wake it did not itself announce.
    pub fn tick(&mut self) -> (Tick, bool) {
        let (_, expired) = self.sched.quantum.tick_and_check();
        let now = if self.idx == CLOCK_OWNER_CPU.0 as usize {
            self.clock.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
        } else {
            self.clock.load(Ordering::Acquire)
        };
        // RT-SCHED1B (SC-CTX-7): charge, apply due releases, then decide — at THIS instant.
        self.sched
            .rq
            .budget_sync(now, self.topo.budget(), self.idx as u8);
        self.sched.rq.note_essential(true);
        let preempt = if self.topo.rt() {
            self.sched.rq.rt_preempt_wanted()
        } else {
            expired
        };
        (TickInstant(now), preempt)
    }

    /// Would this CPU's NEXT timer interrupt preempt? GP: its quantum would expire. RT profile:
    /// a strictly higher-priority entry is queued now.
    pub fn would_preempt_next(&self) -> bool {
        if self.topo.rt() {
            self.sched.rq.rt_preempt_wanted()
        } else {
            self.sched.quantum.would_preempt_next()
        }
    }

    /// RT-SCHED1A — this CPU's domain runs the RT-P1 profile.
    pub fn rt_profile(&self) -> bool {
        self.topo.rt()
    }

    /// RT-SCHED1B — does this CPU enforce budgets (RT profile with `yarm.sched_budget=on`)?
    pub fn budget_enabled(&self) -> bool {
        self.topo.budget()
    }

    /// RT-SCHED1B — bind `tid`'s incarnation `stamp` to a context with `params` on this CPU (its
    /// home), at its first placement. A no-op when budgets are not enforced; `ContextPoolFull`
    /// when the pool is full.
    pub(crate) fn bind_context(
        &mut self,
        tid: ThreadId,
        stamp: u64,
        params: crate::kernel::sched_budget::BudgetParams,
    ) -> Result<(), SchedulerError> {
        self.validate_online()?;
        self.sched.rq.bind_context(tid, stamp, params)
    }

    /// RT-SCHED1B — release `tid`'s context (its incarnation `stamp`, `0` for any) when the
    /// incarnation is retired. `false`: none was bound here.
    pub(crate) fn release_context(&mut self, tid: ThreadId, stamp: u64) -> bool {
        self.sched.rq.release_context(tid, stamp)
    }

    /// FUTEX-SEAL1 live witness only (`kernel::seal_witness`): charge `tid`'s context on this
    /// budgeted CPU its whole remaining budget through the production `sched_budget::charge`
    /// (an induced exhaustion, not elapsed time). `(remaining before, tick, next release)`, or
    /// `None` when budgets are off or `tid` has no context here.
    #[cfg(feature = "futex-seal-witness")]
    pub(crate) fn seal_witness_drain(&mut self, tid: ThreadId) -> Option<(u64, u64, u64)> {
        let rq = &mut self.sched.rq;
        if !rq.budget.enabled {
            return None;
        }
        let now = rq.budget.now;
        let c = rq.budget.get_mut(tid.0)?;
        let before = c.remaining();
        let _ = crate::kernel::sched_budget::charge(c, before);
        Some((before, now, c.next_release()))
    }

    /// RT-SCHED1B — `tid`'s context on this CPU (observation).
    pub(crate) fn context_of(
        &self,
        tid: ThreadId,
    ) -> Option<crate::kernel::sched_budget::SchedContext> {
        self.sched.rq.context_of(tid)
    }

    /// RT-SCHED1B — hosted FIXTURE only: `tid`'s context, mutable, to construct a state no
    /// production path reaches in a test's lifetime (a near-overflow debt).
    #[cfg(test)]
    pub(crate) fn budget_context_mut_for_test(
        &mut self,
        tid: ThreadId,
    ) -> Option<&mut crate::kernel::sched_budget::SchedContext> {
        self.sched.rq.budget.get_mut(tid.0)
    }

    /// RT-SCHED1B — (idle, unaccounted, essential ticks, essential arrivals, contexts bound).
    pub(crate) fn budget_ledger(&self) -> (u64, u64, u64, u64, usize) {
        self.sched.rq.budget_ledger()
    }

    /// RT-SCHED1B — an RT reschedule arrival was settled here (the essential-work ledger).
    pub(crate) fn note_rt_arrival(&mut self) {
        self.sched.rq.note_essential(false);
    }

    /// RT-SCHED1A — SC-PRI-13: in the RT profile, does a queued entry of STRICTLY higher priority
    /// than this CPU's current task (a user task or a kernel thread) require a switch at this
    /// CPU's next legal scheduling point? Always `false` under GP, for an empty slot, and for the
    /// tid-0 idle placeholder. Read-only, O(1).
    pub fn rt_preempt_wanted(&self) -> bool {
        self.online() && self.topo.rt() && self.sched.rq.rt_preempt_wanted()
    }

    /// RT-SCHED1A — the highest priority queued on this CPU (kernel threads included).
    pub fn best_queued_priority(&self) -> Option<TaskPriority> {
        if !self.online() {
            return None;
        }
        self.sched.rq.best_queued_priority()
    }

    /// A voluntary switch: the next task here starts with a full quantum.
    pub fn reset_quantum(&mut self) {
        self.sched.quantum.reset_quantum();
    }

    pub fn set_quantum(&mut self, quantum_ticks: u64) {
        self.sched.quantum.set_quantum(quantum_ticks);
    }

    /// SCHED-LOCK1 witness only: the tick hook, inside this CPU's held 1L (see
    /// `kernel::schedlock1_witness::on_tick`).
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_tick(&self) {
        if let Some(cell) = self.cell {
            crate::kernel::schedlock1_witness::on_tick(
                self.cpu(),
                cell as *const CpuCell as usize,
                || cell.lock.is_locked(),
                self.sched,
            );
        }
    }
}

/// SCHED-LOCK1 — held 1G and every 1L (see [`SchedDomain::lock_all`]).
pub struct SchedAllGuard<'a> {
    domain: &'a SchedDomain,
    guards: [Option<TicketLockIrqGuard<'a, CpuSched>>; MAX_CPUS],
    admission: Option<TicketLockIrqGuard<'a, Admission>>,
}

impl SchedAllGuard<'_> {
    pub fn view(&mut self) -> SchedAll<'_> {
        let domain = self.domain;
        let now = domain.clock.load(Ordering::Acquire);
        let budget = domain.topo.budget();
        let mut idx = 0u8;
        let cpus = self.guards.each_mut().map(|g| {
            let sched = &mut **g
                .as_mut()
                .expect("every 1L is held for the guard's lifetime");
            // RT-SCHED1B: every CPU's ledger is brought up to date as the coherent view opens.
            sched.rq.budget_sync(now, budget, idx);
            idx = idx.wrapping_add(1);
            sched
        });
        SchedAll {
            cpus,
            mirrors: core::array::from_fn(|idx| &domain.cpus[idx].mirror),
            admission: self
                .admission
                .as_mut()
                .expect("1G is held for the guard's lifetime"),
            topo: &domain.topo,
            clock: &domain.clock,
            // Every lock is still held when the view drops: the guard prints after releasing.
            emit_deferred: false,
        }
    }
}

impl Drop for SchedAllGuard<'_> {
    fn drop(&mut self) {
        // Release the 1Ls in DESCENDING order — each guard restores the interrupt state its own
        // acquisition saved, so the first-acquired guard is released last — then 1G. (Mirrors
        // were republished by the view, while every 1L was held.)
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::all_released();
        let mut deferred = [DeferredDiags::default(); MAX_CPUS];
        for idx in (0..MAX_CPUS).rev() {
            if let Some(mut guard) = self.guards[idx].take() {
                deferred[idx] = guard.deferred.take();
                drop(guard);
            }
        }
        drop(self.admission.take());
        // SCHED-LOCK-FAIR1: printed only once every 1L and 1G is released, in CPU order.
        for d in deferred {
            d.emit();
        }
    }
}

/// SCHED-LOCK1 — the coherent whole-domain view: every CPU's state, admission, the bitmaps and
/// the clock. Exists only while every 1L and 1G are held ([`SchedAllGuard`]) or under exclusive
/// ownership of the domain ([`SchedDomain::exclusive`]). The `_on` methods are the base
/// `SmpScheduler` API, each delegating to that CPU's [`CpuView`].
pub struct SchedAll<'a> {
    cpus: [&'a mut CpuSched; MAX_CPUS],
    mirrors: [&'a CpuMirror; MAX_CPUS],
    admission: &'a mut Admission,
    topo: &'a SchedTopo,
    clock: &'a AtomicU64,
    /// SCHED-LOCK-FAIR1: `true` only for the lock-free exclusive view, whose drop may print the
    /// deferred diagnostics; under [`SchedAllGuard`] the guard prints them after releasing.
    emit_deferred: bool,
}

impl Drop for SchedAll<'_> {
    fn drop(&mut self) {
        for idx in 0..MAX_CPUS {
            publish(self.mirrors[idx], self.cpus[idx]);
        }
        if self.emit_deferred {
            for idx in 0..MAX_CPUS {
                self.cpus[idx].deferred.take().emit();
            }
        }
    }
}

impl SchedAll<'_> {
    fn check_cpu(cpu: CpuId) -> Result<usize, SchedulerError> {
        let idx = cpu.0 as usize;
        if idx >= MAX_CPUS {
            return Err(SchedulerError::InvalidCpu);
        }
        Ok(idx)
    }

    /// CPU `cpu`'s view under this authority.
    pub fn cpu(&mut self, cpu: CpuId) -> Result<CpuView<'_>, SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        self.cpus[idx].rq.budget_sync(
            self.clock.load(Ordering::Acquire),
            self.topo.budget(),
            idx as u8,
        );
        Ok(CpuView {
            idx,
            sched: &mut *self.cpus[idx],
            topo: self.topo,
            clock: self.clock,
            #[cfg(feature = "sched-lock1-witness")]
            cell: None,
        })
    }

    /// SCHED-LOCK1 witness only: CPU `cpu`'s state under this (coherent) view, read-only.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_cell(&self, cpu: CpuId) -> &CpuSched {
        &*self.cpus[cpu.0 as usize]
    }

    /// FUTEX-WAKE-CAP1 — the user slots promised on `cpu`, all priorities (tests).
    #[cfg(test)]
    pub(crate) fn reserved_user_slots_on(&self, cpu: CpuId) -> usize {
        self.cpu_ref(cpu).map_or(0, |(_, s)| {
            [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low]
                .into_iter()
                .map(|p| s.rq.reserved_user_slots(p))
                .sum()
        })
    }

    fn cpu_ref(&self, cpu: CpuId) -> Option<(usize, &CpuSched)> {
        let idx = cpu.0 as usize;
        (idx < MAX_CPUS && self.topo.cpu_online(idx)).then(|| (idx, &*self.cpus[idx]))
    }

    pub fn validate_online_cpu(&self, cpu: CpuId) -> Result<(), SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        if !self.topo.cpu_online(idx) {
            return Err(SchedulerError::CpuOffline);
        }
        Ok(())
    }

    /// Stage 183.5: mark/unmark `cpu` wake-only. Idempotent.
    pub fn set_cpu_wake_only(&mut self, cpu: CpuId, wake_only: bool) -> Result<(), SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        SchedTopo::set_bit(&self.topo.wake_only, idx, wake_only);
        Ok(())
    }

    pub fn cpu_wake_only(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .map(|idx| self.topo.wake_only() & (1u64 << idx) != 0)
            .unwrap_or(false)
    }

    pub fn wake_only_bitmap(&self) -> u64 {
        self.topo.wake_only()
    }

    /// The dispatching CPUs (online, not wake-only) — [`SchedTopo::dispatching`].
    pub fn dispatching_bitmap(&self) -> u64 {
        self.topo.dispatching()
    }

    /// Stage 183.5: install the scheduler-owned IDLE current (tid 0) for a wake-only online AP.
    pub fn install_ap_idle_current(&mut self, cpu: CpuId) -> Result<ThreadId, SchedulerError> {
        self.validate_online_cpu(cpu)?;
        let idx = cpu.0 as usize;
        if self.topo.wake_only() & (1u64 << idx) == 0 {
            return Err(SchedulerError::CpuOffline);
        }
        if self.cpus[idx].rq.current_tid().is_some() {
            return Err(SchedulerError::AlreadyQueued);
        }
        let idle = ScheduledTask {
            tid: ThreadId(0),
            priority: TaskPriority::Low,
        };
        self.cpus[idx].rq.current = Some(idle);
        Ok(idle.tid)
    }

    /// QEMU-SMP2 — admit a wake-only AP to dispatch only explicitly placed tasks.
    pub fn admit_pinned_dispatch(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        let mut view = self.cpu(cpu)?;
        admit(&mut view, true)
    }

    /// AP-SCHED1 — admit a wake-only CPU to production (balanced) dispatch.
    pub fn admit_dispatch(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        let mut view = self.cpu(cpu)?;
        admit(&mut view, false)
    }

    /// AP-SCHED1 — the CPU balanced placement would choose now, from the coherent loads.
    pub fn balanced_choice(&self) -> Result<CpuId, SchedulerError> {
        self.topo
            .least_loaded(self.admission.next_balance_cpu, |idx| self.cpus[idx].load())
    }

    pub fn note_balanced_choice(&mut self, cpu: CpuId) {
        self.admission.next_balance_cpu = (cpu.0 as usize + 1) % MAX_CPUS;
    }

    pub fn balance_excluded_bitmap(&self) -> u64 {
        self.topo.balance_excluded()
    }

    /// Simulates the full secondary-CPU bring-up handshake in a single call.
    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        Self::check_cpu(cpu)?;
        let topology = &mut self.admission.topology;
        let result = if topology.start_secondary_cpu(cpu.0).is_err()
            || topology.acknowledge_secondary_cpu(cpu.0).is_err()
            || topology.mark_cpu_online(cpu.0).is_err()
        {
            Err(SchedulerError::CpuOffline)
        } else {
            Ok(())
        };
        self.topo.publish_topology(&self.admission.topology);
        result
    }

    #[cfg(any(test, feature = "hosted-dev"))]
    pub fn for_each_queued_on(&self, cpu: CpuId, mut f: impl FnMut(TaskPriority, ThreadId)) {
        let Some((_, sched)) = self.cpu_ref(cpu) else {
            return;
        };
        sched.rq.for_each_queued(&mut f);
    }

    pub fn cpu_is_online(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .map(|idx| self.topo.cpu_online(idx))
            .unwrap_or(false)
    }

    pub fn online_cpu_bitmap(&self) -> u64 {
        self.topo.online()
    }

    pub fn online_cpu_count(&self) -> usize {
        self.topo.online().count_ones() as usize
    }

    pub fn present_cpu_count(&self) -> usize {
        self.topo.present().count_ones() as usize
    }

    pub fn present_cpu_bitmap(&self) -> u64 {
        self.topo.present()
    }

    /// WARNING: resets all secondary CPU online state (queued work stays where it is).
    pub fn set_present_cpu_bitmap(&mut self, present: u64) {
        self.admission.topology = CpuTopology::from_present_bitmap(present);
        self.admission.next_balance_cpu = 0;
        self.topo.publish_topology(&self.admission.topology);
    }

    pub fn enqueue_on_with_priority(
        &mut self,
        cpu: CpuId,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.validate_online_cpu(cpu)?;
        self.cpu(cpu)?.enqueue_with_priority(tid, priority)
    }

    pub fn enqueue_balanced(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<CpuId, SchedulerError> {
        let cpu = self.balanced_choice()?;
        self.enqueue_on_with_priority(cpu, tid, priority)?;
        self.note_balanced_choice(cpu);
        Ok(cpu)
    }

    pub fn enqueue_on(&mut self, cpu: CpuId, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_on_with_priority(cpu, tid, TaskPriority::Normal)
    }

    pub fn dispatch_next_selection_on(&mut self, cpu: CpuId) -> DispatchSelection {
        self.cpu(cpu)
            .map_or(DispatchSelection::Idle, |mut v| v.dispatch_next_selection())
    }

    pub fn dispatch_next_accepted_selection_on(
        &mut self,
        cpu: CpuId,
        accept: impl FnMut(ThreadId) -> bool,
    ) -> AcceptedSelection {
        self.cpu(cpu).map_or(AcceptedSelection::Empty, |mut v| {
            v.dispatch_next_accepted_selection(accept)
        })
    }

    pub fn on_preempt_selection_on(&mut self, cpu: CpuId) -> DispatchSelection {
        self.cpu(cpu)
            .map_or(DispatchSelection::Idle, |mut v| v.on_preempt_selection())
    }

    pub fn on_preempt_prefer_selection_on(
        &mut self,
        cpu: CpuId,
        preferred: ThreadId,
    ) -> DispatchSelection {
        self.cpu(cpu).map_or(DispatchSelection::Idle, |mut v| {
            v.on_preempt_prefer_selection(preferred)
        })
    }

    pub fn dispatch_next_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu(cpu).ok()?.dispatch_next()
    }

    pub fn on_preempt_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu(cpu).ok()?.on_preempt()
    }

    pub fn preempt_reenqueue_only_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu(cpu).ok()?.preempt_reenqueue_only()
    }

    pub fn on_preempt_prefer_on(&mut self, cpu: CpuId, preferred: ThreadId) -> Option<ThreadId> {
        self.cpu(cpu).ok()?.on_preempt_prefer(preferred)
    }

    pub fn block_current_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu(cpu).ok()?.block_current()
    }

    pub fn block_current_exact_on(&mut self, cpu: CpuId, tid: ThreadId) -> Option<TaskPriority> {
        self.cpu(cpu).ok()?.block_current_exact(tid)
    }

    pub fn restore_exact_current_on(
        &mut self,
        cpu: CpuId,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> bool {
        self.cpu(cpu)
            .is_ok_and(|mut v| v.restore_exact_current(tid, priority))
    }

    /// U9-EXIT3 §2 — is `tid` current on, or queued on, ANY CPU? (coherent)
    pub fn tid_present_anywhere(&self, tid: ThreadId) -> bool {
        self.cpus.iter().any(|s| s.rq.contains_or_current(tid))
    }

    /// RT-SCHED1B — release `tid`'s context on `cpu` (its incarnation `stamp`; `0`: any).
    pub(crate) fn release_context_on(&mut self, cpu: CpuId, tid: ThreadId, stamp: u64) -> bool {
        self.cpu(cpu)
            .map_or(false, |mut v| v.release_context(tid, stamp))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn withdraw_queued_tid_on(&mut self, cpu: CpuId, tid: ThreadId) -> WithdrawOutcome {
        self.cpu(cpu).map_or(WithdrawOutcome::InvalidCpu, |mut v| {
            v.withdraw_queued_tid(tid)
        })
    }

    pub fn current_tid_on(&self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu_ref(cpu)?.1.rq.current_tid()
    }

    pub fn current_priority_on(&self, cpu: CpuId) -> Option<TaskPriority> {
        self.cpu_ref(cpu)?.1.rq.current_priority()
    }

    pub fn runnable_count_on(&self, cpu: CpuId) -> usize {
        self.cpu_ref(cpu).map_or(0, |(_, s)| s.rq.runnable_count())
    }

    /// Stage 199A2D2C2B1: true iff `tid` is queued or current on ANY CPU. (coherent)
    pub fn task_present_anywhere(&self, tid: ThreadId) -> bool {
        self.tid_present_anywhere(tid)
    }

    /// U9-RECV-BLOCK2 §1 — WHERE `tid` is, as ONE coherent observation of every CPU.
    /// SCHED-LOCK2B: no production caller remains — production placement questions about one
    /// incarnation are answered on its home by `CpuView::placement_here_of`. Retained for hosted
    /// tests and the census.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn placement_of(&self, tid: ThreadId) -> TaskPlacement {
        let mut queued_on = None;
        let mut current_on = None;
        for idx in 0..MAX_CPUS {
            let (current, queued) = self.cpus[idx].placement_here(tid);
            if current {
                current_on = Some(CpuId(idx as u8));
            }
            if queued {
                queued_on = Some(CpuId(idx as u8));
            }
        }
        match (current_on, queued_on) {
            (None, None) => TaskPlacement::Nowhere,
            (None, Some(cpu)) => TaskPlacement::Queued(cpu),
            (Some(cpu), None) => TaskPlacement::Current(cpu),
            (Some(cpu), Some(_)) => TaskPlacement::CurrentAndQueued(cpu),
        }
    }

    pub fn peek_next_runnable_on(&self, cpu: CpuId) -> Option<ThreadId> {
        self.cpu_ref(cpu)?.1.rq.peek_highest()
    }

    // ── time and quanta ──────────────────────────────────────────────────────────────────

    /// One timer interrupt on `cpu` (see [`CpuView::tick`]). An out-of-range CPU ticks nothing
    /// and reads the clock.
    pub fn tick_on(&mut self, cpu: CpuId) -> (Tick, bool) {
        match self.cpu(cpu) {
            Ok(mut v) => v.tick(),
            Err(_) => (TickInstant(self.clock.load(Ordering::Acquire)), false),
        }
    }

    pub fn would_preempt_next_on(&self, cpu: CpuId) -> bool {
        let idx = cpu.0 as usize;
        idx < MAX_CPUS && self.cpus[idx].quantum.would_preempt_next()
    }

    pub fn reset_quantum_on(&mut self, cpu: CpuId) {
        if let Ok(mut v) = self.cpu(cpu) {
            v.reset_quantum();
        }
    }

    /// Install `quantum_ticks` on every CPU's quantum. The clock is untouched.
    pub fn set_quantum_all(&mut self, quantum_ticks: u64) {
        for s in self.cpus.iter_mut() {
            s.quantum.set_quantum(quantum_ticks);
        }
    }

    /// The scheduler tick.
    pub fn current_ticks(&self) -> Tick {
        TickInstant(self.clock.load(Ordering::Acquire))
    }

    /// Hosted tests: replace the clock owner's timer (clock value and quantum) wholesale.
    #[cfg(test)]
    pub fn set_clock_owner_timer_for_test(&mut self, timer: Timer) {
        self.clock.store(timer.current_ticks().0, Ordering::Release);
        self.cpus[CLOCK_OWNER_CPU.0 as usize].quantum = timer;
    }

    /// The clock owner's quantum, for tests that inspect it.
    #[cfg(test)]
    pub fn quantum_for_test(&self, cpu: CpuId) -> Timer {
        self.cpus[cpu.0 as usize].quantum
    }

    /// `posix-compat` / tests: advance the clock by one tick outside an interrupt (the base
    /// `timer.tick_and_check()` on the clock owner's timer, quantum included).
    pub fn advance_clock_owner_tick(&mut self) -> Tick {
        self.tick_on(CLOCK_OWNER_CPU).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Stage 199D: non-dispatching runqueue withdrawal ─────────────────────────────────

    fn new_domain() -> SchedDomain {
        SchedDomain::new(topology::default_present_cpu_bitmap(), 1)
    }

    fn online_two_cpu_scheduler() -> SchedDomain {
        let mut dom = new_domain();
        let mut sched = dom.exclusive();
        sched.set_present_cpu_bitmap(0b11);
        sched.bring_up_cpu(CpuId(1)).expect("cpu1 online");
        drop(sched);
        dom
    }

    #[test]
    fn withdraw_removes_from_each_priority_queue() {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            let mut sched_dom = new_domain();
            let mut sched = sched_dom.exclusive();
            sched
                .enqueue_on_with_priority(CpuId(0), ThreadId(7), priority)
                .expect("enqueue");
            assert_eq!(
                sched.withdraw_queued_tid_on(CpuId(0), ThreadId(7)),
                WithdrawOutcome::Removed,
                "{priority:?} queue"
            );
            assert_eq!(sched.runnable_count_on(CpuId(0)), 0);
            // …and the TID may be enqueued again, proving the membership mirror was cleared.
            sched
                .enqueue_on_with_priority(CpuId(0), ThreadId(7), priority)
                .expect("re-enqueue after withdrawal");
        }
    }

    #[test]
    fn withdraw_handles_head_middle_and_tail_positions() {
        for target in [ThreadId(1), ThreadId(2), ThreadId(3)] {
            let mut sched_dom = new_domain();
            let mut sched = sched_dom.exclusive();
            for tid in [ThreadId(1), ThreadId(2), ThreadId(3)] {
                sched
                    .enqueue_on_with_priority(CpuId(0), tid, TaskPriority::Normal)
                    .expect("enqueue");
            }
            assert_eq!(
                sched.withdraw_queued_tid_on(CpuId(0), target),
                WithdrawOutcome::Removed
            );
            // FIFO order of the survivors is preserved.
            let mut seen = alloc::vec::Vec::new();
            while let Some(tid) = sched.dispatch_next_on(CpuId(0)) {
                if seen.contains(&tid) {
                    break;
                }
                seen.push(tid);
                sched.block_current_on(CpuId(0));
            }
            let expected: alloc::vec::Vec<ThreadId> = [ThreadId(1), ThreadId(2), ThreadId(3)]
                .into_iter()
                .filter(|t| *t != target)
                .collect();
            assert_eq!(
                seen, expected,
                "FIFO order preserved after removing {target:?}"
            );
        }
    }

    #[test]
    fn withdraw_preserves_order_in_a_level_whose_entries_are_scattered_through_the_pool() {
        // RT-SCHED1A replaced the ring: a level's entries are linked through a shared pool, so
        // churn on another level leaves this level's entries in non-adjacent, non-ascending pool
        // slots. Removal must still keep the level's FIFO order exactly.
        let mut q = RunQueues::new();
        let other = 7;
        let level = 3;
        for round in 0..40u64 {
            for k in 0..5u64 {
                q.push_back(
                    other,
                    ThreadId(50_000 + round * 5 + k),
                    MAX_QUEUED_PER_PRIORITY,
                )
                .expect("other level");
            }
            q.push_back(level, ThreadId(round + 1), MAX_QUEUED_PER_PRIORITY)
                .expect("level");
            for _ in 0..5 {
                q.pop_front(other).expect("drain other");
            }
        }
        assert_eq!(q.len(level), 40);
        assert_eq!(q.len(other), 0);
        assert!(q.remove_tid(level, ThreadId(1)), "head");
        assert!(q.remove_tid(level, ThreadId(21)), "middle");
        assert!(q.remove_tid(level, ThreadId(40)), "tail");
        assert!(!q.remove_tid(level, ThreadId(40)), "gone");
        let order: alloc::vec::Vec<u64> = q.iter(level).map(|t| t.0).collect();
        let want: alloc::vec::Vec<u64> = (2..40).filter(|&t| t != 21).collect();
        assert_eq!(order, want, "scattered removal preserves FIFO order");
        // The tail was removed: a push must land after 39.
        q.push_back(level, ThreadId(99), MAX_QUEUED_PER_PRIORITY)
            .expect("push");
        assert_eq!(q.iter(level).last(), Some(ThreadId(99)));
        assert_eq!(q.total(), 38);
    }

    #[test]
    fn withdraw_from_an_empty_queue_reports_not_queued() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(0), ThreadId(9)),
            WithdrawOutcome::NotQueued
        );
    }

    #[test]
    fn withdraw_leaves_a_tid_queued_on_another_cpu_untouched() {
        let mut sched_dom = online_two_cpu_scheduler();
        let mut sched = sched_dom.exclusive();
        sched
            .enqueue_on_with_priority(CpuId(1), ThreadId(42), TaskPriority::Normal)
            .expect("enqueue on cpu1");
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(0), ThreadId(42)),
            WithdrawOutcome::NotQueued,
            "withdrawing from the wrong CPU must not find it"
        );
        assert_eq!(sched.runnable_count_on(CpuId(1)), 1, "cpu1 is untouched");
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(1), ThreadId(42)),
            WithdrawOutcome::Removed
        );
        assert_eq!(sched.runnable_count_on(CpuId(1)), 0);
    }

    #[test]
    fn withdraw_refuses_the_current_task_without_mutation() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(5), TaskPriority::Normal)
            .expect("enqueue");
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(6), TaskPriority::Normal)
            .expect("enqueue");
        let current = sched.dispatch_next_on(CpuId(0)).expect("dispatch");
        assert_eq!(current, ThreadId(5));
        let queued_before = sched.runnable_count_on(CpuId(0));
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(0), ThreadId(5)),
            WithdrawOutcome::RefusedCurrent
        );
        assert_eq!(
            sched.current_tid_on(CpuId(0)),
            Some(ThreadId(5)),
            "current unchanged"
        );
        assert_eq!(
            sched.runnable_count_on(CpuId(0)),
            queued_before,
            "no queue mutated"
        );
    }

    #[test]
    fn withdraw_preserves_the_scheduler_owned_idle_current() {
        let mut sched_dom = online_two_cpu_scheduler();
        let mut sched = sched_dom.exclusive();
        sched.set_cpu_wake_only(CpuId(1), true).expect("wake-only");
        sched
            .install_ap_idle_current(CpuId(1))
            .expect("idle current");
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(ThreadId(0)));
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(1), ThreadId(0)),
            WithdrawOutcome::RefusedCurrent,
            "the tid-0 idle current must never be withdrawn"
        );
        assert_eq!(
            sched.current_tid_on(CpuId(1)),
            Some(ThreadId(0)),
            "the idle placeholder survives"
        );
    }

    #[test]
    fn withdraw_fails_closed_on_a_duplicate_occurrence_with_zero_mutation() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        // The public enqueue refuses duplicates, so force the pathological state directly to
        // prove the seam fails closed if it ever arose.
        let high = PriorityScheduler::priority_index(TaskPriority::High);
        let normal = PriorityScheduler::priority_index(TaskPriority::Normal);
        let q = &mut sched.cpus[0].rq.queues;
        q.push_back(high, ThreadId(3), MAX_RUN_QUEUE).expect("push");
        q.push_back(normal, ThreadId(3), MAX_RUN_QUEUE)
            .expect("push");
        q.push_back(normal, ThreadId(8), MAX_RUN_QUEUE)
            .expect("push");
        let before = sched.runnable_count_on(CpuId(0));
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(0), ThreadId(3)),
            WithdrawOutcome::RefusedDuplicate
        );
        assert_eq!(sched.runnable_count_on(CpuId(0)), before, "zero mutation");
        assert!(sched.cpus[0].rq.queues.contains(high, ThreadId(3)));
        assert!(sched.cpus[0].rq.queues.contains(normal, ThreadId(3)));
    }

    #[test]
    fn withdraw_reports_invalid_cpu_for_an_offline_target() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(7), ThreadId(1)),
            WithdrawOutcome::InvalidCpu
        );
    }

    #[test]
    fn withdraw_changes_no_topology_or_current_state() {
        let mut sched_dom = online_two_cpu_scheduler();
        let mut sched = sched_dom.exclusive();
        // Queue on CPU 1 while it is still a dispatching CPU, then mark it wake-only: the
        // withdrawal must not disturb that state either.
        sched
            .enqueue_on_with_priority(CpuId(1), ThreadId(11), TaskPriority::Normal)
            .expect("enqueue");
        sched.set_cpu_wake_only(CpuId(1), true).expect("wake-only");
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(12), TaskPriority::Normal)
            .expect("enqueue");
        let dispatched = sched.dispatch_next_on(CpuId(0)).expect("dispatch");

        let online = sched.online_cpu_bitmap();
        let present = sched.present_cpu_bitmap();
        let wake_only = sched.wake_only_bitmap();
        let cur0 = sched.current_tid_on(CpuId(0));
        let cur1 = sched.current_tid_on(CpuId(1));

        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(1), ThreadId(11)),
            WithdrawOutcome::Removed
        );

        assert_eq!(sched.online_cpu_bitmap(), online, "online bitmap unchanged");
        assert_eq!(
            sched.present_cpu_bitmap(),
            present,
            "present bitmap unchanged"
        );
        assert_eq!(
            sched.wake_only_bitmap(),
            wake_only,
            "wake-only bitmap unchanged"
        );
        assert_eq!(
            sched.current_tid_on(CpuId(0)),
            cur0,
            "cpu0 current unchanged"
        );
        assert_eq!(
            sched.current_tid_on(CpuId(1)),
            cur1,
            "cpu1 current unchanged"
        );
        assert_eq!(cur0, Some(dispatched));
        assert_eq!(sched.runnable_count_on(CpuId(0)), 0, "cpu0 queue untouched");
    }

    #[test]
    fn scheduler_rotates_on_preempt() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(1), TaskPriority::Normal)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(2), TaskPriority::Normal)
                .is_ok()
        );

        assert_eq!(sched.dispatch_next().expect("task 1"), ThreadId(1));
        assert_eq!(sched.on_preempt().expect("task 2"), ThreadId(2));
        assert_eq!(sched.on_preempt().expect("task 1"), ThreadId(1));
    }

    #[test]
    fn scheduler_duplicate_enqueue_is_rejected() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(7), TaskPriority::Normal)
                .is_ok()
        );
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(7), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count(), 1);
    }

    #[test]
    fn scheduler_prefers_higher_priority_work() {
        let mut sched = PriorityScheduler::default();
        assert!(
            sched
                .enqueue_with_priority(ThreadId(10), TaskPriority::Low)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(20), TaskPriority::High)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_with_priority(ThreadId(30), TaskPriority::Normal)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next(), Some(ThreadId(20)));
        assert_eq!(sched.current_priority(), Some(TaskPriority::High));
    }

    #[test]
    fn balanced_enqueue_round_robins_equal_load_cpus() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");

        let cpu_a = sched
            .enqueue_balanced(ThreadId(10), TaskPriority::Normal)
            .expect("enqueue a");
        let cpu_b = sched
            .enqueue_balanced(ThreadId(11), TaskPriority::Normal)
            .expect("enqueue b");

        assert_eq!(cpu_a, CpuId(0));
        assert_eq!(cpu_b, CpuId(1));
    }

    // Stage 195D (BSP DISPATCH AFFINITY): a wake-only online CPU must never receive a
    // balanced user-task placement — `enqueue_balanced` routes to the least-loaded
    // NON-wake-only CPU (the BSP), so an unpinned user task is never stranded on a
    // non-dispatching AP. This is the invariant the AArch64 AP bring-up now establishes.
    #[test]
    fn stage195d_balanced_enqueue_avoids_wake_only_ap() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");
        sched
            .set_cpu_wake_only(CpuId(1), true)
            .expect("mark ap wake-only");
        // Even though CPU 1 is emptier than the BSP, balanced placement must pick CPU 0.
        let a = sched
            .enqueue_balanced(ThreadId(10), TaskPriority::Normal)
            .expect("enqueue a");
        let b = sched
            .enqueue_balanced(ThreadId(11), TaskPriority::Normal)
            .expect("enqueue b");
        assert_eq!(a, CpuId(0), "first balanced placement must be the BSP");
        assert_eq!(
            b,
            CpuId(0),
            "second balanced placement must also be the BSP"
        );
    }

    // Stage 195D: explicit placement onto a wake-only AP is denied (the caller reroutes to the
    // BSP). This is what emits `AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED` on a real rejection.
    #[test]
    fn stage195d_explicit_enqueue_onto_wake_only_ap_denied() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");
        sched
            .set_cpu_wake_only(CpuId(1), true)
            .expect("mark ap wake-only");
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(1), ThreadId(20), TaskPriority::Normal)
                .is_err(),
            "explicit placement onto a wake-only AP must be denied"
        );
        // The BSP still accepts placement.
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(0), ThreadId(21), TaskPriority::Normal)
                .is_ok()
        );
    }

    // Stage 195D: with every AP wake-only, dispatching collapses to the single BSP even though
    // two CPUs are online — the single_dispatcher topology the AArch64 SMP=2 boot now attains.
    #[test]
    fn stage195d_all_aps_wake_only_is_single_dispatcher() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");
        sched
            .set_cpu_wake_only(CpuId(1), true)
            .expect("mark ap wake-only");
        assert_eq!(sched.online_cpu_count(), 2);
        let dispatching = sched.online_cpu_bitmap() & !sched.wake_only_bitmap();
        assert_eq!(
            dispatching.count_ones(),
            1,
            "only the BSP dispatches user tasks"
        );
        assert_eq!(dispatching & 1, 1, "the sole dispatcher is the BSP (cpu 0)");
    }

    /// QEMU-SMP2: the wake-only AP exactly as `start_secondary_cpus` leaves it — marked
    /// wake-only before bring-up, then given the tid-0 idle placeholder.
    /// AP-SCHED1 — production admission is the same single transition, except that the CPU joins
    /// balanced placement; and it is refused, changing nothing, on the same states.
    #[test]
    fn production_admission_joins_balanced_placement() {
        let mut sched_dom = wake_only_ap();
        let mut sched = sched_dom.exclusive();
        sched.admit_dispatch(CpuId(1)).expect("admitted");
        assert!(!sched.cpu_wake_only(CpuId(1)));
        assert_eq!(sched.balance_excluded_bitmap(), 0);
        assert_eq!(sched.current_tid_on(CpuId(1)), None, "placeholder cleared");
        // CPU 0 carries one task, so the balanced choice is CPU 1.
        sched.enqueue_on(CpuId(0), ThreadId(5)).expect("load CPU 0");
        assert_eq!(sched.balanced_choice(), Ok(CpuId(1)));
        // A second admission is refused: it is not wake-only any more.
        assert_eq!(
            sched.admit_dispatch(CpuId(1)),
            Err(SchedulerError::CpuOffline)
        );
        let mut busy_dom = wake_only_ap();
        let mut busy = busy_dom.exclusive();
        busy.set_cpu_wake_only(CpuId(1), true).expect("wake-only");
        busy.cpus[1].rq.current = Some(ScheduledTask {
            tid: ThreadId(9),
            priority: TaskPriority::Normal,
        });
        assert_eq!(
            busy.admit_dispatch(CpuId(1)),
            Err(SchedulerError::AlreadyQueued)
        );
        assert!(busy.cpu_wake_only(CpuId(1)), "refusal changed nothing");
    }

    fn wake_only_ap() -> SchedDomain {
        let mut dom = new_domain();
        let mut sched = dom.exclusive();
        sched.set_present_cpu_bitmap(0b11);
        sched.set_cpu_wake_only(CpuId(1), true).expect("wake-only");
        sched.bring_up_cpu(CpuId(1)).expect("cpu1 online");
        sched
            .install_ap_idle_current(CpuId(1))
            .expect("idle current");
        drop(sched);
        dom
    }

    #[test]
    fn smp2_admission_is_one_transition_out_of_wake_only() {
        let mut sched_dom = wake_only_ap();
        let mut sched = sched_dom.exclusive();
        assert_eq!(
            sched.enqueue_on(CpuId(1), ThreadId(7)),
            Err(SchedulerError::WakeOnly),
            "before admission an explicit placement is refused"
        );
        sched.admit_pinned_dispatch(CpuId(1)).expect("admitted");
        assert!(!sched.cpu_wake_only(CpuId(1)));
        assert_eq!(
            sched.current_tid_on(CpuId(1)),
            None,
            "the idle placeholder is gone, so the idle boundary reads current == None"
        );
        assert_eq!(sched.balance_excluded_bitmap(), 0b10);
        assert_eq!(
            (sched.online_cpu_bitmap() & !sched.wake_only_bitmap()).count_ones(),
            2,
            "two CPUs dispatch"
        );
        sched
            .enqueue_on(CpuId(1), ThreadId(7))
            .expect("pinned placement accepted");
        assert_eq!(sched.runnable_count_on(CpuId(1)), 1);
    }

    #[test]
    fn smp2_balanced_placement_never_chooses_a_pinned_only_cpu() {
        let mut sched_dom = wake_only_ap();
        let mut sched = sched_dom.exclusive();
        sched.admit_pinned_dispatch(CpuId(1)).expect("admitted");
        // Load the BSP so a least-loaded rule WOULD pick CPU 1 if it were eligible.
        for t in 100..110 {
            sched.enqueue_on(CpuId(0), ThreadId(t)).expect("bsp");
        }
        for t in 200..210 {
            assert_eq!(
                sched.enqueue_balanced(ThreadId(t), TaskPriority::Normal),
                Ok(CpuId(0))
            );
        }
        assert_eq!(sched.runnable_count_on(CpuId(1)), 0);
    }

    #[test]
    fn smp2_admission_refuses_every_other_state_and_changes_nothing() {
        // Not wake-only (the BSP).
        let mut sched_dom = wake_only_ap();
        let mut sched = sched_dom.exclusive();
        assert_eq!(
            sched.admit_pinned_dispatch(CpuId(0)),
            Err(SchedulerError::CpuOffline)
        );
        // Offline.
        let mut offline_dom = new_domain();
        let mut offline = offline_dom.exclusive();
        assert!(offline.admit_pinned_dispatch(CpuId(1)).is_err());
        // Already admitted: no longer wake-only.
        sched.admit_pinned_dispatch(CpuId(1)).expect("first");
        assert_eq!(
            sched.admit_pinned_dispatch(CpuId(1)),
            Err(SchedulerError::CpuOffline)
        );
        // A real task current on the AP.
        let mut busy_dom = new_domain();
        let mut busy = busy_dom.exclusive();
        busy.set_present_cpu_bitmap(0b11);
        busy.set_cpu_wake_only(CpuId(1), true).expect("wake-only");
        busy.bring_up_cpu(CpuId(1)).expect("online");
        busy.cpus[1].rq.current = Some(ScheduledTask {
            tid: ThreadId(42),
            priority: TaskPriority::Normal,
        });
        assert_eq!(
            busy.admit_pinned_dispatch(CpuId(1)),
            Err(SchedulerError::AlreadyQueued)
        );
        assert!(busy.cpu_wake_only(CpuId(1)), "refusal changed nothing");
        assert_eq!(busy.balance_excluded_bitmap(), 0);
        assert_eq!(busy.current_tid_on(CpuId(1)), Some(ThreadId(42)));
    }

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        assert_eq!(sched.online_cpu_count(), 1);
        assert!(sched.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(sched.online_cpu_count(), 2);
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(0), ThreadId(10), TaskPriority::Normal)
                .is_ok()
        );
        assert!(
            sched
                .enqueue_on_with_priority(CpuId(1), ThreadId(20), TaskPriority::High)
                .is_ok()
        );
        assert_eq!(sched.dispatch_next_on(CpuId(0)), Some(ThreadId(10)));
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(ThreadId(20)));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(ThreadId(10)));
        assert_eq!(sched.current_tid_on(CpuId(1)), Some(ThreadId(20)));
    }

    #[test]
    fn membership_insert_reuses_tombstone_when_no_empty_slot_exists() {
        let mut sched = PriorityScheduler::default();
        for idx in 0..MEMBERSHIP_SLOTS {
            sched.membership_keys[idx] = ThreadId(idx as u64 + 1);
            sched.membership_state[idx] = MEMBERSHIP_FULL;
        }
        let tombstone = 7;
        sched.membership_state[tombstone] = MEMBERSHIP_TOMBSTONE;

        let inserted = ThreadId(MEMBERSHIP_SLOTS as u64 + 1);
        assert_eq!(sched.membership_insert(inserted), Ok(()));
        assert_eq!(sched.membership_keys[tombstone], inserted);
        assert_eq!(sched.membership_state[tombstone], MEMBERSHIP_FULL);
    }

    #[test]
    fn membership_capacity_covers_all_queues_plus_running_task() {
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(1), TaskPriority::Normal)
            .expect("seed running task");
        assert_eq!(sched.dispatch_next(), Some(ThreadId(1)));

        let mut next_tid = 2;
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            for _ in 0..MAX_RUN_QUEUE {
                sched
                    .enqueue_with_priority(ThreadId(next_tid), priority)
                    .expect("fill legal run-queue capacity");
                next_tid += 1;
            }
        }

        assert_eq!(sched.runnable_count(), 3 * MAX_RUN_QUEUE);
        assert!(!sched.membership_tracking_exhausted);
        assert!(sched.membership_contains(ThreadId(1)));
        for tid in 2..next_tid {
            assert!(sched.membership_contains(ThreadId(tid)));
        }
    }

    #[test]
    fn membership_tracking_reuses_tombstones_without_linear_fallback() {
        let mut sched = PriorityScheduler::default();
        for tid in 1..=(MEMBERSHIP_SLOTS as u64) {
            sched
                .enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
                .expect("seed task");
            assert_eq!(sched.dispatch_next(), Some(ThreadId(tid)));
            assert_eq!(sched.block_current(), Some(ThreadId(tid)));
        }
        assert_eq!(sched.runnable_count(), 0);

        for tid in 1000..(1000 + MAX_RUN_QUEUE as u64) {
            sched
                .enqueue_with_priority(ThreadId(tid), TaskPriority::Normal)
                .expect("reused membership slot");
        }

        assert!(!sched.membership_tracking_exhausted);
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(1000), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
    }

    #[test]
    fn on_preempt_prefer_selects_preferred_over_fifo_head() {
        // With TID 1 at head and TID 2 behind it, on_preempt_prefer(TID 2) must
        // skip TID 1 and make TID 2 current in one operation.
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(1), TaskPriority::Normal)
            .expect("enqueue 1");
        sched
            .enqueue_with_priority(ThreadId(2), TaskPriority::Normal)
            .expect("enqueue 2");
        sched.dispatch_next(); // make TID 1 current

        // Preempt TID 1 in favor of TID 2.
        let next = sched.on_preempt_prefer(ThreadId(2));
        assert_eq!(next, Some(ThreadId(2)));
        assert_eq!(sched.current_tid(), Some(ThreadId(2)));
        // TID 1 was re-enqueued.
        assert_eq!(sched.runnable_count(), 1);
        assert!(sched.contains_tid(ThreadId(1)));
        assert!(sched.membership_contains(ThreadId(2)));
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(2), TaskPriority::Normal),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count(), 1);
    }

    #[test]
    fn on_preempt_prefer_falls_back_to_fifo_when_preferred_absent() {
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(1), TaskPriority::Normal)
            .expect("enqueue 1");
        sched.dispatch_next(); // make TID 1 current

        // TID 99 is not in any queue; on_preempt_prefer should fall back to FIFO
        // (which re-enqueues TID 1 and picks from head).
        let next = sched.on_preempt_prefer(ThreadId(99));
        // TID 1 was re-enqueued and is the only task; FIFO picks it.
        assert_eq!(next, Some(ThreadId(1)));
    }

    // Stage 199A2D3 (freeze): the BSP saved-frame resume relies on on_preempt_prefer_on making the
    // EXACT client the CPU-0 current, so the resumed task's user-copy resolves its own ASID. These pin
    // the authoritative-selection preconditions.

    // A "replacement" TID that is not runnable on the CPU is REJECTED as the preferred selection — it is
    // never made current; the CPU falls back to its own FIFO instead.
    #[test]
    fn on_preempt_prefer_on_rejects_absent_replacement_tid() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(1), TaskPriority::Normal)
            .expect("enqueue 1");
        sched.dispatch_next_on(CpuId(0)); // TID 1 current on CPU 0
        // TID 500 is not queued anywhere -> not selectable; FIFO re-picks TID 1, NOT TID 500.
        let next = sched.on_preempt_prefer_on(CpuId(0), ThreadId(500));
        assert_eq!(next, Some(ThreadId(1)));
        assert_ne!(next, Some(ThreadId(500)));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(ThreadId(1)));
    }

    // A task assigned to a DIFFERENT CPU is never selected as current on this CPU (no cross-CPU steal).
    #[test]
    fn on_preempt_prefer_on_does_not_select_wrong_cpu_task() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched.bring_up_cpu(CpuId(1)).expect("cpu1");
        sched
            .enqueue_on_with_priority(CpuId(1), ThreadId(20), TaskPriority::Normal)
            .expect("enqueue 20 on cpu1");
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(10), TaskPriority::Normal)
            .expect("enqueue 10 on cpu0");
        sched.dispatch_next_on(CpuId(0)); // TID 10 current on CPU 0
        // Prefer TID 20 (on CPU 1) while running on CPU 0: it must NOT be stolen onto CPU 0.
        let next = sched.on_preempt_prefer_on(CpuId(0), ThreadId(20));
        assert_ne!(next, Some(ThreadId(20)));
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(ThreadId(10)));
        // TID 20 is still runnable on CPU 1.
        assert_eq!(sched.dispatch_next_on(CpuId(1)), Some(ThreadId(20)));
    }

    // A duplicate runnable entry is never dispatched twice: once the preferred task is current, it is
    // not in a queue, a re-enqueue is rejected, and the next dispatch does not hand it out again.
    #[test]
    fn on_preempt_prefer_on_does_not_dispatch_duplicate_twice() {
        let mut sched_dom = new_domain();
        let mut sched = sched_dom.exclusive();
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(5), TaskPriority::Normal)
            .expect("enqueue 5");
        sched
            .enqueue_on_with_priority(CpuId(0), ThreadId(6), TaskPriority::Normal)
            .expect("enqueue 6");
        sched.dispatch_next_on(CpuId(0)); // TID 5 current
        // Prefer TID 6 -> current; TID 6 must be current EXACTLY once and not still queued.
        assert_eq!(
            sched.on_preempt_prefer_on(CpuId(0), ThreadId(6)),
            Some(ThreadId(6))
        );
        assert_eq!(sched.current_tid_on(CpuId(0)), Some(ThreadId(6)));
        // Exactly one OTHER runnable entry remains (TID 5, re-enqueued) — TID 6 is NOT duplicated in
        // the queue while it is the current task.
        assert_eq!(sched.runnable_count_on(CpuId(0)), 1);
        // A second runnable copy of the current task is rejected (single-entry invariant).
        assert_eq!(
            sched.enqueue_on_with_priority(CpuId(0), ThreadId(6), TaskPriority::Normal),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(sched.runnable_count_on(CpuId(0)), 1);
    }

    #[test]
    fn run_queue_level_remove_tid_compacts_correctly() {
        let mut q = RunQueues::new();
        for t in [10, 20, 30] {
            q.push_back(0, ThreadId(t), MAX_RUN_QUEUE).expect("push");
        }
        // Remove the middle element.
        assert!(q.remove_tid(0, ThreadId(20)));
        assert_eq!(q.len(0), 2);
        assert_eq!(q.pop_front(0), Some(ThreadId(10)));
        assert_eq!(q.pop_front(0), Some(ThreadId(30)));
        assert_eq!(q.pop_front(0), None);
        assert_eq!(q.nonempty, 0, "an emptied level leaves the bitmap");
    }

    #[test]
    fn run_queue_level_remove_tid_returns_false_when_absent() {
        let mut q = RunQueues::new();
        q.push_back(0, ThreadId(5), MAX_RUN_QUEUE).expect("5");
        assert!(!q.remove_tid(0, ThreadId(99)));
        assert!(!q.remove_tid(1, ThreadId(5)), "another level holds nothing");
        assert_eq!(q.len(0), 1);
    }

    #[test]
    fn run_queue_head_insert_and_the_level_bitmap() {
        let mut q = RunQueues::new();
        q.push_back(31, ThreadId(1), MAX_RUN_QUEUE).expect("31");
        q.push_back(4, ThreadId(2), MAX_RUN_QUEUE).expect("4");
        q.push_front(4, ThreadId(3), MAX_RUN_QUEUE).expect("4 head");
        assert_eq!(q.best_level(), Some(4));
        assert_eq!(q.levels().collect::<alloc::vec::Vec<_>>(), [4, 31]);
        assert_eq!(
            q.iter(4).collect::<alloc::vec::Vec<_>>(),
            [ThreadId(3), ThreadId(2)]
        );
        assert_eq!(q.pop_front(4), Some(ThreadId(3)));
        assert_eq!(q.pop_front(4), Some(ThreadId(2)));
        assert_eq!(q.best_level(), Some(31));
        // The pool is the bound, not any level: it holds exactly POOL_SLOTS entries.
        let mut q = RunQueues::new();
        for i in 0..POOL_SLOTS {
            let level = i % PRIORITY_LEVELS;
            q.push_back(level, ThreadId(1000 + i as u64), MAX_QUEUED_PER_PRIORITY)
                .expect("within the pool");
        }
        assert_eq!(
            q.push_back(0, ThreadId(1), MAX_QUEUED_PER_PRIORITY),
            Err(SchedulerError::QueueFull)
        );
        assert_eq!(q.total(), POOL_SLOTS);
    }

    #[test]
    fn dispatch_next_switches_away_from_tid_zero_when_real_task_is_runnable() {
        // TID 0 (boot/idle) can be current; dispatch_next switches to a real task
        // when one is runnable, and removes TID 0 from the membership table so
        // it can be re-enqueued later without hitting the AlreadyQueued guard.
        let mut sched = PriorityScheduler::default();
        // Simulate bootstrap: enqueue TID 0 then dispatch it to make it current.
        sched
            .enqueue_with_priority(ThreadId(0), TaskPriority::Normal)
            .expect("enqueue boot task");
        assert_eq!(sched.dispatch_next(), Some(ThreadId(0)));
        assert_eq!(sched.current_tid(), Some(ThreadId(0)));

        // Enqueue a real task; dispatch_next must switch to it.
        sched
            .enqueue_with_priority(ThreadId(42), TaskPriority::Normal)
            .expect("enqueue real task");
        assert_eq!(sched.dispatch_next(), Some(ThreadId(42)));
        assert_eq!(sched.current_tid(), Some(ThreadId(42)));

        // TID 0 must no longer be tracked in membership (can re-enqueue without error).
        assert!(!sched.membership_contains(ThreadId(0)));
        assert!(
            sched
                .enqueue_with_priority(ThreadId(0), TaskPriority::Normal)
                .is_ok()
        );
    }

    #[test]
    fn membership_tracking_exhausted_falls_back_to_linear_scan() {
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(10), TaskPriority::Normal)
            .expect("10");
        sched
            .enqueue_with_priority(ThreadId(20), TaskPriority::High)
            .expect("20");
        // dispatch_next picks the highest-priority task (20) as current.
        sched.dispatch_next();

        // Force the exhaustion flag; linear_contains_tid must take over.
        sched.membership_tracking_exhausted = true;

        assert!(sched.contains_tid(ThreadId(20))); // current
        assert!(sched.contains_tid(ThreadId(10))); // in Normal queue
        assert!(!sched.contains_tid(ThreadId(99))); // absent
    }

    #[test]
    fn membership_tracking_exhausted_prevents_duplicate_enqueue() {
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(5), TaskPriority::Normal)
            .expect("5");
        sched.dispatch_next(); // make TID 5 current
        sched
            .enqueue_with_priority(ThreadId(7), TaskPriority::Low)
            .expect("7");

        sched.membership_tracking_exhausted = true;

        // AlreadyQueued must still be enforced via linear scan.
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(5), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        assert_eq!(
            sched.enqueue_with_priority(ThreadId(7), TaskPriority::High),
            Err(SchedulerError::AlreadyQueued)
        );
        // A new TID must be accepted.
        assert!(
            sched
                .enqueue_with_priority(ThreadId(9), TaskPriority::Normal)
                .is_ok()
        );
    }

    #[test]
    fn membership_linear_scan_covers_current_and_all_priority_queues() {
        let mut sched = PriorityScheduler::default();
        sched
            .enqueue_with_priority(ThreadId(1), TaskPriority::High)
            .expect("1");
        sched
            .enqueue_with_priority(ThreadId(2), TaskPriority::Normal)
            .expect("2");
        sched
            .enqueue_with_priority(ThreadId(3), TaskPriority::Low)
            .expect("3");
        // dispatch_next picks TID 1 (High priority) as current.
        sched.dispatch_next();

        assert!(sched.linear_contains_tid(ThreadId(1))); // current
        assert!(sched.linear_contains_tid(ThreadId(2))); // Normal queue
        assert!(sched.linear_contains_tid(ThreadId(3))); // Low queue
        assert!(!sched.linear_contains_tid(ThreadId(99))); // absent
    }

    #[test]
    fn pass_b_scheduler_types_are_reexported_from_yarm_kernel() {
        use core::mem;

        assert_eq!(
            mem::size_of::<CpuId>(),
            mem::size_of::<yarm_kernel::scheduler::CpuId>()
        );
        assert_eq!(
            TaskPriority::High.level(),
            yarm_kernel::scheduler::TaskPriority::High.level()
        );
        let _err: yarm_kernel::scheduler::SchedulerError = SchedulerError::QueueFull;
    }
}

#[cfg(test)]
#[path = "sched_full_rot1_tests.rs"]
mod sched_full_rot1_tests;
