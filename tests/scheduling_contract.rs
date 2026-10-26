// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-CONTRACT1 — the executable checks behind `doc/SCHEDULING_CONTRACT.md`.
//!
//! Three kinds of evidence live here, and they are deliberately kept apart because they prove
//! different things:
//!
//! * [`production`] drives the REAL `PriorityScheduler` / `SmpScheduler` — the owners every
//!   production dispatch reaches. These tests are enforcement evidence, and only for requirements
//!   the contract labels **ENFORCED**. Each names the requirement it pins.
//! * [`model`] is a small, test-only executable policy model of the FUTURE first real-time profile
//!   (RT-P1). It is **specification evidence only**: it is not compiled into the kernel, nothing in
//!   production calls it, and a passing model test says nothing about what production enforces.
//!   It exists so the contract's rules are unambiguous enough to execute, and so their important
//!   predicates are mutation-checked ([`model::every_mutation_is_killed_by_its_targeted_check`]).
//!   It is not a second scheduler: it has no dispatcher, no context, no lock and no caller.
//! * [`sc_gp_1_general_purpose_model_agrees_with_production_priority_scheduler`] is the one
//!   bridge: the model, configured as the general-purpose profile, must make exactly the decisions
//!   the production `PriorityScheduler` makes over random operation sequences. That is what
//!   licenses the contract's claim that the general-purpose profile is described by the same
//!   mechanisms as RT-P1 — for the general-purpose configuration, and for nothing more.

use yarm::kernel::ipc::ThreadId;
use yarm::kernel::scheduler::{
    AcceptedSelection, CpuId, DispatchSelection, MAX_RUN_QUEUE, PriorityScheduler, SchedulerError,
    SmpScheduler, TaskPriority,
};

/// Evidence for requirements the contract labels ENFORCED, against the production owners.
mod production {
    use super::*;

    fn t(n: u64) -> ThreadId {
        ThreadId(n)
    }

    /// Dispatch from an empty `current` until idle, blocking each selected task, and return the
    /// selection order. This is exactly the order the production drains observe: every drain
    /// selects only after `current` was cleared.
    fn drain_order(s: &mut PriorityScheduler) -> Vec<u64> {
        let mut out = Vec::new();
        loop {
            match s.dispatch_next_selection() {
                DispatchSelection::Dequeued { tid } => {
                    out.push(tid.0);
                    assert_eq!(s.block_current(), Some(tid));
                }
                DispatchSelection::Idle => return out,
                DispatchSelection::ContinuedCurrent { tid } => {
                    panic!("current {tid:?} survived block_current")
                }
            }
        }
    }

    /// SC-PRI-1 (ENFORCED): selection is strict priority — every High before any Normal before any
    /// Low — and FIFO within a level.
    #[test]
    fn sc_pri_1_strict_priority_order_high_normal_low() {
        let mut s = PriorityScheduler::default();
        s.enqueue_with_priority(t(30), TaskPriority::Low).unwrap();
        s.enqueue_with_priority(t(20), TaskPriority::Normal)
            .unwrap();
        s.enqueue_with_priority(t(10), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(31), TaskPriority::Low).unwrap();
        s.enqueue_with_priority(t(11), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(21), TaskPriority::Normal)
            .unwrap();
        assert_eq!(drain_order(&mut s), vec![10, 11, 20, 21, 30, 31]);
    }

    /// SC-PRI-2 (ENFORCED): equal priority is FIFO in enqueue order, and the order is a function
    /// of the operation sequence alone — repeated runs agree exactly.
    #[test]
    fn sc_pri_2_equal_priority_is_fifo_and_deterministic() {
        let run = || {
            let mut s = PriorityScheduler::default();
            for n in [5, 3, 9, 1, 7] {
                s.enqueue_with_priority(t(n), TaskPriority::Normal).unwrap();
            }
            drain_order(&mut s)
        };
        let first = run();
        assert_eq!(first, vec![5, 3, 9, 1, 7]);
        for _ in 0..4 {
            assert_eq!(run(), first);
        }
    }

    /// SC-PRI-3 (ENFORCED): a quantum preemption returns the running task to the TAIL of its OWN
    /// priority level — round-robin among equals, and never a demotion: a preempted High task is
    /// reselected ahead of every queued Normal task.
    #[test]
    fn sc_pri_3_quantum_preemption_requeues_at_tail_of_own_priority() {
        let mut s = PriorityScheduler::default();
        for n in [1, 2, 3] {
            s.enqueue_with_priority(t(n), TaskPriority::Normal).unwrap();
        }
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::Dequeued { tid: t(1) }
        );
        assert_eq!(
            s.on_preempt_selection(),
            DispatchSelection::Dequeued { tid: t(2) }
        );
        assert_eq!(s.block_current(), Some(t(2)));
        assert_eq!(drain_order(&mut s), vec![3, 1], "1 went to the tail");

