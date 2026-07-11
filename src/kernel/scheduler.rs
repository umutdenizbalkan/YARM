// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
use crate::arch::{platform_constants, topology};
use crate::kernel::ipc::ThreadId;
use crate::kernel::topology::CpuTopology;
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

    pub fn dispatch_next(&mut self) -> Option<ThreadId> {
        if let Some(current) = self.current {
            if current.tid.0 == 0 && self.runnable_count() > 0 {
                let next = self.dequeue_highest()?;
                self.current = Some(next);
                // Remove idle (tid=0) from the membership table so it can be
                // re-enqueued later without hitting the AlreadyQueued guard.
                if !self.membership_tracking_exhausted {
                    self.membership_remove(current.tid);
                }
                return Some(next.tid);
            }
            return Some(current.tid);
        }
        // No current task: idle state. Pick the next runnable task if any.
        let next = self.dequeue_highest()?;
        self.current = Some(next);
        Some(next.tid)
    }

    pub fn on_preempt(&mut self) -> Option<ThreadId> {
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
                return Some(running.tid);
            }
        }
        self.dispatch_next()
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
                return Some(running.tid);
            }
        }
        // Scan queues in priority order for the preferred TID.
        for priority in [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low] {
            if self.queues[Self::priority_index(priority)].remove_tid(preferred) {
                self.current = Some(ScheduledTask {
                    tid: preferred,
                    priority,
                });
                return Some(preferred);
            }
        }
        // Preferred not in any queue; fall back to normal FIFO dispatch.
        self.dispatch_next()
    }

    pub fn block_current(&mut self) -> Option<ThreadId> {
        let current = self.current.take()?;
        if !self.membership_tracking_exhausted {
            self.membership_remove(current.tid);
        }
        Some(current.tid)
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

#[derive(Debug)]
pub struct SmpScheduler {
    schedulers: [PriorityScheduler; MAX_CPUS],
    topology: CpuTopology,
    next_balance_cpu: usize,
    // Stage 183.5: CPUs that are ONLINE for accounting/wake (they idle, receive
    // IPIs, and have a scheduler-owned idle current) but do NOT yet run a
    // dispatcher, so task PLACEMENT on them is denied — `enqueue_balanced` skips
    // them and explicit enqueues are rejected. This is the intermediate
    // "wake-only online" admission state between idle-live and full dispatch;
    // 183.6+ clears the bit per CPU when its dispatch loop is wired. NOT a
    // fallback knob: no boot option touches it.
    wake_only: u64,
}

impl Default for SmpScheduler {
    fn default() -> Self {
        Self {
            schedulers: core::array::from_fn(|_| PriorityScheduler::default()),
            topology: CpuTopology::from_present_bitmap(topology::default_present_cpu_bitmap()),
            next_balance_cpu: 0,
            wake_only: 0,
        }
    }
}

impl SmpScheduler {
    fn check_cpu(cpu: CpuId) -> Result<usize, SchedulerError> {
        let idx = cpu.0 as usize;
        if idx >= MAX_CPUS {
            return Err(SchedulerError::InvalidCpu);
        }
        Ok(idx)
    }

    fn check_online_cpu(&self, cpu: CpuId) -> Result<usize, SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        if !self.topology.cpu_online(idx as u8) {
            return Err(SchedulerError::CpuOffline);
        }
        Ok(idx)
    }

    pub fn validate_online_cpu(&self, cpu: CpuId) -> Result<(), SchedulerError> {
        self.check_online_cpu(cpu).map(|_| ())
    }

    /// Stage 183.5: mark/unmark `cpu` wake-only (online for accounting/wake, no task
    /// placement — see the field doc). Idempotent.
    pub fn set_cpu_wake_only(&mut self, cpu: CpuId, wake_only: bool) -> Result<(), SchedulerError> {
        let idx = Self::check_cpu(cpu)?;
        if wake_only {
            self.wake_only |= 1u64 << idx;
        } else {
            self.wake_only &= !(1u64 << idx);
        }
        Ok(())
    }

    pub fn cpu_wake_only(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .map(|idx| self.wake_only & (1u64 << idx) != 0)
            .unwrap_or(false)
    }

    pub fn wake_only_bitmap(&self) -> u64 {
        self.wake_only
    }

    /// Stage 183.5: install the scheduler-owned IDLE current for a wake-only online
    /// AP. Uses the scheduler's existing idle convention — current = tid 0, the
    /// placeholder `dispatch_next` already knows to switch away from when real work
    /// arrives (so the representation is forward-correct for the 183.6 AP
    /// dispatcher). Only valid on an online, wake-only CPU with no current task.
    pub fn install_ap_idle_current(&mut self, cpu: CpuId) -> Result<ThreadId, SchedulerError> {
        let idx = self.check_online_cpu(cpu)?;
        if self.wake_only & (1u64 << idx) == 0 {
            return Err(SchedulerError::CpuOffline);
        }
        if self.schedulers[idx].current_tid().is_some() {
            return Err(SchedulerError::AlreadyQueued);
        }
        let idle = ScheduledTask {
            tid: ThreadId(0),
            priority: TaskPriority::Low,
        };
        self.schedulers[idx].current = Some(idle);
        Ok(idle.tid)
    }

    fn least_loaded_online_cpu(&self, start: usize) -> Result<CpuId, SchedulerError> {
        let mut best: Option<(usize, CpuId)> = None;
        for offset in 0..MAX_CPUS {
            let idx = (start + offset) % MAX_CPUS;
            // Stage 183.5: wake-only CPUs are online but accept no task placement
            // (no dispatcher runs on them yet) — never balance onto them.
            if self.wake_only & (1u64 << idx) != 0 {
                continue;
            }
            if self.topology.cpu_online(idx as u8) {
                let load = self.schedulers[idx].runnable_count()
                    + usize::from(self.schedulers[idx].current_tid().is_some());
                let cpu = CpuId(idx as u8);
                if best.map_or(true, |(best_load, _)| load < best_load) {
                    best = Some((load, cpu));
                }
            }
        }
        best.map(|(_, cpu)| cpu).ok_or(SchedulerError::CpuOffline)
    }

    /// Simulates the full secondary-CPU bring-up handshake in a single call.
    ///
    /// This is suitable for tests and single-threaded simulation. On real SMP
    /// hardware, the bootstrap CPU should call `start_secondary_cpu()`, the
    /// secondary CPU's entry point should call `acknowledge_secondary_cpu()` on
    /// itself, and only then should the bootstrap CPU call
    /// `mark_cpu_online()`.
    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), SchedulerError> {
        Self::check_cpu(cpu)?;
        self.topology
            .start_secondary_cpu(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)?;
        self.topology
            .acknowledge_secondary_cpu(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)?;
        self.topology
            .mark_cpu_online(cpu.0)
            .map_err(|_| SchedulerError::CpuOffline)
    }

    pub fn cpu_is_online(&self, cpu: CpuId) -> bool {
        Self::check_cpu(cpu)
            .ok()
            .map(|idx| self.topology.cpu_online(idx as u8))
            .unwrap_or(false)
    }

    pub fn online_cpu_bitmap(&self) -> u64 {
        self.topology.online_cpu_bitmap()
    }

    pub fn online_cpu_count(&self) -> usize {
        self.topology.online_cpu_count()
    }

    pub fn present_cpu_count(&self) -> usize {
        self.topology.present_cpu_count()
    }

    pub fn present_cpu_bitmap(&self) -> u64 {
        self.topology.present_cpu_bitmap()
    }

    /// WARNING: Resets all secondary CPU online state.
    ///
    /// Any tasks already queued on secondary CPUs remain in their per-CPU run
    /// queues, but those CPUs will appear offline until `bring_up_cpu()` is run
    /// again for each secondary.
    pub fn set_present_cpu_bitmap(&mut self, present: u64) {
        self.topology = CpuTopology::from_present_bitmap(present);
        self.next_balance_cpu = 0;
    }

    pub fn enqueue_on_with_priority(
        &mut self,
        cpu: CpuId,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        let idx = self.check_online_cpu(cpu)?;
        // Stage 183.5: a wake-only online CPU runs no dispatcher — a task placed on
        // its queue would strand forever. Deny placement explicitly (nothing pins
        // work to APs today; 183.6 lifts this per CPU when the AP dispatcher lands).
        if self.wake_only & (1u64 << idx) != 0 {
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
            return Err(SchedulerError::CpuOffline);
        }
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "ENQUEUE_QUEUE_INDEX tid={} requested_cpu={} queue_index={}",
                tid.0,
                cpu.0,
                idx
            );
        }
        self.schedulers[idx]
            .enqueue_with_priority(tid, priority)
            .map(|_| {
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!("ENQUEUE_COMMIT tid={} queue_cpu={}", tid.0, cpu.0);
                }
            })
    }

    pub fn enqueue_balanced(
        &mut self,
        tid: ThreadId,
        priority: TaskPriority,
    ) -> Result<CpuId, SchedulerError> {
        let start = self.next_balance_cpu;
        let cpu = self.least_loaded_online_cpu(start)?;
        self.enqueue_on_with_priority(cpu, tid, priority)?;
        self.next_balance_cpu = (cpu.0 as usize + 1) % MAX_CPUS;
        Ok(cpu)
    }

    pub fn enqueue_on(&mut self, cpu: CpuId, tid: ThreadId) -> Result<(), SchedulerError> {
        self.enqueue_on_with_priority(cpu, tid, TaskPriority::Normal)
    }

    pub fn dispatch_next_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        let idle_tid = self.schedulers[idx].current_tid().unwrap_or(ThreadId(0));
        let runq_len = self.schedulers[idx].runnable_count();
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "SCHED cpu={} idle_tid={} runq_len={}",
                cpu.0,
                idle_tid.0,
                runq_len
            );
        }
        let final_tid = self.schedulers[idx].dispatch_next();
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("SCHED cpu={} final_tid={:?}", cpu.0, final_tid);
        }
        final_tid
    }

    pub fn on_preempt_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].on_preempt()
    }

    /// Stage 192B: the re-enqueue half of `on_preempt_on` — re-enqueue the current task on
    /// `cpu` and clear the current slot WITHOUT dispatching. See
    /// `PriorityScheduler::preempt_reenqueue_only`.
    pub fn preempt_reenqueue_only_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].preempt_reenqueue_only()
    }

    /// Preempt current task on `cpu`, preferring `preferred` as the next task.
    /// See `PriorityScheduler::on_preempt_prefer` for semantics.
    pub fn on_preempt_prefer_on(&mut self, cpu: CpuId, preferred: ThreadId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].on_preempt_prefer(preferred)
    }

    pub fn block_current_on(&mut self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].block_current()
    }

    pub fn current_tid_on(&self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].current_tid()
    }

    pub fn current_priority_on(&self, cpu: CpuId) -> Option<TaskPriority> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].current_priority()
    }

    pub fn runnable_count_on(&self, cpu: CpuId) -> usize {
        let Ok(idx) = self.check_online_cpu(cpu) else {
            return 0;
        };
        self.schedulers[idx].runnable_count()
    }

    /// Non-mutating peek of the next-runnable dispatch candidate on `cpu`: the TID
    /// that `dispatch_next_on` would select when the current slot is idle/cleared
    /// (highest-priority queue head), WITHOUT dequeuing or setting current. Read-only.
    pub fn peek_next_runnable_on(&self, cpu: CpuId) -> Option<ThreadId> {
        let idx = self.check_online_cpu(cpu).ok()?;
        self.schedulers[idx].peek_highest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut sched = SmpScheduler::default();
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
        let mut sched = SmpScheduler::default();
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
        let mut sched = SmpScheduler::default();
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
        let mut sched = SmpScheduler::default();
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

    #[test]
    fn smp_scheduler_tracks_per_cpu_queues() {
        let mut sched = SmpScheduler::default();
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
