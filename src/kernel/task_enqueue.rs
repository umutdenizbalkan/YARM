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
//! is a *second* scheduler policy: the rank-1 half delegates to the per-CPU scheduler primitives
//! unchanged, so queue selection, priority ordering and duplicate refusal remain the
//! scheduler's own.

use crate::kernel::boot::{KernelError, map_scheduler_error};
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, CpuView, SchedAll, TaskPriority};
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
    /// SCHED-LOCK2A — the incarnation this plan is for (`ThreadControlBlock::incarnation`), read
    /// under the same rank-2 acquisition that produced the plan. `0` = TID 0 or an anonymous
    /// reservation; otherwise every 1L step re-checks it ([`admit_home_locked`]), so a plan whose
    /// TID was reused after rank 2 was dropped places nothing.
    pub(crate) incarnation: u64,
}

/// SCHED-LOCK2A — the incarnation stamp of `tid`'s live TCB, `0` for TID 0 or no TCB.
pub(crate) fn incarnation_of_locked(tcbs: &[Option<ThreadControlBlock>], tid: u64) -> u64 {
    if tid == 0 {
        return 0;
    }
    tcbs.iter()
        .flatten()
        .find(|t| t.tid.0 == tid)
        .map_or(0, |t| t.incarnation)
}

/// SCHED-LOCK2B — what rank 2 says about where ONE exact incarnation can be placed (H-HOME,
/// SCHEDULING_CONTRACT §20.5): only on the home of its current incarnation, and nowhere if it has
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeLookup {
    /// The incarnation `stamp` (never 0) has home `home`: its placement, if any, is on `home`'s
    /// 1L and nowhere else.
    Home { home: CpuId, stamp: u64 },
    /// The incarnation `stamp` has no home. Every 1L placement step admits only on a recorded
    /// home, so at this rank-2 instant it is placed on NO CPU.
    Homeless { stamp: u64 },
    /// TID 0 or a kernel-thread id (neither has a TCB), no TCB, or the TID names an incarnation
    /// other than the caller's stamp.
    Stale,
}

/// SCHED-LOCK2B — rank 2: locate `tid`'s home for the caller's incarnation `stamp`.
///
/// `stamp != 0` must equal the TCB's stamp, so a TID reused after the caller captured its stamp —
/// inside the same address space included — is `Stale`. `stamp == 0` (hosted fixtures, and a caller
/// whose identity is the numeric TID itself, such as the reaper's pre-claim proof) pins whichever
/// incarnation this acquisition sees; the 1L step then revalidates THAT stamp
/// ([`home_unchanged_locked`]), so the two steps are always about one incarnation.
pub(crate) fn home_lookup_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    stamp: u64,
) -> HomeLookup {
    if tid == 0 {
        return HomeLookup::Stale;
    }
    let Some(tcb) = tcbs.iter().flatten().find(|t| t.tid.0 == tid) else {
        return HomeLookup::Stale;
    };
    if stamp != 0 && tcb.incarnation != stamp {
        return HomeLookup::Stale;
    }
    match tcb.cpu_affinity {
        Some(home) => HomeLookup::Home {
            home,
            stamp: tcb.incarnation,
        },
        None => HomeLookup::Homeless {
            stamp: tcb.incarnation,
        },
    }
}

/// SCHED-LOCK2B — rank 2 nested in `home`'s 1L: is `tid` still incarnation `stamp`, homed on
/// `home`? Homes are never rewritten, so `false` means the incarnation went away (exit and reap,
/// TID reuse) between the lookup and the 1L.
pub(crate) fn home_unchanged_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    stamp: u64,
    home: CpuId,
) -> bool {
    tid != 0
        && tcbs
            .iter()
            .flatten()
            .find(|t| t.tid.0 == tid)
            .is_some_and(|t| t.incarnation == stamp && t.cpu_affinity == Some(home))
}

/// SCHED-LOCK2A — why a placement was refused for its home. Every refusal maps to the error a
/// commit onto a CPU that cannot take the task already returns (`WrongObject`, from
/// `SchedulerError::CpuOffline`), so no caller and no syscall ABI sees a new error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeRefusal {
    /// The incarnation has a home and the target is another CPU.
    OffHome,
    /// The TID now names a different incarnation (or none): the plan is stale.
    Stale,
    /// The incarnation has no home at a 1L step: its first placement never recorded one.
    Homeless,
}

