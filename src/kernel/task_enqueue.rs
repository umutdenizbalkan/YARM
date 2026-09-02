// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN1 SP-1 — **THE** task-enqueue transition, in one place.
//!
//! Making a task scheduler-visible is one decision spread across two lock ranks:
//!
//! * **rank 2 (task)** decides *whether* the task may be enqueued and *with what* — the
//!   spawn-reservation refusal, the driver-affinity pin, and the class→priority derivation;
//! * **rank 1 (scheduler)** performs the queue mutation itself, and owns the duplicate-enqueue
//!   refusal (`SchedulerError::AlreadyQueued`) inside the queue primitive.
//!
//! Before this module the composition existed **six** times over — twice in the broad
//! `KernelState` (`enqueue_task`, `enqueue_on_cpu`) and four more times transcribed into
//! `SharedKernel` split paths (the owner-revalidation requeue, the wake transaction, the
//! AP enqueue→dispatch transaction, and the FutexWake route). Six transcriptions of one policy
//! is six chances for them to disagree, and they already had: the FutexWake copy omitted the
//! spawn-reservation refusal entirely.
//!
//! Everything here is a free function over borrowed domain storage, so it imposes **no** lock
//! discipline of its own. That is deliberate, because the two disciplines in the tree are both
//! legitimate and must both keep working:
//!
//! * **plan-then-commit** — take rank 2, produce an [`EnqueuePlan`], *release rank 2*, then take
//!   rank 1 and commit. This is what a fresh split route does, and it makes rank 1 strictly last.
//! * **nested ascending 1 → 2** — hold rank 1, take rank 2 inside it to resolve the policy, and
//!   commit without releasing. `enqueue_then_dispatch_on_cpu_split` needs this because its
//!   block-and-requeue must be atomic against the dispatcher.
//!
//! Both call the same functions and therefore cannot drift. What this module must never become
//! is a *second* scheduler policy: the rank-1 half delegates to `SmpScheduler` primitives
//! unchanged, so queue selection, priority ordering and duplicate refusal remain the
//! scheduler's own.

use crate::kernel::boot::{KernelError, map_scheduler_error};
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, SmpScheduler, TaskPriority};
use crate::kernel::task::{TaskClass, ThreadControlBlock};

/// Which queue the commit half will place the task on.
///
/// `Balanced` reproduces `enqueue_task`'s unpinned arm — `enqueue_balanced` picks the
/// least-loaded non-wake-only online CPU. `Pinned` reproduces both the affinity arm of
/// `enqueue_task` and every `enqueue_on_cpu` caller, which names its CPU explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueTarget {
    Balanced,
    Pinned(CpuId),
}

/// The complete, already-decided enqueue. Producing one takes rank 2; consuming one takes rank 1.
///
/// It carries no borrow of task storage, which is exactly what lets a caller drop the rank-2
/// guard between the two halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnqueuePlan {
    pub(crate) tid: u64,
    pub(crate) priority: TaskPriority,
    pub(crate) target: EnqueueTarget,
}

/// THE class → priority rule.
///
/// `SystemServer` is `High`; `Driver` and `App` are `Normal`. TID 0 is the idle/supervisor
/// sentinel and is `Normal` without a class lookup at all — see [`plan_pinned_enqueue_locked`].
#[inline]
pub(crate) const fn priority_for_class(class: TaskClass) -> TaskPriority {
    match class {
        TaskClass::SystemServer => TaskPriority::High,
        TaskClass::Driver | TaskClass::App => TaskPriority::Normal,
    }
}

/// rank 2 — locate `tid`'s slot, refusing a spawn reservation.
///
/// The run queue carries bare TIDs with no status precondition, so the enqueue seam is the only
/// place that can keep a `Reserved` task out of it. A reservation becomes enqueueable at exactly
/// the moment the typed live commit clears `Reserved`, and not before.
///
/// `Ok(None)` means "no such TID", which is not by itself an error: TID 0 is legitimately absent
/// from the TCB array, and each caller decides what a missing slot means for it.
pub(crate) fn refuse_reservation_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
) -> Result<Option<usize>, KernelError> {
    let Some(idx) = tcbs
        .iter()
        .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid))
    else {
        return Ok(None);
    };
    if tcbs[idx]
        .as_ref()
        .is_some_and(ThreadControlBlock::is_spawn_reservation)
    {
        crate::yarm_log!(
            "ENQUEUE_REFUSED tid={} reason=spawn_reservation_not_live",
            tid
        );
        return Err(KernelError::WrongObject);
    }
    Ok(Some(idx))
}

