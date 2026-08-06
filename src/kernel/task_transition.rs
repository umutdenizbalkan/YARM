// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D-WA3A — production-enforced task status transition barriers.
//!
//! # Why
//!
//! The WA2B census found nine scheduler/lifecycle status writes that could move **any** task,
//! including a `Blocked(EndpointReceive)` receiver, because nothing in the source constrained the
//! previous status (`doc/KERNEL_UNLOCK_AUDIT.md` §6.1.34). The run queue carries bare TIDs and
//! `crates/yarm-kernel/src/scheduler.rs` mentions `TaskStatus` nowhere, so "a queued task is
//! Runnable" was an unstated dynamic invariant rather than an enforced one.
//!
//! This module makes each of those writes an **exact, typed, fail-closed** transition that is
//! compiled into every build. A `debug_assert` would not do: it is compiled out of release
//! kernels, which is precisely where the proof has to hold.
//!
//! # Incarnation identity
//!
//! Where the caller knows which incarnation it means, it passes `expect_asid`. A numeric TID
//! alone never authorizes a transition on a replacement task: a recycled TID under a different
//! address space is a different incarnation and is refused with
//! [`TransitionRefusal::IncarnationMismatch`].
//!
//! # This module takes no lock
//!
//! Every entry point operates on a `&mut [Option<ThreadControlBlock>]` slice the caller already
//! holds under the task rank-2 acquisition. Nothing here acquires, nests or reorders a domain
//! lock, so no path gains a broad-lock acquisition and no task(2) → scheduler(1) inversion is
//! introduced.

use crate::kernel::task::{TaskStatus, ThreadControlBlock};
use crate::kernel::vm::Asid;

/// The idle / bootstrap TID.
///
/// This is the same TID `PriorityScheduler::dispatch_next` special-cases as idle
/// (`current.tid.0 == 0`). It is made `current` by the rank-1 scheduler without ever being
/// marked `Running`, so preempting it out is a `Runnable → Runnable` no-op rather than the
/// ordinary `Running → Runnable`. That case gets its OWN transition
/// ([`TaskTransition::PreemptOutgoingIdle`]) restricted to this TID by construction, so the
/// ordinary-task contract is not weakened to accommodate it.
pub(crate) const IDLE_TID: u64 = 0;

/// The exact transitions the scheduler/lifecycle cohort may perform.
///
/// Each variant names **one** legal `from → to` pair. There is deliberately no "set status"
/// escape hatch: a caller that needs a transition not listed here must add it, with its own
/// census row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskTransition {
    /// A task dequeued for dispatch becomes current: `Runnable → Running`, and nothing else.
    /// A `Blocked(_)`, `Faulted`, `Exited` or `Dead` task in a run queue is a scheduler
    /// invariant break, and dispatching it would be an unarbitrated wake.
    DispatchIncoming,
    /// The already-current task continues across a queue-neutral dispatch: `Running → Running`.
    /// Separated from [`Self::DispatchIncoming`] so the idempotent case cannot be used to
    /// launder a non-`Runnable` task into `Running`.
    ContinueCurrent,
    /// The outgoing current task is preempted or yields: `Running → Runnable`, and nothing else.
    /// A task that has already blocked itself must not be made runnable again here.
    PreemptOutgoing,
    /// Exact rollback of a dispatch this transaction performed: `Running → Runnable`.
    RollbackDispatchedIncoming,
    /// The current task faults: `Running → Faulted`, and nothing else.
    FaultRunningCurrent,
    /// **Idle only.** The idle task ([`IDLE_TID`]) is made `current` by the rank-1 scheduler
    /// without a mark-running step, so preempting it out is `Runnable → Runnable`. Refused for
    /// every other TID, so an ordinary task can never take this branch.
    PreemptOutgoingIdle,
}