        let mut s = PriorityScheduler::default();
        s.enqueue_with_priority(t(8), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(9), TaskPriority::Normal).unwrap();
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::Dequeued { tid: t(8) }
        );
        assert_eq!(
            s.on_preempt_selection(),
            DispatchSelection::Dequeued { tid: t(8) },
            "a quantum preemption never demotes"
        );
        assert_eq!(s.current_priority(), Some(TaskPriority::High));
    }

    /// SC-PRI-4 (ENFORCED for the general-purpose profile): a wake appends at the tail of the
    /// woken task's level and NEVER displaces the running task, whatever their priorities. A
    /// higher-priority wake is selected only at the next scheduling point (here a quantum
    /// preemption). This is the baseline RT-SCHED1 must change, and the contract says so.
    #[test]
    fn sc_pri_4_wake_appends_at_tail_and_never_displaces_running_task() {
        let mut s = PriorityScheduler::default();
        s.enqueue_with_priority(t(7), TaskPriority::Low).unwrap();
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::Dequeued { tid: t(7) }
        );
        // A High wake while Low runs.
        s.enqueue_with_priority(t(8), TaskPriority::High).unwrap();
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::ContinuedCurrent { tid: t(7) }
        );
        assert_eq!(s.current_tid(), Some(t(7)));
        // Only a scheduling point selects it.
        assert_eq!(
            s.on_preempt_selection(),
            DispatchSelection::Dequeued { tid: t(8) }
        );

        // Block, then wake: the woken task joins the tail of its level.
        let mut s = PriorityScheduler::default();
        for n in [1, 2] {
            s.enqueue_with_priority(t(n), TaskPriority::Normal).unwrap();
        }
        assert_eq!(
            s.dispatch_next_selection(),
            DispatchSelection::Dequeued { tid: t(1) }
        );
        assert_eq!(s.block_current(), Some(t(1)));
        s.enqueue_with_priority(t(1), TaskPriority::Normal).unwrap();
        assert_eq!(drain_order(&mut s), vec![2, 1]);
    }

    /// SC-CPU-1 (ENFORCED at the scheduler): a placement is confined to the CPU it names. It
    /// mutates no other CPU's queue, never displaces the target's running task, and leaves an idle
    /// CPU idle.
    #[test]
    fn sc_cpu_1_enqueue_is_confined_to_the_named_cpu() {
        let mut s = SmpScheduler::default();
        s.set_present_cpu_bitmap(0b11);
        s.bring_up_cpu(CpuId(1)).expect("cpu1 online");
        s.enqueue_on_with_priority(CpuId(1), t(5), TaskPriority::Normal)
            .unwrap();
        assert_eq!(
            s.dispatch_next_selection_on(CpuId(1)),
            DispatchSelection::Dequeued { tid: t(5) }
        );
        // A remote wake for CPU 1, higher priority than what CPU 1 runs.
        s.enqueue_on_with_priority(CpuId(1), t(6), TaskPriority::High)
            .unwrap();
        assert_eq!(s.current_tid_on(CpuId(1)), Some(t(5)));
        assert_eq!(s.peek_next_runnable_on(CpuId(1)), Some(t(6)));
        assert_eq!(s.runnable_count_on(CpuId(0)), 0);
        assert_eq!(
            s.dispatch_next_selection_on(CpuId(0)),
            DispatchSelection::Idle
        );
        assert!(!s.tid_present_anywhere(t(99)));
    }

    /// SC-POOL-1 (ENFORCED): each per-CPU priority level is a fixed `MAX_RUN_QUEUE`-entry ring;
    /// overflow is the explicit `QueueFull`, it mutates nothing, and it is per level.
    #[test]
    fn sc_pool_1_run_queue_exhaustion_is_explicit_and_atomic() {
        let mut s = PriorityScheduler::default();
        for n in 0..MAX_RUN_QUEUE as u64 {
            s.enqueue_with_priority(t(1000 + n), TaskPriority::Normal)
                .unwrap();
        }
        assert_eq!(
            s.enqueue_with_priority(t(5000), TaskPriority::Normal),
            Err(SchedulerError::QueueFull)
        );
        assert_eq!(s.runnable_count(), MAX_RUN_QUEUE);
        assert!(
            !s.contains_or_current(t(5000)),
            "a refused enqueue left no trace"
        );
        // The capacity is per level: another level still accepts.
        s.enqueue_with_priority(t(6000), TaskPriority::High)
            .unwrap();
        assert_eq!(s.runnable_count(), MAX_RUN_QUEUE + 1);
        let first: Vec<u64> = drain_order(&mut s).into_iter().take(2).collect();
        assert_eq!(first, vec![6000, 1000], "nothing reordered by the refusal");
    }

    /// SC-ELIG-1 (ENFORCED): the production selection is ACCEPTANCE-FILTERED. A candidate the
    /// caller's predicate refuses is skipped IN PLACE — a lower-priority acceptable task runs, the
    /// refused one keeps its exact position, and "nothing acceptable" is a measured count. This
    /// is the existing hook a future partition/budget eligibility predicate attaches to.
    #[test]
    fn sc_elig_1_acceptance_filter_skips_ineligible_highest_in_place() {
        let mut s = PriorityScheduler::default();
        s.enqueue_with_priority(t(1), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(2), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(3), TaskPriority::Normal).unwrap();
        assert_eq!(
            s.dispatch_next_accepted_selection(|tid| tid != t(1)),
            AcceptedSelection::Selected(DispatchSelection::Dequeued { tid: t(2) })
        );
        assert_eq!(s.block_current(), Some(t(2)));
        assert_eq!(drain_order(&mut s), vec![1, 3], "1 kept the head of High");

        let mut s = PriorityScheduler::default();
        s.enqueue_with_priority(t(1), TaskPriority::High).unwrap();
        s.enqueue_with_priority(t(3), TaskPriority::Low).unwrap();
        assert_eq!(
            s.dispatch_next_accepted_selection(|_| false),
            AcceptedSelection::NoneAcceptable { examined: 2 }
        );
        assert_eq!(s.current_tid(), None);
        assert_eq!(s.runnable_count(), 2, "nothing was dequeued");
    }
}

/// Executable policy model of RT-P1. SPECIFICATION EVIDENCE ONLY — see the module docs above.
mod model {
    use std::collections::{BTreeMap, VecDeque};

    pub type Tick = u64;
    /// RT-P1 fixed priority levels, 0 highest (SC-PRI-10).
    pub const PRIORITY_LEVELS: usize = 32;
    /// RT-P1 bound on configured windows (SC-POOL-2).
    pub const MAX_WINDOWS: usize = 64;