/// rank 2 — the policy `KernelState::enqueue_on_cpu` reads: refusal, then priority.
///
/// Deliberately does **not** pin driver affinity and does **not** read `cpu_affinity`: a pinned
/// enqueue names its CPU, and the broad twin has never consulted affinity on this path. TID 0 is
/// `Normal` without requiring a TCB or a class; any other TID needs both.
pub(crate) fn plan_pinned_enqueue_locked(
    tcbs: &[Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    tid: u64,
    cpu: CpuId,
) -> Result<EnqueuePlan, KernelError> {
    let idx = refuse_reservation_locked(tcbs, tid)?;
    let priority = if tid == 0 {
        TaskPriority::Normal
    } else {
        priority_for_class(
            idx.and_then(|idx| classes.get(idx).copied().flatten())
                .ok_or(KernelError::TaskMissing)?,
        )
    };
    Ok(EnqueuePlan {
        tid,
        priority,
        target: EnqueueTarget::Pinned(cpu),
    })
}

/// rank 2 — the policy `KernelState::enqueue_task` reads: refusal, driver-affinity pin, priority,
/// then the affinity that pin may just have established.
///
/// The order matters and is reproduced exactly: the pin runs *before* the affinity is read, so a
/// driver enqueued for the first time lands on `current_cpu` rather than being balanced away
/// from the CPU that owns its device.
pub(crate) fn plan_balanced_enqueue_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    tid: u64,
    current_cpu: CpuId,
) -> Result<EnqueuePlan, KernelError> {
    let idx = refuse_reservation_locked(tcbs, tid)?;
    if tid == 0 {
        return Ok(EnqueuePlan {
            tid,
            priority: TaskPriority::Normal,
            target: EnqueueTarget::Balanced,
        });
    }
    let idx = idx.ok_or(KernelError::TaskMissing)?;
    let class = classes
        .get(idx)
        .copied()
        .flatten()
        .ok_or(KernelError::TaskMissing)?;
    let tcb = tcbs[idx].as_mut().ok_or(KernelError::TaskMissing)?;
    if class == TaskClass::Driver && tcb.cpu_affinity.is_none() {
        tcb.cpu_affinity = Some(current_cpu);
    }
    Ok(EnqueuePlan {
        tid,
        priority: priority_for_class(class),
        target: match tcb.cpu_affinity {
            Some(cpu) => EnqueueTarget::Pinned(cpu),
            None => EnqueueTarget::Balanced,
        },
    })
}