impl HomeRefusal {
    fn reason(self) -> &'static str {
        match self {
            HomeRefusal::OffHome => "off_home",
            HomeRefusal::Stale => "stale",
            HomeRefusal::Homeless => "homeless",
        }
    }
}

fn refuse_home(tid: u64, cpu: CpuId, home: Option<CpuId>, why: HomeRefusal) -> KernelError {
    crate::yarm_log!(
        "SCHED_HOME_REFUSED tid={} cpu={} home={} reason={}",
        tid,
        cpu.0,
        home.map_or(-1, |h| h.0 as i32),
        why.reason()
    );
    map_scheduler_error(crate::kernel::scheduler::SchedulerError::CpuOffline)
}

/// SCHED-LOCK2A — rank 2: THE home binding of an explicit placement of `tid` on `cpu`.
///
/// * The incarnation has a home: `cpu` must be it, or the placement is refused (`OffHome`) with
///   NOTHING written. This is SC-CPU-2's structural half — a Pinned plan for a homed task names
///   that home or fails closed; it never silently overrides it.
/// * It has none: `cpu` is recorded as its home in this same acquisition, so of two CPUs racing
///   to place it first, exactly one records its CPU and the other then sees that home (and is
///   refused if it named a different CPU).
/// * TID 0 (no TCB, no home) and a TID with no TCB bind nothing.
///
/// Returns the incarnation the binding is for (`0` for TID 0 / no TCB).
pub(crate) fn bind_home_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
    cpu: CpuId,
) -> Result<u64, KernelError> {
    if tid == 0 {
        return Ok(0);
    }
    let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) else {
        return Ok(0);
    };
    match tcb.cpu_affinity {
        Some(home) if home != cpu => Err(refuse_home(tid, cpu, Some(home), HomeRefusal::OffHome)),
        Some(_) => Ok(tcb.incarnation),
        None => {
            tcb.cpu_affinity = Some(cpu);
            Ok(tcb.incarnation)
        }
    }
}

