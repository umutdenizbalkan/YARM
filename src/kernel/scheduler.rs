// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
use crate::arch::platform_constants;
#[cfg(test)]
use crate::arch::topology;
use crate::kernel::ipc::ThreadId;
use crate::kernel::lock::{SpinLockIrq, SpinLockIrqGuard};
use crate::kernel::scheduler_timer::Timer;
use crate::kernel::time::{Tick, TickInstant};
use crate::kernel::topology::CpuTopology;
use core::sync::atomic::{AtomicU64, Ordering};
pub use yarm_kernel::scheduler::{CpuId, SchedulerError, TaskPriority};

pub const MAX_RUN_QUEUE: usize = 64;
pub const MAX_CPUS: usize = platform_constants::MAX_CPUS;
const _: () = assert!(MAX_RUN_QUEUE.is_power_of_two());

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingQueue {
    tids: [ThreadId; MAX_RUN_QUEUE],
    head: usize,
    len: usize,
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

impl RingQueue {
    const fn new() -> Self {
        Self {
            tids: [ThreadId(0); MAX_RUN_QUEUE],
            head: 0,
            len: 0,
        }
    }

    fn index(offset: usize) -> usize {
        offset & (MAX_RUN_QUEUE - 1)
    }

    fn contains(&self, tid: ThreadId) -> bool {
        for offset in 0..self.len {
            let idx = Self::index(self.head + offset);
            if self.tids[idx] == tid {
                return true;
            }
        }
        false
    }

    fn push(&mut self, tid: ThreadId) -> Result<(), SchedulerError> {
        if self.len >= MAX_RUN_QUEUE {
            return Err(SchedulerError::QueueFull);
        }
        let tail = Self::index(self.head + self.len);
        self.tids[tail] = tid;
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<ThreadId> {
        if self.len == 0 {
            return None;
        }
        let tid = self.tids[self.head];
        self.head = Self::index(self.head + 1);
        self.len -= 1;
        Some(tid)
    }

    /// Non-mutating peek of the head (the TID `pop` would return next). Read-only.
    fn peek(&self) -> Option<ThreadId> {
        if self.len == 0 {
            return None;
        }
        Some(self.tids[self.head])
    }

    /// Number of live slots holding `tid`. A scan only — it mutates nothing and shares
    /// `Self::index` with the rest of the ring, so no queue algorithm is duplicated.
    fn count_tid(&self, tid: ThreadId) -> usize {
        let mut seen = 0;
        for offset in 0..self.len {
            let idx = Self::index(self.head + offset);
            if self.tids[idx] == tid {
                seen += 1;
            }
        }
        seen
    }

    /// Remove `tid` from any position in the ring buffer.
    /// Compacts the elements after the removed slot toward the head.
    /// Returns `true` if `tid` was found and removed, `false` otherwise.
    fn remove_tid(&mut self, tid: ThreadId) -> bool {
        for i in 0..self.len {
            let idx = Self::index(self.head + i);
            if self.tids[idx] == tid {
                for j in i..self.len - 1 {
                    let dst = Self::index(self.head + j);
                    let src = Self::index(self.head + j + 1);
                    self.tids[dst] = self.tids[src];
                }
                self.len -= 1;
                return true;
            }
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
    queues: [RingQueue; 3],
    current: Option<ScheduledTask>,
    membership_keys: [ThreadId; MEMBERSHIP_SLOTS],
    membership_state: [u8; MEMBERSHIP_SLOTS],
    membership_tracking_exhausted: bool,
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            queues: [RingQueue::new(), RingQueue::new(), RingQueue::new()],
            current: None,
            membership_keys: [ThreadId(0); MEMBERSHIP_SLOTS],
            membership_state: [MEMBERSHIP_EMPTY; MEMBERSHIP_SLOTS],
            membership_tracking_exhausted: false,
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
        self.queues.iter().any(|queue| queue.contains(tid))
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

        for queue_idx in 0..self.queues.len() {
            let queue_len = self.queues[queue_idx].len;
            for offset in 0..queue_len {
                let idx = RingQueue::index(self.queues[queue_idx].head + offset);
                let tid = self.queues[queue_idx].tids[idx];
                if self.membership_insert(tid).is_err() {
                    exhausted = true;
                }
            }
        }

        exhausted
    }

    fn priority_index(priority: TaskPriority) -> usize {
        priority as usize
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
        self.queues[Self::priority_index(priority)].push(tid)?;
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        Ok(())
    }

    fn dequeue_highest(&mut self) -> Option<ScheduledTask> {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if let Some(tid) = self.queues[Self::priority_index(priority)].pop() {
                return Some(ScheduledTask { tid, priority });
            }
        }
        None
    }

    /// Non-mutating twin of [`dequeue_highest`]: return the TID that would be
    /// dispatched next from an idle/cleared current (highest-priority queue head),
    /// WITHOUT dequeuing it. Read-only — same priority scan order as `dequeue_highest`.
    fn peek_highest(&self) -> Option<ThreadId> {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if let Some(tid) = self.queues[Self::priority_index(priority)].peek() {
                return Some(tid);
            }
        }
        None
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
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            let qi = Self::priority_index(priority);
            // Snapshot the entries in FIFO order first: `accept` may take another lock, so it must
            // not be called while indexing a queue this loop is about to mutate.
            let mut candidates = [ThreadId(0); MAX_RUN_QUEUE];
            let len = self.queues[qi].len;
            for (i, slot) in candidates.iter_mut().enumerate().take(len) {
                *slot = self.queues[qi].tids[RingQueue::index(self.queues[qi].head + i)];
            }
            for &tid in candidates.iter().take(len) {
                examined += 1;
                if !accept(tid) {
                    continue;
                }
                if !self.queues[qi].remove_tid(tid) {
                    // The entry vanished between the snapshot and the removal. Nothing was
                    // mutated for it; keep scanning rather than claiming a dequeue.
                    continue;
                }
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
            if let Err(err) = self.enqueue_with_priority(running.tid, running.priority) {
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
        if let Err(_err) = self.enqueue_with_priority(running.tid, running.priority) {
            // Re-enqueue failed — restore as current and signal the caller to fall back.
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
            if let Err(err) = self.enqueue_with_priority(running.tid, running.priority) {
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
        // Scan queues in priority order for the preferred TID.
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if self.queues[Self::priority_index(priority)].remove_tid(preferred) {
                self.current = Some(ScheduledTask {
                    tid: preferred,
                    priority,
                });
                return DispatchSelection::Dequeued { tid: preferred };
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
    /// `RingQueue::remove_tid`: the one existing caller (`on_preempt_prefer`) moves the task from
    /// a queue to `current`, so the task stays present in this scheduler and membership must NOT
    /// change. Withdrawal removes it from the scheduler entirely, so membership must be cleared
    /// or a later `enqueue_with_priority` would refuse the TID as already queued.
    pub(crate) fn withdraw_queued_tid(&mut self, tid: ThreadId) -> WithdrawOutcome {
        // (3)(4) current-task protection, before any mutation. This also protects the
        // scheduler-owned tid-0 idle current installed on a wake-only CPU.
        if self.current.is_some_and(|task| task.tid == tid) {
            return WithdrawOutcome::RefusedCurrent;
        }
        // (5) exact-one rule: count first across every priority queue, mutate only if the count
        // is exactly 1. A duplicate fails closed with zero mutation.
        let mut total = 0usize;
        for queue in self.queues.iter() {
            total += queue.count_tid(tid);
        }
        match total {
            0 => return WithdrawOutcome::NotQueued,
            1 => {}
            _ => return WithdrawOutcome::RefusedDuplicate,
        }
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if self.queues[Self::priority_index(priority)].remove_tid(tid) {
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
        self.queues.iter().map(|queue| queue.len).sum()
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
}

impl CpuSched {
    fn new(quantum_ticks: u64) -> Self {
        Self {
            rq: PriorityScheduler::default(),
            quantum: Timer::new(quantum_ticks),
        }
    }

    fn load(&self) -> usize {
        self.rq.runnable_count() + usize::from(self.rq.current.is_some())
    }

    /// SCHED-LOCK1 witness only: the current slot, read-only.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_current(&self) -> Option<ThreadId> {
        self.rq.current_tid()
    }

    /// SCHED-LOCK1 witness only: every queued entry, read-only, in dispatch order.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_for_each_queued(&self, mut f: impl FnMut(TaskPriority, ThreadId)) {
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            let q = &self.rq.queues[PriorityScheduler::priority_index(priority)];
            for offset in 0..q.len {
                f(priority, q.tids[RingQueue::index(q.head + offset)]);
            }
        }
    }

    /// Where `tid` is on THIS CPU: (current here, queued here). `current` is checked separately
    /// from the queues because the current slot is the authority on what is executing.
    fn placement_here(&self, tid: ThreadId) -> (bool, bool) {
        (
            self.rq.current.is_some_and(|task| task.tid == tid),
            self.rq.queues.iter().any(|q| q.count_tid(tid) > 0),
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
    lock: SpinLockIrq<CpuSched>,
    mirror: CpuMirror,
}

impl CpuCell {
    fn new(quantum_ticks: u64) -> Self {
        Self {
            lock: SpinLockIrq::new(CpuSched::new(quantum_ticks)),
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

/// SCHED-LOCK1 — the published admission bitmaps. Written only under 1G **and** the affected
/// CPU's 1L (so a holder of 1L[c] sees `c`'s bits stable); read lock-free with Acquire.
#[derive(Debug, Default)]
pub(crate) struct SchedTopo {
    present: AtomicU64,
    online: AtomicU64,
    wake_only: AtomicU64,
    balance_excluded: AtomicU64,
}

impl SchedTopo {
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
    admission: SpinLockIrq<Admission>,
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
    pub fn new(present: u64, quantum_ticks: u64) -> Self {
        let cells: [CpuCell; MAX_CPUS] = core::array::from_fn(|_| CpuCell::new(quantum_ticks));
        #[cfg(feature = "hosted-dev")]
        let cells = alloc::boxed::Box::new(cells);
        let topology = CpuTopology::from_present_bitmap(present);
        let topo = SchedTopo::default();
        topo.publish_topology(&topology);
        Self {
            cpus: cells,
            admission: SpinLockIrq::new(Admission {
                topology,
                next_balance_cpu: 0,
            }),
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

    /// SCHED-LOCK1 witness only: CPU `idx`'s cell and its lock, as addresses — the authoritative
    /// CPU → lock-instance map the witness records under the coherent view.
    #[cfg(feature = "sched-lock1-witness")]
    pub(crate) fn witness_cell_addrs(&self, idx: usize) -> (usize, usize) {
        let cell = &self.cpus[idx];
        (
            cell as *const CpuCell as usize,
            &cell.lock as *const SpinLockIrq<CpuSched> as usize,
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

    /// CPU `idx`'s published load (queued + current).
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
            guard,
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
        let mut guards: [Option<SpinLockIrqGuard<'_, CpuSched>>; MAX_CPUS] =
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
        for (idx, cell) in self.cpus.iter_mut().enumerate() {
            let CpuCell { lock, mirror } = cell;
            cpus[idx] = Some(lock.get_mut());
            mirrors[idx] = Some(&*mirror);
        }
        SchedAll {
            cpus: cpus.map(|c| c.expect("every cell")),
            mirrors: mirrors.map(|m| m.expect("every cell")),
            admission: self.admission.get_mut(),
            topo: &self.topo,
            clock: &self.clock,
        }
    }
}

/// SCHED-LOCK1 — a held 1L on one CPU. Projects ONLY that CPU's [`CpuSched`]; republishes the
/// CPU's mirror when released (before the lock itself is released).
pub struct CpuSchedGuard<'a> {
    guard: SpinLockIrqGuard<'a, CpuSched>,
    cell: &'a CpuCell,
    topo: &'a SchedTopo,
    clock: &'a AtomicU64,
    idx: usize,
}

impl CpuSchedGuard<'_> {
    pub fn view(&mut self) -> CpuView<'_> {
        CpuView {
            idx: self.idx,
            sched: &mut self.guard,
            topo: self.topo,
            clock: self.clock,
            #[cfg(feature = "sched-lock1-witness")]
            cell: Some(self.cell),
        }
    }
}

impl Drop for CpuSchedGuard<'_> {
    fn drop(&mut self) {
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::before_release(
            self.idx,
            self.cell as *const CpuCell as usize,
            &self.guard,
        );
        publish(&self.cell.mirror, &self.guard);
        // `guard` is dropped after this body: the mirror is published while 1L is still held.
    }
}

/// SCHED-LOCK1 — 1G and one CPU's 1L, for that CPU's admission transition.
pub(crate) struct CpuAdmissionGuard<'a> {
    // Field order is drop order: the 1L guard is released before 1G.
    cpu: CpuSchedGuard<'a>,
    admission: SpinLockIrqGuard<'a, Admission>,
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
            crate::yarm_log!(
                "SCHED_ENQUEUE_DENIED_WAKE_ONLY cpu={} tid={} reason=no_ap_dispatcher_yet",
                cpu.0,
                tid.0
            );
            // Stage 195D (BSP DISPATCH AFFINITY): an actual prevented placement of a runnable
            // user task onto a wake-only AP queue. Emitted ONLY on a real rejection (never
            // fabricated); the caller reroutes to the BSP dispatcher.
            #[cfg(target_arch = "aarch64")]
            crate::yarm_log!(
                "AARCH64_WAKE_ONLY_AP_QUEUE_REJECTED tid={} cpu={}",
                tid.0,
                cpu.0
            );
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

    /// Visit every queued entry in dispatch order (High → Normal → Low, FIFO within each).
    #[cfg(any(test, feature = "hosted-dev"))]
    pub fn for_each_queued(&self, mut f: impl FnMut(TaskPriority, ThreadId)) {
        if !self.online() {
            return;
        }
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            let q = &self.sched.rq.queues[PriorityScheduler::priority_index(priority)];
            for offset in 0..q.len {
                f(priority, q.tids[RingQueue::index(q.head + offset)]);
            }
        }
    }

    /// One timer interrupt on this CPU: the clock owner advances the scheduler tick; every CPU
    /// consumes only its own quantum. Returns `(scheduler tick, this CPU's quantum expired)`.
    pub fn tick(&mut self) -> (Tick, bool) {
        let (_, expired) = self.sched.quantum.tick_and_check();
        let now = if self.idx == CLOCK_OWNER_CPU.0 as usize {
            self.clock.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
        } else {
            self.clock.load(Ordering::Acquire)
        };
        (TickInstant(now), expired)
    }

    /// Would this CPU's NEXT timer interrupt expire its quantum?
    pub fn would_preempt_next(&self) -> bool {
        self.sched.quantum.would_preempt_next()
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
    guards: [Option<SpinLockIrqGuard<'a, CpuSched>>; MAX_CPUS],
    admission: Option<SpinLockIrqGuard<'a, Admission>>,
}

impl SchedAllGuard<'_> {
    pub fn view(&mut self) -> SchedAll<'_> {
        let domain = self.domain;
        let cpus = self.guards.each_mut().map(|g| {
            &mut **g
                .as_mut()
                .expect("every 1L is held for the guard's lifetime")
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
        for idx in (0..MAX_CPUS).rev() {
            drop(self.guards[idx].take());
        }
        drop(self.admission.take());
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
}

impl Drop for SchedAll<'_> {
    fn drop(&mut self) {
        for idx in 0..MAX_CPUS {
            publish(self.mirrors[idx], self.cpus[idx]);
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
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            let q = &sched.rq.queues[PriorityScheduler::priority_index(priority)];
            for offset in 0..q.len {
                f(priority, q.tids[RingQueue::index(q.head + offset)]);
            }
        }
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
    fn withdraw_compacts_a_wrapped_ring_queue() {
        let mut q = RingQueue::new();
        // Wrap the ring: fill most of it, drain most of that, then refill past the end.
        for tid in 1..=60u64 {
            q.push(ThreadId(tid)).expect("push");
        }
        for _ in 0..56 {
            q.pop().expect("pop");
        }
        for tid in 61..=66u64 {
            q.push(ThreadId(tid)).expect("push past the wrap");
        }
        assert!(
            q.head + q.len > MAX_RUN_QUEUE,
            "the ring must be wrapped (head={}, len={})",
            q.head,
            q.len
        );
        let before = q.len;
        // TID 61 sits at the last physical slot; its successors live past the wrap.
        assert!(q.remove_tid(ThreadId(61)));
        assert_eq!(q.len, before - 1);
        let mut order = alloc::vec::Vec::new();
        while let Some(tid) = q.pop() {
            order.push(tid.0);
        }
        assert_eq!(
            order,
            alloc::vec![57, 58, 59, 60, 62, 63, 64, 65, 66],
            "wrapped compaction preserves order"
        );
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
        sched.cpus[0].rq.queues[PriorityScheduler::priority_index(TaskPriority::High)]
            .push(ThreadId(3))
            .expect("push");
        sched.cpus[0].rq.queues[PriorityScheduler::priority_index(TaskPriority::Normal)]
            .push(ThreadId(3))
            .expect("push");
        sched.cpus[0].rq.queues[PriorityScheduler::priority_index(TaskPriority::Normal)]
            .push(ThreadId(8))
            .expect("push");
        let before = sched.runnable_count_on(CpuId(0));
        assert_eq!(
            sched.withdraw_queued_tid_on(CpuId(0), ThreadId(3)),
            WithdrawOutcome::RefusedDuplicate
        );
        assert_eq!(sched.runnable_count_on(CpuId(0)), before, "zero mutation");
        assert!(
            sched.cpus[0].rq.queues[PriorityScheduler::priority_index(TaskPriority::High)]
                .contains(ThreadId(3))
        );
        assert!(
            sched.cpus[0].rq.queues[PriorityScheduler::priority_index(TaskPriority::Normal)]
                .contains(ThreadId(3))
        );
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
    fn ring_queue_remove_tid_compacts_correctly() {
        let mut q = RingQueue::new();
        q.push(ThreadId(10)).expect("10");
        q.push(ThreadId(20)).expect("20");
        q.push(ThreadId(30)).expect("30");

        // Remove the middle element.
        assert!(q.remove_tid(ThreadId(20)));
        assert_eq!(q.len, 2);
        assert_eq!(q.pop(), Some(ThreadId(10)));
        assert_eq!(q.pop(), Some(ThreadId(30)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn ring_queue_remove_tid_returns_false_when_absent() {
        let mut q = RingQueue::new();
        q.push(ThreadId(5)).expect("5");
        assert!(!q.remove_tid(ThreadId(99)));
        assert_eq!(q.len, 1);
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
            TaskPriority::High as u8,
            yarm_kernel::scheduler::TaskPriority::High as u8
        );
        let _err: yarm_kernel::scheduler::SchedulerError = SchedulerError::QueueFull;
    }
}