/// rank 1 — **THE** commit. The only place a task-lifecycle transition touches a run queue.
///
/// Delegates to the `SmpScheduler` primitives unchanged, so wake-only rejection, CPU-online
/// validation, balanced placement and the duplicate-enqueue refusal all remain the scheduler's
/// own rules rather than being restated here. Returns the CPU the task actually landed on.
pub(crate) fn commit_enqueue_locked(
    scheduler: &mut SmpScheduler,
    plan: &EnqueuePlan,
) -> Result<CpuId, KernelError> {
    match plan.target {
        EnqueueTarget::Pinned(cpu) => scheduler
            .enqueue_on_with_priority(cpu, ThreadId(plan.tid), plan.priority)
            .map(|()| cpu)
            .map_err(map_scheduler_error),
        EnqueueTarget::Balanced => scheduler
            .enqueue_balanced(ThreadId(plan.tid), plan.priority)
            .map_err(map_scheduler_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::task::TaskStatus;

    fn tcb(tid: u64, affinity: Option<CpuId>, status: TaskStatus) -> ThreadControlBlock {
        let mut t = ThreadControlBlock::new(ThreadId(tid), None);
        t.cpu_affinity = affinity;
        t.status = status;
        t
    }

    /// The refusal is structural: a reservation can never reach a run queue, whichever planner
    /// is asked.
    #[test]
    fn a_spawn_reservation_is_refused_by_every_planner() {
        let mut tcbs = [Some(tcb(7, None, TaskStatus::Reserved))];
        let classes = [Some(TaskClass::App)];
        assert_eq!(
            plan_pinned_enqueue_locked(&tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::WrongObject
        );
        assert_eq!(
            plan_balanced_enqueue_locked(&mut tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::WrongObject
        );
        // And nothing was mutated on the way to refusing.
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, None);
    }

    /// TID 0 is the sentinel: Normal, no class lookup, no TCB required.
    #[test]
    fn tid_zero_is_normal_without_a_class() {
        let mut tcbs: [Option<ThreadControlBlock>; 0] = [];
        let classes: [Option<TaskClass>; 0] = [];
        let pinned = plan_pinned_enqueue_locked(&tcbs, &classes, 0, CpuId(1)).expect("sentinel");
        assert_eq!(pinned.priority, TaskPriority::Normal);
        assert_eq!(pinned.target, EnqueueTarget::Pinned(CpuId(1)));
        let balanced =
            plan_balanced_enqueue_locked(&mut tcbs, &classes, 0, CpuId(1)).expect("sentinel");
        assert_eq!(balanced.target, EnqueueTarget::Balanced);
    }

    /// A live TID with no class is `TaskMissing`, not a silent `Normal`. The FutexWake
    /// transcription defaulted here, which is exactly the drift this module ends.
    #[test]
    fn a_classless_task_is_task_missing_not_normal() {
        let mut tcbs = [Some(tcb(7, None, TaskStatus::Runnable))];
        let classes = [None];
        assert_eq!(
            plan_pinned_enqueue_locked(&tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::TaskMissing
        );
        assert_eq!(
            plan_balanced_enqueue_locked(&mut tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::TaskMissing
        );
    }

    /// The driver pin happens before the affinity is read, and only when unpinned.
    #[test]
    fn an_unpinned_driver_is_pinned_to_the_current_cpu_then_targeted_there() {
        let mut tcbs = [Some(tcb(9, None, TaskStatus::Runnable))];
        let classes = [Some(TaskClass::Driver)];
        let plan = plan_balanced_enqueue_locked(&mut tcbs, &classes, 9, CpuId(3)).expect("plan");
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, Some(CpuId(3)));
        assert_eq!(plan.target, EnqueueTarget::Pinned(CpuId(3)));
        assert_eq!(plan.priority, TaskPriority::Normal);
    }

    #[test]
    fn an_already_pinned_driver_keeps_its_cpu() {
        let mut tcbs = [Some(tcb(9, Some(CpuId(1)), TaskStatus::Runnable))];
        let classes = [Some(TaskClass::Driver)];
        let plan = plan_balanced_enqueue_locked(&mut tcbs, &classes, 9, CpuId(3)).expect("plan");
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, Some(CpuId(1)));
        assert_eq!(plan.target, EnqueueTarget::Pinned(CpuId(1)));
    }

    /// A non-driver is never pinned by the enqueue, and an unpinned one is balanced.
    #[test]
    fn a_system_server_is_high_priority_and_never_pinned_by_the_enqueue() {
        let mut tcbs = [Some(tcb(4, None, TaskStatus::Runnable))];
        let classes = [Some(TaskClass::SystemServer)];
        let plan = plan_balanced_enqueue_locked(&mut tcbs, &classes, 4, CpuId(2)).expect("plan");
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, None);
        assert_eq!(plan.target, EnqueueTarget::Balanced);
        assert_eq!(plan.priority, TaskPriority::High);
    }

    /// The pinned planner ignores affinity entirely — it enqueues where it is told.
    #[test]
    fn the_pinned_planner_names_its_own_cpu_regardless_of_affinity() {
        let tcbs = [Some(tcb(9, Some(CpuId(1)), TaskStatus::Runnable))];
        let classes = [Some(TaskClass::Driver)];
        let plan = plan_pinned_enqueue_locked(&tcbs, &classes, 9, CpuId(0)).expect("plan");
        assert_eq!(plan.target, EnqueueTarget::Pinned(CpuId(0)));
    }

    /// The class→priority rule has exactly one definition, and it is this one.
    #[test]
    fn the_priority_rule_is_total_and_stated_once() {
        assert_eq!(
            priority_for_class(TaskClass::SystemServer),
            TaskPriority::High
        );
        assert_eq!(priority_for_class(TaskClass::Driver), TaskPriority::Normal);
        assert_eq!(priority_for_class(TaskClass::App), TaskPriority::Normal);
        // Written once, inside `priority_for_class`. The literal is assembled here so this
        // assertion is not itself a second copy of the rule.
        const SRC: &str = include_str!("task_enqueue.rs");
        let rule = concat!("TaskClass::SystemServer =", "> TaskPriority::High");
        assert_eq!(
            SRC.matches(rule).count(),
            1,
            "the rule is written once, in priority_for_class"
        );
    }
}