/// SCHED-LOCK2A — rank 2, read-only, run INSIDE the 1L of `cpu` (ascending 1 → 2): may `cpu`
/// reserve, commit or restore the placement of `tid`'s incarnation `incarnation`?
///
/// Bound to the authority that commits — the CPU whose lock is held — and to the incarnation the
/// plan was made for, so it holds across every dropped lock between planning and this step:
///
/// * TID 0 is always admitted (the sentinel has no TCB and no home);
/// * the TID must still name the same incarnation (`incarnation == 0` admits whichever one it
///   names: an anonymous plan, or a caller with no captured stamp) — else `Stale`, which covers
///   exit-and-reap and TID reuse, including reuse inside the same address space;
/// * that incarnation's home must be `cpu` — `OffHome` if it is another CPU, `Homeless` if the
///   first placement recorded none.
///
/// An exited but unreaped incarnation is still that incarnation and still has its home: it is
/// admitted there only, so an exited entry that is still queued is always queued on its home.
pub(crate) fn admit_home_locked(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    incarnation: u64,
    cpu: CpuId,
) -> Result<(), KernelError> {
    if tid == 0 {
        return Ok(());
    }
    let Some(tcb) = tcbs.iter().flatten().find(|t| t.tid.0 == tid) else {
        return Err(refuse_home(tid, cpu, None, HomeRefusal::Stale));
    };
    if incarnation != 0 && tcb.incarnation != incarnation {
        return Err(refuse_home(tid, cpu, tcb.cpu_affinity, HomeRefusal::Stale));
    }
    match tcb.cpu_affinity {
        Some(home) if home == cpu => Ok(()),
        Some(home) => Err(refuse_home(tid, cpu, Some(home), HomeRefusal::OffHome)),
        None => Err(refuse_home(tid, cpu, None, HomeRefusal::Homeless)),
    }
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

/// rank 2 — the policy `KernelState::enqueue_on_cpu` reads: refusal, then priority, then the
/// home binding.
///
/// Deliberately does **not** pin driver affinity: a pinned enqueue names its CPU. SCHED-LOCK2A:
/// it does read `cpu_affinity` now, as the HOME — the named CPU must be the incarnation's home
/// (or becomes it, for a task with none), else the plan is refused before anything is written
/// ([`bind_home_locked`]). TID 0 is `Normal` without requiring a TCB or a class; any other TID
/// needs both.
pub(crate) fn plan_pinned_enqueue_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    tid: u64,
    cpu: CpuId,
) -> Result<EnqueuePlan, KernelError> {
    // SCHED-LOCK1 witness only: the incarnation this owner places, from its own rank-2 view.
    #[cfg(feature = "sched-lock1-witness")]
    crate::kernel::schedlock1_witness::stage_from_tcbs(tcbs, tid);
    let idx = refuse_reservation_locked(tcbs, tid)?;
    let priority = if tid == 0 {
        TaskPriority::Normal
    } else {
        priority_for_class(
            idx.and_then(|idx| classes.get(idx).copied().flatten())
                .ok_or(KernelError::TaskMissing)?,
        )
    };
    // Every refusal above wrote nothing; the binding is the plan's only write, and it is last.
    let incarnation = bind_home_locked(tcbs, tid, cpu)?;
    Ok(EnqueuePlan {
        tid,
        priority,
        target: EnqueueTarget::Pinned(cpu),
        incarnation,
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
    // SCHED-LOCK1 witness only: the incarnation this owner places, from its own rank-2 view.
    #[cfg(feature = "sched-lock1-witness")]
    crate::kernel::schedlock1_witness::stage_from_tcbs(tcbs, tid);
    let idx = refuse_reservation_locked(tcbs, tid)?;
    if tid == 0 {
        return Ok(EnqueuePlan {
            tid,
            priority: TaskPriority::Normal,
            target: EnqueueTarget::Balanced,
            incarnation: 0,
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
        incarnation: tcb.incarnation,
    })
}

/// AP-SCHED1 — rank 2: **THE home rule.** A task's first placement fixes the CPU it runs on for
/// the rest of its life; every later enqueue of it — a wake, a requeue, a timeout — targets that
/// home. Records `chosen` as `tid`'s home if it has none and returns the home either way. There is
/// no migration in this increment, and this is what rules it out: no enqueue path can pick a CPU
/// for a task that already has one.
///
/// `chosen` is the caller's placement decision — the CPU an explicit placement names, or the
/// scheduler's balanced choice read beforehand under rank 1. A stale balanced choice only picks a
/// less-than-ideal home; it can never move a task that already has one. TID 0 (no TCB) has no home
/// and gets `chosen`.
pub(crate) fn fix_home_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
    chosen: CpuId,
) -> CpuId {
    match tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
        Some(tcb) => *tcb.cpu_affinity.get_or_insert(chosen),
        None => chosen,
    }
}

/// FUTEX-REQUEUE-ATOMIC1 — rank 2, read-only: where a wake would place `tid` IF it already has a
/// home — `(home, priority_for_class(class))`, exactly what [`plan_balanced_enqueue_locked`]
/// followed by the home rule resolves to for such a task. `None` for a task with no home yet (its
/// first placement needs the scheduler's balanced choice, at rank 1), no class, no TCB, or a
/// spawn reservation. Mutates nothing, so a compare-and-requeue commit can ask it under the task
/// lock and still write nothing when it refuses.
pub(crate) fn homed_wake_placement_locked(
    tcbs: &[Option<ThreadControlBlock>],
    classes: &[Option<TaskClass>],
    tid: u64,
) -> Option<(CpuId, TaskPriority)> {
    if tid == 0 {
        return None;
    }
    let idx = tcbs.iter().position(|slot| {
        slot.as_ref()
            .is_some_and(|tcb| tcb.tid.0 == tid && !tcb.is_spawn_reservation())
    })?;
    let class = classes.get(idx).copied().flatten()?;
    let home = tcbs[idx].as_ref()?.cpu_affinity?;
    Some((home, priority_for_class(class)))
}

/// rank 1L — **THE** commit, on the one CPU whose lock the caller holds. The only place a
/// task-lifecycle transition touches a run queue.
///
/// Delegates to the per-CPU scheduler primitive unchanged, so wake-only rejection, CPU-online
/// validation and the duplicate-enqueue refusal all remain the scheduler's own rules rather than
/// being restated here. SCHED-LOCK1: the plan must name exactly this CPU (`Pinned(view.cpu())`);
/// a balanced plan is resolved to a CPU before any 1L is taken (`resolve_plan_home_split`), so a
/// plan for another CPU is refused (`CpuOffline`, the error a commit onto a CPU that cannot take
/// it already returns) with nothing mutated.
///
/// SCHED-LOCK2A: `tcbs` is the task table read under rank 2 nested inside this 1L; the home
/// admission ([`admit_home_locked`]) runs before the queue is touched.
pub(crate) fn commit_enqueue_on_cpu_locked(
    view: &mut CpuView<'_>,
    plan: &EnqueuePlan,
    tcbs: &[Option<ThreadControlBlock>],
) -> Result<CpuId, KernelError> {
    let cpu = view.cpu();
    admit_home_locked(tcbs, plan.tid, plan.incarnation, cpu)?;
    match plan.target {
        EnqueueTarget::Pinned(target) if target == cpu => view
            .enqueue_with_priority(ThreadId(plan.tid), plan.priority)
            .map(|()| cpu)
            .map_err(map_scheduler_error),
        _ => Err(map_scheduler_error(
            crate::kernel::scheduler::SchedulerError::CpuOffline,
        )),
    }
}

/// FUTEX-WAKE-CAP1 — what filling a reserved slot achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReservedCommit {
    /// The task is now queued exactly once on this CPU.
    Queued(CpuId),
    /// The task was already placed on this CPU — its current task, mid-publication of a wait whose
    /// own read-back owns its placement, or already queued. The reservation is released.
    AlreadyPlaced(CpuId),
    /// SCHED-LOCK2A — the home admission refused the commit (the TID now names another
    /// incarnation, or none): nothing was queued, and the reservation was released here, so the
    /// slot is settled exactly once.
    Refused(CpuId),
}