    /// A deliberate defect in one of the model's predicates, used only by
    /// [`every_mutation_is_killed_by_its_targeted_check`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mutation {
        IgnoreWindow,
        IgnoreBudget,
        LifoEqualPriority,
        HandoffBypassesEligibility,
        RemoteWakeMovesCpu,
        RemoteWakeRefillsBudget,
        BoundaryWaitsForActivity,
        AllowOverlap,
        WrappingWindowArithmetic,
        EssentialWorkUncharged,
        NoOverrunDebt,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Window {
        pub cpu: usize,
        pub partition: usize,
        pub offset: Tick,
        pub duration: Tick,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ScheduleError {
        ZeroMajorFrame,
        NoWindows,
        TooManyWindows,
        EpochOverflow,
        UnknownCpu { window: usize },
        UnknownPartition { window: usize },
        ZeroDuration { window: usize },
        Overflow { window: usize },
        BeyondMajorFrame { window: usize },
        Overlap { a: usize, b: usize },
        PartitionSpansCpus { partition: usize },
    }

    /// SC-PART: a validated cyclic schedule. Windows are half-open `[offset, offset+duration)`
    /// within one major frame; the frame repeats from `epoch`.
    #[derive(Clone, Debug)]
    pub struct Schedule {
        pub epoch: Tick,
        pub major_frame: Tick,
        pub windows: Vec<Window>,
    }

    impl Schedule {
        pub fn new(
            epoch: Tick,
            major_frame: Tick,
            windows: &[Window],
            n_cpus: usize,
            n_partitions: usize,
            mutation: Option<Mutation>,
        ) -> Result<Self, ScheduleError> {
            use ScheduleError as E;
            if major_frame == 0 {
                return Err(E::ZeroMajorFrame);
            }
            if windows.is_empty() {
                return Err(E::NoWindows);
            }
            if windows.len() > MAX_WINDOWS {
                return Err(E::TooManyWindows);
            }
            if epoch.checked_add(major_frame).is_none() {
                return Err(E::EpochOverflow);
            }
            let mut ends = Vec::with_capacity(windows.len());
            for (i, w) in windows.iter().enumerate() {
                if w.cpu >= n_cpus {
                    return Err(E::UnknownCpu { window: i });
                }
                if w.partition >= n_partitions {
                    return Err(E::UnknownPartition { window: i });
                }
                if w.duration == 0 {
                    return Err(E::ZeroDuration { window: i });
                }
                let end = if mutation == Some(Mutation::WrappingWindowArithmetic) {
                    w.offset.wrapping_add(w.duration)
                } else {
                    w.offset
                        .checked_add(w.duration)
                        .ok_or(E::Overflow { window: i })?
                };
                if end > major_frame {
                    return Err(E::BeyondMajorFrame { window: i });
                }
                ends.push(end);
            }
            if mutation != Some(Mutation::AllowOverlap) {
                for a in 0..windows.len() {
                    for b in (a + 1)..windows.len() {
                        let (wa, wb) = (windows[a], windows[b]);
                        if wa.cpu == wb.cpu && wa.offset < ends[b] && wb.offset < ends[a] {
                            return Err(E::Overlap { a, b });
                        }
                    }
                }
            }
            // SC-PART-4: in RT-P1 a partition is bound to exactly one CPU.
            for p in 0..n_partitions {
                let mut cpu = None;
                for w in windows.iter().filter(|w| w.partition == p) {
                    match cpu {
                        None => cpu = Some(w.cpu),
                        Some(c) if c != w.cpu => {
                            return Err(E::PartitionSpansCpus { partition: p });
                        }
                        Some(_) => {}
                    }
                }
            }
            Ok(Self {
                epoch,
                major_frame,
                windows: windows.to_vec(),
            })
        }

        /// The partition whose window covers `now` on `cpu`: a pure function of time, so a
        /// boundary takes effect whether or not anything ran (SC-PART-6). `None` before the epoch
        /// and in a gap.
        pub fn active_partition(&self, cpu: usize, now: Tick) -> Option<usize> {
            let phase = now.checked_sub(self.epoch)? % self.major_frame;
            self.windows
                .iter()
                .find(|w| w.cpu == cpu && w.offset <= phase && phase < w.offset + w.duration)
                .map(|w| w.partition)
        }

        /// The index of the major frame containing `now`.
        pub fn frame_index(&self, now: Tick) -> Option<Tick> {
            Some(now.checked_sub(self.epoch)? / self.major_frame)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Budget {
        /// General-purpose profile only: user execution is not budget-limited.
        Unlimited,
        /// RT-P1: `budget` ticks per `period`, replenished at fixed release instants.
        Limited { budget: Tick, period: Tick },
    }

    /// SC-CTX: a scheduling context, bound to one CPU, one partition and one budget owner.
    #[derive(Clone, Debug)]
    pub struct Context {
        pub cpu: usize,
        pub partition: usize,
        pub priority: usize,
        pub budget: Budget,
        pub remaining: Tick,
        pub next_release: Tick,
        pub debt: Tick,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Blocked,
        Ready,
        Running,
    }

    /// SC-EXEC: who pays for a piece of kernel execution.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum KernelWork {
        /// Synchronous kernel work in the caller's own trap: charged to the caller.
        CallerSynchronous(usize),
        /// Essential, non-deferrable work (interrupt top halves, remote requests, the scheduler
        /// itself): charged to the CPU's reserved system allowance; any excess is an overrun.
        Essential,
        /// Deferrable work run by an independent kernel worker: charged to the worker's context.
        Worker(usize),
    }

    pub struct Model {
        pub schedule: Schedule,
        /// One thread per scheduling context in RT-P1; the thread id is the context index.
        pub ctx: Vec<Context>,
        pub state: Vec<State>,
        queues: BTreeMap<(usize, usize, usize), VecDeque<usize>>,
        pub running: Vec<Option<usize>>,
        pub now: Tick,
        /// RT-P1: a wake is a scheduling point. General-purpose: it is not (SC-PRI-4).
        pub preempt_on_wake: bool,
        /// RT-P1: a block reselects at once. The differential test drives selection explicitly.
        pub resched_on_block: bool,
        pub allowance: Tick,
        pub allowance_used: Vec<Tick>,
        allowance_frame: Vec<Option<Tick>>,
        pub overrun: Vec<Tick>,
        pub essential_total: Vec<Tick>,
        mutation: Option<Mutation>,
    }

    impl Model {
        pub fn new(
            schedule: Schedule,
            contexts: &[(usize, usize, usize, Budget)],
            n_cpus: usize,
            allowance: Tick,
            mutation: Option<Mutation>,
        ) -> Self {
            let ctx = contexts
                .iter()
                .map(|&(cpu, partition, priority, budget)| {
                    assert!(priority < PRIORITY_LEVELS);
                    let (remaining, next_release) = match budget {
                        Budget::Unlimited => (0, Tick::MAX),
                        Budget::Limited { budget, period } => {
                            assert!(budget <= period && period > 0);
                            (budget, schedule.epoch + period)
                        }
                    };
                    Context {
                        cpu,
                        partition,
                        priority,
                        budget,
                        remaining,
                        next_release,
                        debt: 0,
                    }
                })
                .collect::<Vec<_>>();
            let n = ctx.len();
            Self {
                now: schedule.epoch,
                schedule,
                ctx,
                state: vec![State::Blocked; n],
                queues: BTreeMap::new(),
                running: vec![None; n_cpus],
                preempt_on_wake: true,
                resched_on_block: true,
                allowance,
                allowance_used: vec![0; n_cpus],
                allowance_frame: vec![None; n_cpus],
                overrun: vec![0; n_cpus],
                essential_total: vec![0; n_cpus],
                mutation,
            }
        }

        fn mutated(&self, m: Mutation) -> bool {
            self.mutation == Some(m)
        }

        fn key(&self, t: usize) -> (usize, usize, usize) {
            let c = &self.ctx[t];
            (c.cpu, c.priority, c.partition)
        }

        pub fn budget_ok(&self, t: usize) -> bool {
            if self.mutated(Mutation::IgnoreBudget) {
                return true;
            }
            match self.ctx[t].budget {
                Budget::Unlimited => true,
                Budget::Limited { .. } => self.ctx[t].remaining > 0,
            }
        }

        fn window_ok(&self, t: usize, cpu: usize) -> bool {
            self.mutated(Mutation::IgnoreWindow)
                || self.schedule.active_partition(cpu, self.now) == Some(self.ctx[t].partition)
        }

        /// SC-SEP-1: the ONE eligibility predicate — affinity, partition window, budget and
        /// readiness. Priority is consulted only among eligible threads, and criticality never.
        pub fn eligible(&self, t: usize, cpu: usize) -> bool {
            self.ctx[t].cpu == cpu
                && self.state[t] != State::Blocked
                && self.window_ok(t, cpu)
                && self.budget_ok(t)
        }

        /// Highest-priority eligible ready thread on `cpu`, FIFO within a level (SC-PRI-11).
        pub fn select(&self, cpu: usize) -> Option<usize> {
            for ((c, _prio, _part), q) in self.queues.range((cpu, 0, 0)..(cpu + 1, 0, 0)) {
                debug_assert_eq!(*c, cpu);
                let pick = if self.mutated(Mutation::LifoEqualPriority) {
                    q.iter().rev().copied().find(|&t| self.eligible(t, cpu))
                } else {
                    q.iter().copied().find(|&t| self.eligible(t, cpu))
                };
                if pick.is_some() {
                    return pick;
                }
            }
            None
        }

        fn push_back(&mut self, t: usize) {
            let k = self.key(t);
            self.queues.entry(k).or_default().push_back(t);
        }

        fn push_front(&mut self, t: usize) {
            let k = self.key(t);
            self.queues.entry(k).or_default().push_front(t);
        }

        fn take(&mut self, cpu: usize, t: usize) {
            let k = self.key(t);
            if let Some(q) = self.queues.get_mut(&k) {
                q.retain(|&x| x != t);
            }
            self.state[t] = State::Running;
            self.running[cpu] = Some(t);
        }

        /// SC-PRI-12: the RT-P1 scheduling decision on `cpu`.
        pub fn reschedule(&mut self, cpu: usize) {
            if let Some(cur) = self.running[cpu]
                && !self.eligible(cur, cpu)
            {
                self.running[cpu] = None;
                self.state[cur] = State::Ready;
                if self.budget_ok(cur) {
                    // A window boundary: the thread keeps its place for its next window.
                    self.push_front(cur);
                } else {
                    // Exhausted: it rejoins the tail, eligible again only at replenishment.
                    self.push_back(cur);
                }
            }
            let cand = self.select(cpu);
            match (self.running[cpu], cand) {
                (Some(cur), Some(c)) if self.ctx[c].priority < self.ctx[cur].priority => {
                    // Preempted by strictly higher priority: head of its level (SC-PRI-12a).
                    self.state[cur] = State::Ready;
                    self.push_front(cur);
                    self.take(cpu, c);
                }
                (None, Some(c)) => self.take(cpu, c),
                _ => {}
            }
        }

        /// General-purpose selection point: select only when nothing runs (never preempts).
        pub fn dispatch_point(&mut self, cpu: usize) {
            if self.running[cpu].is_none() {
                self.reschedule(cpu);
            }
        }

        /// A wake, from any CPU. It changes readiness only: never the context's CPU, never its
        /// budget (SC-CPU-3).
        pub fn wake(&mut self, t: usize, from_cpu: usize) {
            if self.state[t] != State::Blocked {
                return;
            }
            if self.mutated(Mutation::RemoteWakeMovesCpu) {
                self.ctx[t].cpu = from_cpu;
            }
            if self.mutated(Mutation::RemoteWakeRefillsBudget)
                && let Budget::Limited { budget, .. } = self.ctx[t].budget
            {
                self.ctx[t].remaining = budget;
            }
            self.state[t] = State::Ready;
            self.push_back(t);
            if self.preempt_on_wake {
                let cpu = self.ctx[t].cpu;
                self.reschedule(cpu);
            }
        }

        pub fn block(&mut self, cpu: usize) {
            if let Some(cur) = self.running[cpu].take() {
                self.state[cur] = State::Blocked;
            }
            if self.resched_on_block {
                self.reschedule(cpu);
            }
        }

        /// Explicit Yield (and the general-purpose quantum expiry): tail of its own level.
        pub fn yield_now(&mut self, cpu: usize) {
            if let Some(cur) = self.running[cpu].take() {
                self.state[cur] = State::Ready;
                self.push_back(cur);
            }
            self.reschedule(cpu);
        }

        /// Charge `ticks` of execution to `t`'s own context. Execution past exhaustion (a
        /// non-preemptible section that outlived the budget) is an overrun carried as debt.
        pub fn charge(&mut self, t: usize, ticks: Tick) {
            if let Budget::Limited { .. } = self.ctx[t].budget {
                let c = &mut self.ctx[t];
                let used = ticks.min(c.remaining);
                c.remaining -= used;
                c.debt += ticks - used;
            }
        }

        /// SC-EXEC: account a piece of kernel execution to its owner. Nothing is exempt.
        pub fn charge_kernel(&mut self, cpu: usize, work: KernelWork, ticks: Tick) {
            match work {
                KernelWork::CallerSynchronous(t) | KernelWork::Worker(t) => self.charge(t, ticks),
                KernelWork::Essential => {
                    if self.mutated(Mutation::EssentialWorkUncharged) {
                        return;
                    }
                    let frame = self.schedule.frame_index(self.now);
                    if self.allowance_frame[cpu] != frame {
                        self.allowance_frame[cpu] = frame;
                        self.allowance_used[cpu] = 0;
                    }
                    let room = self.allowance - self.allowance_used[cpu];
                    let within = ticks.min(room);
                    self.allowance_used[cpu] += within;
                    self.overrun[cpu] += ticks - within;
                    self.essential_total[cpu] += ticks;
                }
            }
            for c in 0..self.running.len() {
                if self.running[c].is_some() {
                    self.reschedule(c);
                }
            }
        }

        /// A synchronous IPC call from the running `client` to the blocked `server`. The client
        /// blocks; the server becomes ready under its OWN context. No budget moves (SC-IPC-2), and
        /// the server runs only if normal selection would choose it (SC-IPC-1).
        pub fn handoff(&mut self, cpu: usize, client: usize, server: usize) {
            assert_eq!(self.running[cpu], Some(client));
            self.running[cpu] = None;
            self.state[client] = State::Blocked;
            if self.mutated(Mutation::HandoffBypassesEligibility) {
                self.state[server] = State::Ready;
                self.take(cpu, server);
                return;
            }
            self.wake(server, cpu);
            self.reschedule(cpu);
        }

        /// One tick: charge the running contexts, replenish at release instants, then let every
        /// CPU observe the new instant — boundaries included, idle or not.
        pub fn tick(&mut self) {
            for cpu in 0..self.running.len() {
                if let Some(t) = self.running[cpu] {
                    self.charge(t, 1);
                }
            }
            self.now = self.now.checked_add(1).expect("model time overflow");
            for t in 0..self.ctx.len() {
                if let Budget::Limited { budget, period } = self.ctx[t].budget
                    && self.now >= self.ctx[t].next_release
                {
                    let c = &mut self.ctx[t];
                    c.remaining = if self.mutation == Some(Mutation::NoOverrunDebt) {
                        budget
                    } else {
                        budget.saturating_sub(c.debt)
                    };
                    c.debt = 0;
                    c.next_release = c
                        .next_release
                        .checked_add(period)
                        .expect("release overflow");
                }
            }
            for cpu in 0..self.running.len() {
                if self.mutated(Mutation::BoundaryWaitsForActivity) && self.running[cpu].is_none() {
                    continue;
                }
                self.reschedule(cpu);
            }
        }
    }

    type Check = fn(Option<Mutation>) -> Result<(), String>;

    macro_rules! ensure {
        ($cond:expr, $($msg:tt)*) => {
            if !$cond {
                return Err(format!($($msg)*));
            }
        };
    }

    fn one_cpu(windows: &[Window], m: Tick, parts: usize, mu: Option<Mutation>) -> Schedule {
        Schedule::new(0, m, windows, 1, parts, mu).expect("valid schedule")
    }

    fn w(cpu: usize, partition: usize, offset: Tick, duration: Tick) -> Window {
        Window {
            cpu,
            partition,
            offset,
            duration,
        }
    }

    const fn lim(budget: Tick, period: Tick) -> Budget {
        Budget::Limited { budget, period }
    }

    /// SC-PART-2: the highest priority in the system cannot run outside its partition's window.
    pub fn check_window(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 5), w(0, 1, 5, 5)], 10, 2, mu);
        // ctx 0: priority 0 in partition 1; ctx 1: priority 31 in partition 0.
        let mut m = Model::new(
            s,
            &[(0, 1, 0, lim(10, 10)), (0, 0, 31, lim(10, 10))],
            1,
            0,
            mu,
        );
        m.wake(0, 0);
        m.wake(1, 0);
        for _ in 0..30 {
            let want = if (m.now % 10) < 5 { 1 } else { 0 };
            ensure!(
                m.running[0] == Some(want),
                "t={} ran {:?}, window says ctx {want}",
                m.now,
                m.running[0]
            );
            m.tick();
        }
        Ok(())
    }

    /// SC-CTX-3: an exhausted context is ineligible whatever its priority, until replenishment.
    pub fn check_budget(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 10)], 10, 1, mu);
        let mut m = Model::new(
            s,
            &[(0, 0, 0, lim(2, 10)), (0, 0, 5, lim(10, 10))],
            1,
            0,
            mu,
        );
        m.wake(0, 0);
        m.wake(1, 0);
        for _ in 0..12 {
            let phase = m.now % 10;
            let want = if phase < 2 { 0 } else { 1 };
            ensure!(
                m.running[0] == Some(want),
                "t={} ran {:?}, want ctx {want}",
                m.now,
                m.running[0]
            );
            m.tick();
        }
        Ok(())
    }

    fn fifo_order(mu: Option<Mutation>) -> Vec<usize> {
        let s = one_cpu(&[w(0, 0, 0, 10)], 10, 1, mu);
        let ctxs = [(0, 0, 3, lim(10, 10)); 3];
        let mut m = Model::new(s, &ctxs, 1, 0, mu);
        for t in [2, 0, 1] {
            m.wake(t, 0);
        }
        let mut order = Vec::new();
        while let Some(t) = m.running[0] {
            order.push(t);
            m.block(0);
        }
        order
    }

    /// SC-PRI-11: equal priority is FIFO by readiness, and deterministic.
    pub fn check_fifo(mu: Option<Mutation>) -> Result<(), String> {
        let first = fifo_order(mu);
        ensure!(first == vec![2, 0, 1], "order {first:?}, want [2, 0, 1]");
        for _ in 0..3 {
            ensure!(fifo_order(mu) == first, "order not deterministic");
        }
        Ok(())
    }

    /// SC-PRI-12: preemption, Yield, block and wake each place the thread exactly as specified.
    pub fn check_queue_positions(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 100)], 100, 1, mu);
        let (a, b, c) = (0, 1, 2);
        let ctxs = [
            (0, 0, 1, lim(100, 100)),
            (0, 0, 1, lim(100, 100)),
            (0, 0, 0, lim(100, 100)),
        ];
        let mut m = Model::new(s, &ctxs, 1, 0, mu);
        m.wake(a, 0);
        m.wake(b, 0);
        ensure!(m.running[0] == Some(a), "a runs first");
        m.wake(c, 0);
        ensure!(m.running[0] == Some(c), "a higher-priority wake preempts");
        m.block(0);
        ensure!(
            m.running[0] == Some(a),
            "the preempted thread resumes ahead of its equal (got {:?})",
            m.running[0]
        );
        m.yield_now(0);
        ensure!(m.running[0] == Some(b), "Yield goes to the tail");
        m.block(0);
        ensure!(m.running[0] == Some(a), "a follows b");
        m.wake(b, 0);
        m.yield_now(0);
        ensure!(m.running[0] == Some(b), "a wake appends at the tail");
        Ok(())
    }

    /// SC-IPC-1/2: a handoff satisfies the same eligibility as selection and donates nothing.
    pub fn check_handoff(mu: Option<Mutation>) -> Result<(), String> {
        // Server in a window that is not active at the call.
        let s = one_cpu(&[w(0, 0, 0, 5), w(0, 1, 5, 5)], 10, 2, mu);
        let (client, server) = (0, 1);
        let mut m = Model::new(
            s,
            &[(0, 0, 5, lim(10, 10)), (0, 1, 0, lim(10, 10))],
            1,
            0,
            mu,
        );
        m.wake(client, 0);
        m.tick();
        let client_budget = m.ctx[client].remaining;
        m.handoff(0, client, server);
        while m.now < 5 {
            ensure!(
                m.running[0] != Some(server),
                "t={} the server ran outside its window via the handoff",
                m.now
            );
            m.tick();
        }
        ensure!(
            m.running[0] == Some(server),
            "the server runs in its own window"
        );
        m.tick();
        m.tick();
        ensure!(
            m.ctx[client].remaining == client_budget,
            "the client paid for the server's execution"
        );
        ensure!(
            m.ctx[server].remaining == 8,
            "the server paid for its own execution"
        );

        // Server whose own budget is exhausted.
        let s = one_cpu(&[w(0, 0, 0, 10)], 10, 1, mu);
        let mut m = Model::new(
            s,
            &[(0, 0, 5, lim(10, 10)), (0, 0, 0, lim(1, 10))],
            1,
            0,
            mu,
        );
        m.charge(server, 1);
        m.wake(client, 0);
        m.handoff(0, client, server);
        ensure!(
            m.running[0] != Some(server),
            "an exhausted server ran because the caller handed off to it"
        );
        Ok(())
    }

    /// SC-CPU-3: a remote wake changes neither the woken context's CPU nor its budget.
    pub fn check_remote_wake(mu: Option<Mutation>) -> Result<(), String> {
        let s = Schedule::new(0, 10, &[w(0, 0, 0, 10), w(1, 1, 0, 10)], 2, 2, mu)
            .map_err(|e| format!("{e:?}"))?;
        let t = 0;
        let mut m = Model::new(s, &[(1, 1, 0, lim(3, 10))], 2, 0, mu);
        m.charge(t, 3);
        m.wake(t, 0);
        ensure!(
            m.ctx[t].cpu == 1,
            "the wake moved the context to CPU {}",
            m.ctx[t].cpu
        );
        ensure!(
            m.ctx[t].remaining == 0,
            "the wake manufactured {} ticks of budget",
            m.ctx[t].remaining
        );
        ensure!(m.running == vec![None, None], "an exhausted context ran");
        while m.now < 10 {
            m.tick();
        }
        ensure!(
            m.running[1] == Some(t),
            "it runs on its own CPU once replenished"
        );
        Ok(())
    }

    /// SC-PART-6: window boundaries are instants of time; they advance on an idle CPU.
    pub fn check_idle_boundary(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 10), w(0, 1, 10, 10)], 20, 2, mu);
        let mut m = Model::new(s, &[(0, 1, 0, lim(10, 20))], 1, 0, mu);
        m.wake(0, 0);
        ensure!(
            m.running[0].is_none(),
            "nothing eligible in partition 0's window"
        );
        while m.now < 10 {
            m.tick();
        }
        ensure!(
            m.running[0] == Some(0),
            "partition 1's window opened at t=10 on an idle CPU"
        );
        Ok(())
    }

    /// SC-PART-3: invalid schedules, overlaps and arithmetic overflow are rejected exactly.
    pub fn check_schedule_validation(mu: Option<Mutation>) -> Result<(), String> {
        use ScheduleError as E;
        let cases: Vec<(Tick, Tick, Vec<Window>, ScheduleError)> = vec![
            (0, 0, vec![w(0, 0, 0, 1)], E::ZeroMajorFrame),
            (0, 10, vec![], E::NoWindows),
            (
                0,
                10_000,
                (0..65).map(|i| w(0, 0, i * 10, 1)).collect(),
                E::TooManyWindows,
            ),
            (Tick::MAX, 10, vec![w(0, 0, 0, 1)], E::EpochOverflow),
            (0, 10, vec![w(2, 0, 0, 1)], E::UnknownCpu { window: 0 }),
            (
                0,
                10,
                vec![w(0, 9, 0, 1)],
                E::UnknownPartition { window: 0 },
            ),
            (0, 10, vec![w(0, 0, 0, 0)], E::ZeroDuration { window: 0 }),
            (
                0,
                10,
                vec![w(0, 0, Tick::MAX - 1, 5)],
                E::Overflow { window: 0 },
            ),
            (
                0,
                10,
                vec![w(0, 0, 8, 5)],
                E::BeyondMajorFrame { window: 0 },
            ),
            (
                0,
                10,
                vec![w(0, 0, 0, 5), w(0, 1, 3, 3)],
                E::Overlap { a: 0, b: 1 },
            ),
            (
                0,
                10,
                vec![w(0, 0, 0, 5), w(1, 0, 5, 5)],
                E::PartitionSpansCpus { partition: 0 },
            ),
        ];
        for (epoch, frame, windows, want) in cases {
            let got = Schedule::new(epoch, frame, &windows, 2, 2, mu).map(|_| ());
            ensure!(got == Err(want), "expected {want:?}, got {got:?}");
        }
        // Back-to-back windows and a gap are valid.
        let ok = Schedule::new(0, 10, &[w(0, 0, 0, 5), w(0, 1, 5, 3)], 2, 2, mu);
        ensure!(ok.is_ok(), "an abutting schedule with a gap was rejected");
        let s = ok.unwrap();
        ensure!(
            s.active_partition(0, 8).is_none(),
            "a gap activates no partition"
        );
        Ok(())
    }

    /// SC-EXEC-5/6: essential kernel work is charged to the CPU's reserved allowance; excess is a
    /// recorded overrun, never an uncounted exemption; the allowance renews per major frame.
    pub fn check_essential(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 10)], 10, 1, mu);
        let worker = 1;
        let mut m = Model::new(
            s,
            &[(0, 0, 0, lim(10, 10)), (0, 0, 4, lim(5, 10))],
            1,
            3,
            mu,
        );
        // SC-EXEC-3: deferrable work is the worker's own, charged to its own context — not to
        // the CPU allowance and not to whoever happens to be running.
        m.charge_kernel(0, KernelWork::Worker(worker), 2);
        ensure!(
            m.ctx[worker].remaining == 3,
            "the worker did not pay for its work"
        );
        ensure!(
            m.allowance_used[0] == 0,
            "worker time was charged to the system allowance"
        );
        m.charge_kernel(0, KernelWork::Essential, 2);
        m.charge_kernel(0, KernelWork::Essential, 2);
        ensure!(m.essential_total[0] == 4, "essential work went uncounted");
        ensure!(
            m.allowance_used[0] == 3 && m.overrun[0] == 1,
            "used {} overrun {}",
            m.allowance_used[0],
            m.overrun[0]
        );
        ensure!(
            m.essential_total[0] == m.allowance_used[0] + m.overrun[0],
            "conservation violated"
        );
        while m.now < 10 {
            m.tick();
        }
        m.charge_kernel(0, KernelWork::Essential, 1);
        ensure!(
            m.allowance_used[0] == 1 && m.overrun[0] == 1,
            "the allowance did not renew"
        );
        Ok(())
    }

    /// SC-CTX-5: execution past exhaustion is debt repaid from the next replenishment.
    pub fn check_overrun_debt(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 10)], 10, 1, mu);
        let mut m = Model::new(s, &[(0, 0, 0, lim(3, 10))], 1, 0, mu);
        m.wake(0, 0);
        m.charge_kernel(0, KernelWork::CallerSynchronous(0), 5);
        ensure!(m.running[0].is_none(), "an exhausted context kept running");
        ensure!(
            m.ctx[0].debt == 2,
            "overrun not recorded: debt {}",
            m.ctx[0].debt
        );
        while m.now < 10 {
            m.tick();
        }
        ensure!(
            m.ctx[0].remaining == 1,
            "replenished to {}, want budget 3 minus debt 2",
            m.ctx[0].remaining
        );
        Ok(())
    }

    /// SC-IPC-3: cross-partition IPC does not extend either partition's window.
    pub fn check_cross_partition_window(mu: Option<Mutation>) -> Result<(), String> {
        let s = one_cpu(&[w(0, 0, 0, 5), w(0, 1, 5, 5)], 10, 2, mu);
        let (client, server) = (0, 1);
        let mut m = Model::new(
            s,
            &[(0, 0, 5, lim(10, 10)), (0, 1, 0, lim(10, 10))],
            1,
            0,
            mu,
        );
        m.wake(client, 0);
        while m.now < 4 {
            m.tick();
        }
        m.handoff(0, client, server);
        while m.now < 9 {
            m.tick();
            if m.now >= 5 {
                ensure!(
                    m.running[0] == Some(server),
                    "t={} server not running",
                    m.now
                );
            }
        }
        m.tick();
        ensure!(
            m.running[0] != Some(server),
            "t={} the server ran into partition 0's window",
            m.now
        );
        while m.now < 15 {
            m.tick();
        }
        ensure!(
            m.running[0] == Some(server),
            "the server resumes in its next window"
        );
        Ok(())
    }

    const CHECKS: &[(&str, Check)] = &[
        ("window", check_window),
        ("budget", check_budget),
        ("fifo", check_fifo),
        ("queue_positions", check_queue_positions),
        ("handoff", check_handoff),
        ("remote_wake", check_remote_wake),
        ("idle_boundary", check_idle_boundary),
        ("schedule_validation", check_schedule_validation),
        ("essential", check_essential),
        ("overrun_debt", check_overrun_debt),
        ("cross_partition_window", check_cross_partition_window),
    ];

    #[test]
    fn model_highest_priority_cannot_run_outside_its_partition_window() {
        check_window(None).unwrap();
    }

    #[test]
    fn model_exhausted_context_is_ineligible_despite_priority() {
        check_budget(None).unwrap();
    }

    #[test]
    fn model_equal_priority_order_is_deterministic_fifo() {
        check_fifo(None).unwrap();
    }

    #[test]
    fn model_preemption_yield_block_and_wake_queue_positions() {
        check_queue_positions(None).unwrap();
    }

    #[test]
    fn model_ipc_handoff_cannot_bypass_eligibility_or_donate_budget() {
        check_handoff(None).unwrap();
    }

    #[test]
    fn model_remote_wake_keeps_cpu_and_manufactures_no_budget() {
        check_remote_wake(None).unwrap();
    }

    #[test]
    fn model_window_boundaries_advance_while_the_cpu_is_idle() {
        check_idle_boundary(None).unwrap();
    }

    #[test]
    fn model_invalid_overlapping_and_overflowing_schedules_are_rejected() {
        check_schedule_validation(None).unwrap();
    }

    #[test]
    fn model_essential_kernel_work_is_charged_and_overrun_reported() {
        check_essential(None).unwrap();
    }

    #[test]
    fn model_overrun_debt_is_repaid_at_replenishment() {
        check_overrun_debt(None).unwrap();
    }

    #[test]
    fn model_cross_partition_ipc_does_not_extend_either_window() {
        check_cross_partition_window(None).unwrap();
    }

    /// The mutation check. Every important predicate is broken in turn; the check that targets
    /// it must then FAIL, and with no mutation every check passes. A check that cannot tell a
    /// predicate from its mutation is not evidence for that predicate.
    #[test]
    fn every_mutation_is_killed_by_its_targeted_check() {
        use Mutation as M;
        let targets: &[(Mutation, &[&str])] = &[
            (M::IgnoreWindow, &["window", "cross_partition_window"]),
            (M::IgnoreBudget, &["budget"]),
            (M::LifoEqualPriority, &["fifo", "queue_positions"]),
            (M::HandoffBypassesEligibility, &["handoff"]),
            (M::RemoteWakeMovesCpu, &["remote_wake"]),
            (M::RemoteWakeRefillsBudget, &["remote_wake"]),
            (M::BoundaryWaitsForActivity, &["idle_boundary"]),
            (M::AllowOverlap, &["schedule_validation"]),
            (M::WrappingWindowArithmetic, &["schedule_validation"]),
            (M::EssentialWorkUncharged, &["essential"]),
            (M::NoOverrunDebt, &["overrun_debt"]),
        ];
        for (name, check) in CHECKS {
            assert_eq!(check(None), Ok(()), "unmutated check {name} must pass");
        }
        for (mutation, killers) in targets {
            for killer in *killers {
                let (_, check) = CHECKS
                    .iter()
                    .find(|(n, _)| n == killer)
                    .expect("named check");
                assert!(
                    check(Some(*mutation)).is_err(),
                    "mutation {mutation:?} SURVIVED check {killer}"
                );
            }
        }
    }
}