impl TaskTransition {
    /// The single status this transition may be applied from.
    pub(crate) const fn expected_from(self) -> TaskStatus {
        match self {
            Self::DispatchIncoming => TaskStatus::Runnable,
            Self::PreemptOutgoingIdle => TaskStatus::Runnable,
            Self::ContinueCurrent
            | Self::PreemptOutgoing
            | Self::RollbackDispatchedIncoming
            | Self::FaultRunningCurrent => TaskStatus::Running,
        }
    }

    /// The status the task ends in.
    pub(crate) const fn resulting(self) -> TaskStatus {
        match self {
            Self::DispatchIncoming | Self::ContinueCurrent => TaskStatus::Running,
            Self::PreemptOutgoing
            | Self::RollbackDispatchedIncoming
            | Self::PreemptOutgoingIdle => TaskStatus::Runnable,
            Self::FaultRunningCurrent => TaskStatus::Faulted,
        }
    }

    /// A short stable name for the refusal marker.
    pub(crate) const fn marker(self) -> &'static str {
        match self {
            Self::DispatchIncoming => "dispatch_incoming",
            Self::ContinueCurrent => "continue_current",
            Self::PreemptOutgoing => "preempt_outgoing",
            Self::RollbackDispatchedIncoming => "rollback_dispatched_incoming",
            Self::FaultRunningCurrent => "fault_running_current",
            Self::PreemptOutgoingIdle => "preempt_outgoing_idle",
        }
    }
}

/// Why a transition was refused. Every variant is fail-closed: **no field of the TCB is
/// written**, so a refusal is observationally identical to never having called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionRefusal {
    /// No TCB holds this TID.
    TaskMissing,
    /// The TID exists but is a different incarnation than the caller meant.
    IncarnationMismatch { observed: Option<Asid> },
    /// The TID and incarnation are right, but the task is not in the one status this
    /// transition may be applied from.
    WrongStatus { observed: TaskStatus },
    /// An idle-only transition was attempted on an ordinary task.
    NotIdleTask,
}

/// Apply `transition` to `tid`, or refuse without mutating anything.
///
/// On success returns the status the task was in (always `transition.expected_from()`), so a
/// caller performing an exact rollback can assert what it undid.
pub(crate) fn apply_task_transition(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
    expect_asid: Option<Asid>,
    transition: TaskTransition,
) -> Result<TaskStatus, TransitionRefusal> {
    if transition == TaskTransition::PreemptOutgoingIdle && tid != IDLE_TID {
        return Err(TransitionRefusal::NotIdleTask);
    }
    let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) else {
        return Err(TransitionRefusal::TaskMissing);
    };
    // Incarnation first: a stale numeric TID must not even be told what status the replacement
    // task is in, let alone be allowed to move it.
    if let Some(expected) = expect_asid
        && tcb.asid != Some(expected)
    {
        return Err(TransitionRefusal::IncarnationMismatch { observed: tcb.asid });
    }
    let observed = tcb.status;
    if observed != transition.expected_from() {
        return Err(TransitionRefusal::WrongStatus { observed });
    }
    tcb.status = transition.resulting();
    Ok(observed)
}

/// Read-only sibling of [`apply_task_transition`]: would the transition be accepted?
///
/// Used where the authoritative scheduler mutation must be validated **before** it happens, so
/// there is nothing to roll back.
pub(crate) fn task_transition_would_be_accepted(
    tcbs: &[Option<ThreadControlBlock>],
    tid: u64,
    expect_asid: Option<Asid>,
    transition: TaskTransition,
) -> Result<TaskStatus, TransitionRefusal> {
    if transition == TaskTransition::PreemptOutgoingIdle && tid != IDLE_TID {
        return Err(TransitionRefusal::NotIdleTask);
    }
    let Some(tcb) = tcbs.iter().flatten().find(|t| t.tid.0 == tid) else {
        return Err(TransitionRefusal::TaskMissing);
    };
    if let Some(expected) = expect_asid
        && tcb.asid != Some(expected)
    {
        return Err(TransitionRefusal::IncarnationMismatch { observed: tcb.asid });
    }
    let observed = tcb.status;
    if observed != transition.expected_from() {
        return Err(TransitionRefusal::WrongStatus { observed });
    }
    Ok(observed)
}