/// rank 1L — FUTEX-WAKE-CAP1: promise `plan` a user slot on this CPU, BEFORE the caller mutates
/// the task. The plan must name exactly this CPU. Refused — with nothing mutated — for a full
/// queue, an offline or wake-only CPU, or a plan for another CPU: the same refusals the commit
/// would make, moved ahead of the irreversible step.
///
/// SCHED-LOCK2A: the home admission runs first, so an off-home or stale plan reserves nothing.
pub(crate) fn reserve_enqueue_on_cpu_locked(
    view: &mut CpuView<'_>,
    plan: &EnqueuePlan,
    tcbs: &[Option<ThreadControlBlock>],
) -> Result<(), KernelError> {
    admit_home_locked(tcbs, plan.tid, plan.incarnation, view.cpu())?;
    let reserved = match plan.target {
        EnqueueTarget::Pinned(target) if target == view.cpu() => view
            .reserve_user_slot(plan.priority)
            .map_err(map_scheduler_error),
        _ => Err(map_scheduler_error(
            crate::kernel::scheduler::SchedulerError::CpuOffline,
        )),
    };
    // SCHED-SAT-EVIDENCE1 (witness only): the occupancy this refusal saw, under this 1L.
    #[cfg(any(test, feature = "sched-time-witness", feature = "futex-time-witness"))]
    if let Err(err) = &reserved {
        let (queued, promised) = view.witness_occupancy(plan.priority);
        let reason = if matches!(err, KernelError::SchedulerFull) {
            crate::kernel::sat_witness::REASON_QUEUE_FULL
        } else {
            crate::kernel::sat_witness::REASON_OTHER
        };
        crate::kernel::sat_witness::record(
            view.cpu().0 as usize,
            plan.tid,
            plan.priority as u64,
            queued,
            promised,
            reason,
        );
    }
    reserved
}

/// rank 1L — FUTEX-WAKE-CAP1: release a promise made by [`reserve_enqueue_on_cpu_locked`] that
/// will not be filled (the task was taken by another owner first).
pub(crate) fn release_enqueue_reservation_on_cpu_locked(
    view: &mut CpuView<'_>,
    plan: &EnqueuePlan,
) {
    view.release_user_slot(plan.priority);
}

/// rank 1L — FUTEX-WAKE-CAP1: THE commit of a reserved enqueue. It cannot be refused for capacity
/// (the slot was promised); `AlreadyQueued` is reported as `AlreadyPlaced`. Anything else is an
/// invariant break and is returned as the error.
///
/// SCHED-LOCK2A: the home admission is re-checked — the task lock was dropped between the
/// reservation, the claim and this commit — and a refused commit releases the reservation
/// itself (`Refused`), queueing nothing.
pub(crate) fn commit_reserved_enqueue_on_cpu_locked(
    view: &mut CpuView<'_>,
    plan: &EnqueuePlan,
    tcbs: &[Option<ThreadControlBlock>],
) -> Result<ReservedCommit, KernelError> {
    let cpu = view.cpu();
    if admit_home_locked(tcbs, plan.tid, plan.incarnation, cpu).is_err() {
        view.release_user_slot(plan.priority);
        return Ok(ReservedCommit::Refused(cpu));
    }
    match view.enqueue_reserved_with_priority(ThreadId(plan.tid), plan.priority) {
        Ok(()) => Ok(ReservedCommit::Queued(cpu)),
        Err(crate::kernel::scheduler::SchedulerError::AlreadyQueued) => {
            Ok(ReservedCommit::AlreadyPlaced(cpu))
        }
        Err(other) => Err(map_scheduler_error(other)),
    }
}