/// The bridge (see the module docs): the model configured as the GENERAL-PURPOSE profile — one
/// partition per CPU whose single window spans the whole major frame, unlimited user budgets,
/// three priorities, wake never preempts, quantum expiry requeues at the tail — must agree with
/// the production `PriorityScheduler` decision for decision.
#[test]
fn sc_gp_1_general_purpose_model_agrees_with_production_priority_scheduler() {
    use model::{Budget, Model, Schedule, Window};

    const TIDS: usize = 48; // below MAX_RUN_QUEUE per level, so QueueFull is never the answer
    let prio = |p: usize| [TaskPriority::High, TaskPriority::Normal, TaskPriority::Low][p];
    for seed in 1..=24u64 {
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let levels: Vec<usize> = (0..TIDS).map(|_| (next() % 3) as usize).collect();
        let window = Window {
            cpu: 0,
            partition: 0,
            offset: 0,
            duration: 1000,
        };
        let schedule = Schedule::new(0, 1000, &[window], 1, 1, None).unwrap();
        let ctxs: Vec<_> = levels
            .iter()
            .map(|&p| (0, 0, p, Budget::Unlimited))
            .collect();
        let mut m = Model::new(schedule, &ctxs, 1, 0, None);
        m.preempt_on_wake = false;
        m.resched_on_block = false;
        let mut p = PriorityScheduler::default();
        let mut blocked: Vec<usize> = (0..TIDS).collect();
        for step in 0..3000 {
            match next() % 4 {
                0 if !blocked.is_empty() => {
                    let t = blocked.swap_remove((next() as usize) % blocked.len());
                    m.wake(t, 0);
                    p.enqueue_with_priority(ThreadId(t as u64 + 1), prio(levels[t]))
                        .unwrap();
                }
                1 => {
                    m.dispatch_point(0);
                    p.dispatch_next_selection();
                }
                2 => {
                    m.yield_now(0);
                    p.on_preempt_selection();
                }
                _ => {
                    if let Some(t) = m.running[0] {
                        blocked.push(t);
                    }
                    m.block(0);
                    p.block_current();
                }
            }
            assert_eq!(
                m.running[0].map(|t| t as u64 + 1),
                p.current_tid().map(|t| t.0),
                "seed {seed} step {step}: model and production disagree on the running task"
            );
        }
    }
}