/// Emit the canonical refusal marker. One place, so every refusal is greppable and every site
/// reports the same fields.
pub(crate) fn log_transition_refusal(
    site: &str,
    tid: u64,
    transition: TaskTransition,
    refusal: TransitionRefusal,
) {
    match refusal {
        TransitionRefusal::TaskMissing => crate::yarm_log!(
            "TASK_TRANSITION_REFUSED site={} tid={} transition={} reason=task_missing",
            site,
            tid,
            transition.marker()
        ),
        TransitionRefusal::IncarnationMismatch { observed } => crate::yarm_log!(
            "TASK_TRANSITION_REFUSED site={} tid={} transition={} reason=incarnation_mismatch observed_asid={}",
            site,
            tid,
            transition.marker(),
            observed.map(|a| a.0 as u64).unwrap_or(u64::MAX)
        ),
        TransitionRefusal::NotIdleTask => crate::yarm_log!(
            "TASK_TRANSITION_REFUSED site={} tid={} transition={} reason=not_idle_task",
            site,
            tid,
            transition.marker()
        ),
        TransitionRefusal::WrongStatus { observed } => crate::yarm_log!(
            "TASK_TRANSITION_REFUSED site={} tid={} transition={} reason=wrong_status observed={:?}",
            site,
            tid,
            transition.marker(),
            observed
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::task::WaitReason;

    fn tcb(tid: u64, status: TaskStatus, asid: Option<Asid>) -> ThreadControlBlock {
        let mut t = ThreadControlBlock::new(ThreadId(tid), None);
        t.status = status;
        t.asid = asid;
        t
    }

    fn blocked_receiver() -> TaskStatus {
        TaskStatus::Blocked(WaitReason::EndpointReceive(
            crate::kernel::capabilities::CapId(7),
        ))
    }

    /// Every transition accepts exactly one `from` status and refuses every other, including —
    /// the point of the whole stage — a blocked endpoint receiver.
    #[test]
    fn each_transition_accepts_exactly_one_from_status() {
        let states = [
            TaskStatus::Runnable,
            TaskStatus::Running,
            blocked_receiver(),
            TaskStatus::Blocked(WaitReason::Join(ThreadId(9))),
            TaskStatus::Faulted,
            TaskStatus::Exited(3),
            TaskStatus::Dead,
        ];
        for transition in [
            TaskTransition::DispatchIncoming,
            TaskTransition::ContinueCurrent,
            TaskTransition::PreemptOutgoing,
            TaskTransition::RollbackDispatchedIncoming,
            TaskTransition::FaultRunningCurrent,
        ] {
            for state in states {
                let mut slots = [Some(tcb(5, state, None))];
                let r = apply_task_transition(&mut slots, 5, None, transition);
                if state == transition.expected_from() {
                    assert_eq!(r, Ok(state), "{transition:?} must accept {state:?}");
                    assert_eq!(
                        slots[0].as_ref().expect("tcb").status,
                        transition.resulting()
                    );
                } else {
                    assert_eq!(
                        r,
                        Err(TransitionRefusal::WrongStatus { observed: state }),
                        "{transition:?} must refuse {state:?}"
                    );
                    assert_eq!(
                        slots[0].as_ref().expect("tcb").status,
                        state,
                        "a refused {transition:?} must leave {state:?} untouched"
                    );
                }
            }
        }
    }

    #[test]
    fn a_blocked_endpoint_receiver_is_refused_by_every_transition() {
        for transition in [
            TaskTransition::DispatchIncoming,
            TaskTransition::ContinueCurrent,
            TaskTransition::PreemptOutgoing,
            TaskTransition::RollbackDispatchedIncoming,
            TaskTransition::FaultRunningCurrent,
        ] {
            let mut slots = [Some(tcb(5, blocked_receiver(), None))];
            assert_eq!(
                apply_task_transition(&mut slots, 5, None, transition),
                Err(TransitionRefusal::WrongStatus {
                    observed: blocked_receiver()
                })
            );
            assert_eq!(slots[0].as_ref().expect("tcb").status, blocked_receiver());
        }
    }

    #[test]
    fn a_stale_incarnation_cannot_move_a_replacement_task() {
        let mut slots = [Some(tcb(5, TaskStatus::Runnable, Some(Asid(2))))];
        assert_eq!(
            apply_task_transition(
                &mut slots,
                5,
                Some(Asid(1)),
                TaskTransition::DispatchIncoming
            ),
            Err(TransitionRefusal::IncarnationMismatch {
                observed: Some(Asid(2))
            })
        );
        assert_eq!(slots[0].as_ref().expect("tcb").status, TaskStatus::Runnable);
        // The right incarnation still works.
        assert_eq!(
            apply_task_transition(
                &mut slots,
                5,
                Some(Asid(2)),
                TaskTransition::DispatchIncoming
            ),
            Ok(TaskStatus::Runnable)
        );
    }

    #[test]
    fn a_missing_task_is_refused_without_touching_any_other_slot() {
        let mut slots = [Some(tcb(5, TaskStatus::Runnable, None))];
        assert_eq!(
            apply_task_transition(&mut slots, 6, None, TaskTransition::DispatchIncoming),
            Err(TransitionRefusal::TaskMissing)
        );
        assert_eq!(slots[0].as_ref().expect("tcb").status, TaskStatus::Runnable);
    }

    /// The read-only sibling answers identically and mutates nothing.
    #[test]
    fn the_read_only_probe_agrees_with_the_mutating_form() {
        for state in [
            TaskStatus::Runnable,
            TaskStatus::Running,
            blocked_receiver(),
        ] {
            for transition in [
                TaskTransition::DispatchIncoming,
                TaskTransition::PreemptOutgoing,
            ] {
                let probe = {
                    let slots = [Some(tcb(5, state, None))];
                    task_transition_would_be_accepted(&slots, 5, None, transition)
                };
                let mut slots = [Some(tcb(5, state, None))];
                let applied = apply_task_transition(&mut slots, 5, None, transition);
                assert_eq!(probe, applied, "{transition:?} on {state:?}");
            }
        }
        // …and the probe leaves the slice alone even when it would accept.
        let slots = [Some(tcb(5, TaskStatus::Runnable, None))];
        assert!(
            task_transition_would_be_accepted(&slots, 5, None, TaskTransition::DispatchIncoming)
                .is_ok()
        );
        assert_eq!(slots[0].as_ref().expect("tcb").status, TaskStatus::Runnable);
    }

    #[test]
    fn the_transition_table_is_exactly_the_five_scheduler_lifecycle_pairs() {
        for (transition, from, to) in [
            (
                TaskTransition::DispatchIncoming,
                TaskStatus::Runnable,
                TaskStatus::Running,
            ),
            (
                TaskTransition::ContinueCurrent,
                TaskStatus::Running,
                TaskStatus::Running,
            ),
            (
                TaskTransition::PreemptOutgoing,
                TaskStatus::Running,
                TaskStatus::Runnable,
            ),
            (
                TaskTransition::RollbackDispatchedIncoming,
                TaskStatus::Running,
                TaskStatus::Runnable,
            ),
            (
                TaskTransition::FaultRunningCurrent,
                TaskStatus::Running,
                TaskStatus::Faulted,
            ),
        ] {
            assert_eq!(transition.expected_from(), from);
            assert_eq!(transition.resulting(), to);
        }
    }
}