/// The whole-domain commit (boot and hosted tests, under every 1L and 1G): a pinned plan is THE
/// per-CPU commit above on its CPU. SCHED-LOCK2A: a balanced plan reaches here only for TID 0
/// (every caller resolves a live task's balanced plan to its recorded home first); for any other
/// TID it is refused, because a balanced commit would place the task without recording a home.
pub(crate) fn commit_enqueue_locked(
    scheduler: &mut SchedAll<'_>,
    plan: &EnqueuePlan,
    tcbs: &[Option<ThreadControlBlock>],
) -> Result<CpuId, KernelError> {
    match plan.target {
        EnqueueTarget::Pinned(cpu) => {
            scheduler
                .validate_online_cpu(cpu)
                .map_err(map_scheduler_error)?;
            let mut view = scheduler.cpu(cpu).map_err(map_scheduler_error)?;
            commit_enqueue_on_cpu_locked(&mut view, plan, tcbs)
        }
        EnqueueTarget::Balanced if plan.tid == 0 => scheduler
            .enqueue_balanced(ThreadId(plan.tid), plan.priority)
            .map_err(map_scheduler_error),
        EnqueueTarget::Balanced => {
            Err(refuse_home(plan.tid, CpuId(0), None, HomeRefusal::Homeless))
        }
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
            plan_pinned_enqueue_locked(&mut tcbs, &classes, 7, CpuId(0)).unwrap_err(),
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
        let pinned =
            plan_pinned_enqueue_locked(&mut tcbs, &classes, 0, CpuId(1)).expect("sentinel");
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
            plan_pinned_enqueue_locked(&mut tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::TaskMissing
        );
        assert_eq!(
            plan_balanced_enqueue_locked(&mut tcbs, &classes, 7, CpuId(0)).unwrap_err(),
            KernelError::TaskMissing
        );
    }

    /// AP-SCHED1 — the home rule: the first placement is recorded, and only the first.
    #[test]
    fn the_first_placement_fixes_the_home_and_nothing_moves_it() {
        let mut tcbs = [Some(tcb(9, None, TaskStatus::Runnable))];
        assert_eq!(fix_home_locked(&mut tcbs, 9, CpuId(1)), CpuId(1));
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, Some(CpuId(1)));
        // A later placement decision — balanced or explicit — returns the home, unchanged.
        assert_eq!(fix_home_locked(&mut tcbs, 9, CpuId(0)), CpuId(1));
        assert_eq!(tcbs[0].as_ref().unwrap().cpu_affinity, Some(CpuId(1)));
        // TID 0 has no TCB and no home.
        assert_eq!(fix_home_locked(&mut tcbs, 0, CpuId(0)), CpuId(0));
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

    /// SCHED-LOCK2A (SC-CPU-2) — replaces `the_pinned_planner_names_its_own_cpu_regardless_of_affinity`,
    /// which documented the defect: the pinned planner placed a homed task wherever it was told.
    /// Now a pinned plan for a homed task must name the home or is refused, with nothing
    /// written; on the home it succeeds and names it.
    #[test]
    fn sl2a_a_pinned_plan_off_the_home_is_refused_with_nothing_written() {
        let mut tcbs = [Some(tcb(9, Some(CpuId(1)), TaskStatus::Runnable))];
        let classes = [Some(TaskClass::Driver)];
        let before = tcbs.clone();
        assert_eq!(
            plan_pinned_enqueue_locked(&mut tcbs, &classes, 9, CpuId(0)).unwrap_err(),
            KernelError::WrongObject
        );
        assert_eq!(tcbs, before, "a refused plan writes nothing");
        let plan = plan_pinned_enqueue_locked(&mut tcbs, &classes, 9, CpuId(1)).expect("home");
        assert_eq!(plan.target, EnqueueTarget::Pinned(CpuId(1)));
        assert_eq!(plan.incarnation, tcbs[0].as_ref().unwrap().incarnation);
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
