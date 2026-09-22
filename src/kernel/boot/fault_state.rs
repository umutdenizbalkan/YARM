// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{FaultBookkeepingMode, KernelError, KernelState, TrapHandleError, kernel_ref};
use crate::arch::hal::Hal;
use crate::kernel::ipc::{Message, ThreadId};
use crate::kernel::syscall::{
    Syscall, SyscallError, complete_blocked_recv_for_waiter, dispatch as dispatch_syscall,
};
use crate::kernel::task::FaultPolicy;
use crate::kernel::task::TaskStatus;
use crate::kernel::trap::{FaultAccess, FaultInfo, Trap, TrapEvent};
use crate::kernel::trapframe::TrapFrame;

/// U9-PF §1 — the pure Phase-A PageFault class.
///
/// Produced by [`KernelState::classify_page_fault_split`] from read-only facts, before any
/// mutation. The variants are exactly the outcomes the existing broad arm already distinguishes;
/// no new user/kernel classification is introduced.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageFaultClass {
    /// A WRITE fault on a page the memory owner marks copy-on-write. The broad arm attempts
    /// `try_handle_cow_fault` first for exactly this case.
    CowCandidate,
    /// A fault `try_handle_demand_page_fault` would attempt. U9-PF routes NONE of these: the
    /// class has zero live witnesses on all three architectures, so it stays broad.
    DemandCandidate,
    /// Neither recovery owner would claim it, so existing policy reaches `PAGE_FAULT_UNHANDLED`
    /// and terminal settlement.
    TerminallyUnhandled,
    /// U9-PAGEFAULT1 §3 — a USER-mode access to a kernel-space address.
    ///
    /// This used to be folded into [`Self::KernelOrAbsentTask`], and the fold was the hazard §3
    /// names: the two have nothing in common except the word "kernel". A user task dereferencing
    /// a kernel address is an ORDINARY user fault — the broad arm declines both recovery owners
    /// (`is_cow_page` is false for it, and `try_handle_demand_page_fault` returns `Ok(false)` at
    /// its own `page.0 >= KERNEL_SPACE_BASE` test), reaches `PAGE_FAULT_UNHANDLED`, reports the
    /// fault and terminates the task. The kernel does not die; the task does.
    ///
    /// Folding it with a supervisor-mode fault would have let a userspace ADDRESS CHOICE decide
    /// the kernel's fate. It is a distinct class so the settlement is chosen by who faulted, not
    /// by which half of the address space they named. Facts ARE available for it — the tid and
    /// the ASID are both known — so it settles through the existing terminal owner.
    UserKernelAddress,
    /// U9-PAGEFAULT1 §3 — a fault this kernel cannot attribute to a running user incarnation:
    /// a SUPERVISOR-mode fault, an absent current task, or a current task with no address space.
    ///
    /// What unites these three — and what `UserKernelAddress` never had in common with them — is
    /// that no `PageFaultFacts` can be built. There is no `{tid, asid}` coordinate to revalidate
    /// against, so no recovery owner may run and no fault report can name a victim.
    ///
    /// It carries WHICH cause produced it, so the three stop being indistinguishable at the
    /// point that has to settle them.
    KernelOrAbsentTask(UnattributableFault),
}

/// U9-PAGEFAULT1 §3 — WHY a fault could not be attributed to a running user incarnation.
///
/// `KernelOrAbsentTask` names one settlement class; this names which of its three causes
/// produced it. The distinction is not cosmetic: a supervisor-mode fault is a kernel bug, an
/// absent current task is a dispatch-state bug, and a task without an address space is a
/// construction bug — and each is reported differently by the fatal path that receives it.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnattributableFault {
    /// The architectural decoder reported a fault taken in supervisor mode.
    SupervisorOrigin,
    /// No task is current on the faulting CPU.
    NoCurrentTask,
    /// A task is current but has no address space, so no mapping could ever be resolved for it.
    NoAddressSpace,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl UnattributableFault {
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::SupervisorOrigin => "supervisor_origin",
            Self::NoCurrentTask => "no_current_task",
            Self::NoAddressSpace => "no_address_space",
        }
    }
}

/// U9-PAGEFAULT3 §2 — why the off-lock classifier produced no `{class, facts}` pair.
///
/// This replaces an `Option`. The routes used to call a helper that mapped BOTH of these to
/// `None` and then mapped `None` to `NotHandled`, which sent the fault to the broad dispatcher
/// — and the two causes do not deserve the same answer at all. One is a kernel bug that must
/// never reach a recovery owner; the other is a lost race that mutated nothing.
///
/// Erasing them into `Option` is what made that conflation invisible: by the time the route
/// saw the value, the reason no longer existed.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageFaultClassifyRefusal {
    /// The fault cannot be attributed to a running user incarnation. Carries WHICH of the three
    /// causes, because they are settled differently and reported differently.
    Unattributable(UnattributableFault),
    /// The identity moved between the classifier's separate rank-local acquisitions, so the
    /// facts describe a departed incarnation. Nothing was read into the marker stream and
    /// nothing was written.
    IdentityChanged,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl PageFaultClassifyRefusal {
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::Unattributable(cause) => cause.marker(),
            Self::IdentityChanged => "identity_changed",
        }
    }
}

/// U9-PF §1 — the read-only facts a classification rests on, captured once under one VM
/// snapshot so a later phase can revalidate against the exact same coordinates.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageFaultFacts {
    /// Explicit — never an ambient `current_cpu` read at a later phase.
    pub cpu: crate::kernel::scheduler::CpuId,
    pub tid: u64,
    /// The incarnation discriminator. See `classify_page_fault_split` on why `{tid, asid}` is
    /// the authoritative coordinate and no new generation field is invented.
    pub asid: crate::kernel::vm::Asid,
    pub page: crate::kernel::vm::VirtAddr,
    pub access: FaultAccess,
    pub mapping_present: bool,
    pub mapping_writable: bool,
    pub cow_marked: bool,
    pub demand_region: bool,
}

/// U9-COW1 §3 — the outcome of the owner-local private-copy COW recovery.
///
/// The split is deliberately three-way, not two-way, because "declined" and "failed" have
/// different rights here. Everything named `Refused*` happened BEFORE the first mutation and may
/// fall through to the unchanged broad dispatcher, which will re-derive the same facts and do
/// whatever it would have done. Everything named `FailedClosed*` happened AFTER a frame was
/// allocated: the allocation has been rolled back exactly, but the broad path must NOT run,
/// because it would allocate a second frame for a fault it never saw declined.
///
/// `Committed` carries both physical addresses so the caller can attest the transition it just
/// performed rather than re-reading state that may already have moved on.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CowRecovery {
    /// The private copy is mapped writable, the refcounts are transitioned, the stale translation
    /// is retired, and the faulting task may resume at the same instruction.
    Committed {
        old_phys: crate::kernel::vm::PhysAddr,
        new_phys: crate::kernel::vm::PhysAddr,
        /// Whether the old frame's shootdown was acknowledged by every target. `false` means the
        /// old frame was deliberately left unreclaimed — never that the mapping is incomplete.
        shootdown_acked: bool,
    },
    /// Pre-mutation: the faulting incarnation is no longer the one that was classified.
    RefusedIdentityChanged,
    /// U9-PAGEFAULT2 §3 — the mapping is GONE. The broad arm's
    /// `.ok_or(KernelError::UserMemoryFault)?` for a COW-marked page with nothing to copy from.
    RefusedMappingAbsent,
    /// U9-PAGEFAULT2 §3 — the mapping is already WRITABLE. Another owner completed the copy
    /// between classification and here, so the faulting write will now succeed: the canonical
    /// answer is to retry the instruction, not to hand the fault anywhere.
    RefusedAlreadyWritable,
    /// U9-PAGEFAULT2 §3 — the page is no longer COW-marked. `is_cow_page` false is the broad
    /// arm's `Ok(false)`: not this family's fault, continue down the handler chain.
    RefusedNotCowMarked,
    /// Pre-mutation: the faulting task has no resolvable CNode, so a cap cannot be minted for it.
    RefusedNoCnode,
    /// Pre-mutation: no frame, no object slot, or no cnode slot. Nothing was left allocated.
    RefusedAllocation,
    /// Post-allocation: the freshly minted cap did not resolve to the frame it was minted for.
    /// The allocation is rolled back.
    ///
    /// Each `FailedClosed*` carries the EXACT `KernelError` the broad arm produces at the same
    /// step, so the trap the caller finally reports is the one it would have reported anyway —
    /// the owner changed, the user-visible error did not.
    FailedClosedResolve(crate::kernel::boot::KernelError),
    /// Post-allocation: the page copy failed. The allocation is rolled back.
    FailedClosedCopy(crate::kernel::boot::KernelError),
    /// Post-allocation: the mapping replacement failed. The allocation is rolled back and the
    /// original mapping is untouched — `map_page` either replaced the entry or it did not.
    FailedClosedRemap(crate::kernel::boot::KernelError),
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl CowRecovery {
    /// True only for the outcomes that mutated nothing and may therefore reach the broad arm.
    pub(crate) fn may_fall_back_to_broad(self) -> bool {
        matches!(
            self,
            Self::RefusedIdentityChanged
                | Self::RefusedMappingAbsent
                | Self::RefusedAlreadyWritable
                | Self::RefusedNotCowMarked
                | Self::RefusedNoCnode
                | Self::RefusedAllocation
        )
    }

    /// The refusal/failure reason as the marker text, so no call site invents its own spelling.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Committed { .. } => "committed",
            Self::RefusedIdentityChanged => "identity_changed",
            Self::RefusedMappingAbsent => "mapping_absent",
            Self::RefusedAlreadyWritable => "already_writable",
            Self::RefusedNotCowMarked => "not_cow_marked",
            Self::RefusedNoCnode => "no_cnode",
            Self::RefusedAllocation => "allocation",
            Self::FailedClosedResolve(_) => "resolve_phys",
            Self::FailedClosedCopy(_) => "copy_frame",
            Self::FailedClosedRemap(_) => "remap",
        }
    }

    /// U9-PAGEFAULT2 §3 — the canonical settlement for a pre-mutation refusal, or `None` for an
    /// outcome that is not one.
    pub(crate) fn pre_mutation_settlement(self) -> Option<PreMutationSettlement> {
        use PreMutationSettlement as S;
        Some(match self {
            // The page became writable under us. This is NOT `RetryInstruction`: the broad arm's
            // `path=already_writable` arm clears the stale COW mark before returning, and only
            // the non-private-copy owner does that. Re-enter it.
            Self::RefusedAlreadyWritable => S::ReenterFamilyOwner,
            // `is_cow_page` false — the broad arm's `Ok(false)`, which tries demand next.
            Self::RefusedNotCowMarked => S::ContinueFamily,
            // The exact incarnation left. The next route re-classifies against whoever is
            // current now, which is the only correct thing to do with this fault.
            Self::RefusedIdentityChanged => S::ContinueFamily,
            // The mapping vanished under us. The broad arm's answer is
            // `.ok_or(KernelError::UserMemoryFault)?` — which is precisely what the
            // non-private-copy owner's `NoMapping` ending already produces, revalidated under
            // rank 5 rather than predicted from a stale read.
            Self::RefusedMappingAbsent => S::ReenterFamilyOwner,
            // No CNode to mint into: the capability the recovery needs cannot be created, which is
            // the broad arm's own answer when `resolve_memory_object_phys` finds no cspace.
            Self::RefusedNoCnode => {
                S::ResourceFailure(crate::kernel::boot::KernelError::InvalidCapability)
            }
            // `alloc_anonymous_memory_object` exhaustion, in the broad arm's own spelling.
            Self::RefusedAllocation => {
                S::ResourceFailure(crate::kernel::boot::KernelError::MemoryObjectFull)
            }
            Self::Committed { .. }
            | Self::FailedClosedResolve(_)
            | Self::FailedClosedCopy(_)
            | Self::FailedClosedRemap(_) => return None,
        })
    }

    /// The error a post-allocation failure carries. `None` for every outcome that is not one —
    /// a committed recovery has no error, and a pre-mutation refusal hands the fault to the broad
    /// arm, which produces its own.
    pub(crate) fn kernel_error(self) -> Option<crate::kernel::boot::KernelError> {
        match self {
            Self::FailedClosedResolve(e)
            | Self::FailedClosedCopy(e)
            | Self::FailedClosedRemap(e) => Some(e),
            _ => None,
        }
    }
}

/// U9-PAGEFAULT2 §3 — what a PRE-MUTATION refusal settles as.
///
/// PAGEFAULT1 mapped every one of these to `NotHandled`, and defended it with "the broad arm
/// re-derives the same facts under its own lock and reaches the same answer". That is a
/// description of a dependency, not a reason to keep it — and its premise is wrong besides,
/// because a broad-lock holder does not exclude the split writers that take the rank-ordered
/// subsystem locks directly.
///
/// Each refusal has a canonical answer that the broad arm itself would reach, and every one of
/// those answers is available here. Which one it is comes from WHICH refusal was raised, which
/// is why the refusal set is split finely enough to tell them apart rather than collapsed into
/// "something changed".
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreMutationSettlement {
    /// The world already satisfies the faulting access — another owner did this work while we
    /// were classifying. Return to the faulting instruction; it succeeds now. Nothing is
    /// published, nothing is woken, no scheduler state moves.
    RetryInstruction,
    /// Not this recovery class's fault after all. Continue down the route order, which is what
    /// the broad handler chain does with its own `Ok(false)`.
    ContinueFamily,
    /// U9-PAGEFAULT2 §3 — the page changed, between classification and the transaction's
    /// revalidation, into the shape THIS family's OTHER owner handles. Re-enter that owner.
    ///
    /// This exists because `RetryInstruction` was wrong for it, and wrong in a way that only
    /// shows up against the broad arm's body. `try_handle_cow_fault`'s already-writable arm does
    /// not merely return success: it calls `clear_cow_page` FIRST and then returns `Ok(true)`.
    /// Settling the same race by returning to the instruction leaves the write to succeed — and
    /// leaves the stale COW mark set, which the broad arm would have cleared. The mark is not
    /// cosmetic: it is what a later `fork` reads to decide whether a page needs copying.
    ///
    /// The owner re-entered is the one the route already calls from its PRE-transaction screen,
    /// so this is the same code path the non-raced form takes, not a second implementation. It
    /// revalidates under its own domain lock, which is what makes the re-entry safe, and it is
    /// entered AT MOST ONCE: its own "raced again" ending continues down the route order rather
    /// than coming back here, so there is no loop.
    ReenterFamilyOwner,
    /// A resource the recovery needs does not exist, and the broad arm propagates this exact
    /// error out of the trap. Nothing was allocated, so there is nothing to roll back.
    ResourceFailure(crate::kernel::boot::KernelError),
}

/// U9-PAGEFAULT1 §1c — the outcome of the COW arm that is NOT the private copy.
///
/// `try_handle_cow_fault` has three endings for a page it accepts as COW-marked, and the split
/// route previously owned only one of them. The other two were folded into a single decline
/// (`mapping_writable || !mapping_present`) that reached the broad dispatcher — yet they are not
/// the same outcome at all, and neither of them is `PAGE_FAULT_UNHANDLED`:
///
/// * **already writable** — `mapping.flags.write` is set, so the broad arm clears the stale COW
///   mark, prints `path=already_writable` and returns `Ok(true)`, i.e. `PAGE_FAULT_HANDLED_COW`.
///   No allocation, no copy, no shootdown.
/// * **no mapping at all** — the broad arm's `.ok_or(KernelError::UserMemoryFault)?` propagates
///   out of `try_handle_cow_fault`, through the trap handler's `map_err` chain, and the trap
///   reports an error. It never reaches the demand attempt and never prints `UNHANDLED`.
///
/// Composing them here is what lets the COW family settle its own outcomes instead of handing
/// two thirds of them away.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CowNonPrivateSettlement {
    /// The stale COW mark was cleared against a mapping that is present and already writable.
    /// The faulting instruction retries and the write now succeeds. Mirrors the broad arm's
    /// `path=already_writable` return exactly: metadata only, nothing allocated.
    MarkCleared { phys: crate::kernel::vm::PhysAddr },
    /// The page is COW-marked with no mapping to copy from. The broad arm reports
    /// `UserMemoryFault` here, so this settles as that error rather than as a fallback.
    NoMapping,
    /// Re-read under the lock, the page is no longer what classification saw: the mark is gone,
    /// or it became the private-copy shape. Nothing was mutated, so the fault continues down the
    /// route order exactly as the broad arm would continue past `Ok(false)`.
    Raced,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl CowNonPrivateSettlement {
    /// The marker text for this settlement, so no call site invents its own spelling.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::MarkCleared { .. } => "already_writable",
            Self::NoMapping => "no_mapping",
            Self::Raced => "raced",
        }
    }
}

/// U9-PAGEFAULT1 §1d — the outcome of the demand arm that is NOT a fresh mapping.
///
/// `try_handle_demand_page_fault`'s present-mapping branch is a stale-translation repair, not an
/// allocation: the software mapping already satisfies the access, so the only thing wrong is the
/// translation the CPU cached. The broad arm repairs it and returns `Ok(true)`, which reaches its
/// handled-demand completion marker. The split route used to decline the whole branch.
///
/// Note which shapes never arrive here. `evaluate_page_fault_class` admits a present mapping as a
/// `DemandCandidate` only when the access is already satisfied, so the broad arm's
/// `!write_satisfied` decline is a CLASS decision taken upstream, not an outcome this settles.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemandStaleTranslation {
    /// The cached translation was retired against a mapping that is present and satisfies the
    /// access. `write` records which repair the broad arm performs for this access: a write fault
    /// widens the intermediate entries and reloads the whole local TLB, any other fault issues
    /// the single-page invalidation.
    Repaired { write: bool },
    /// Re-read under the lock, the mapping is absent, no longer satisfies the access, or the
    /// incarnation changed. Nothing was mutated.
    Raced,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl DemandStaleTranslation {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Repaired { write: true } => "already_writable_after_flush",
            Self::Repaired { write: false } => "invalidated",
            Self::Raced => "raced",
        }
    }
}

/// U9-PAGEFAULT2 §2 — the outcome of ONE fault-report delivery attempt.
///
/// `emit_fault_report_for_fault` is a total function: for every state of the world it either
/// publishes the report somewhere or records that it could not, and then RETURNS. It never
/// declines in a way that leaves the caller to try something else, and it never prevents the
/// faulted task from being terminated. This type is that totality, made explicit.
///
/// The three endings are not ranked. A buffered report that wakes nobody is not a degraded
/// direct delivery, and a delivery failure is not an error the caller must handle — it is what
/// the broad emitter does when reporting cannot succeed, and the task is still terminated after
/// it.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultReportOutcome {
    /// Handed DIRECTLY to a blocked receiver, which is woken exactly once. The broad emitter's
    /// waiter arm.
    DeliveredToWaiter {
        endpoint_idx: usize,
        waiter_tid: u64,
    },
    /// Enqueued on the endpoint's buffer, waking nobody. A valid outcome, not a degraded one.
    Buffered { endpoint_idx: usize },
    /// Reporting could not succeed. The broad emitter records the same and returns; the faulted
    /// task is terminated regardless.
    Failed(FaultReportFailure),
}

/// U9-PAGEFAULT2 §2 — why a fault report could not be published.
///
/// Each of these is a spelling the broad emitter already produces. None of them is a new policy,
/// and none of them changes what happens to the faulted task.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultReportFailure {
    /// Neither a fault-handler nor a supervisor endpoint is registered.
    NoRoute,
    /// The route named an endpoint slot that no longer exists, or whose generation moved.
    EndpointStale,
    /// The endpoint's buffer is full. The broad emitter discovers this INSIDE the enqueue and
    /// prints `..._ENQUEUE_FAIL` + `..._FAIL`; the report is lost and the task still dies.
    BufferFull,
    /// The wire payload would not fit a `Message`.
    MessageBuild,
    /// A waiter was present and the direct hand-off failed — the broad emitter's
    /// `TASK_FAULT_REPORT_BLOCKED_COMPLETE_FAIL` arm, which also does not fall back to the
    /// buffer.
    WaiterDelivery,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl FaultReportFailure {
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::NoRoute => "no_route",
            Self::EndpointStale => "endpoint_stale",
            Self::BufferFull => "buffer_full",
            Self::MessageBuild => "message_build",
            Self::WaiterDelivery => "waiter_delivery",
        }
    }
}

/// U9-FT3 §3 — the outcome of the split terminal task transition.
///
/// Reached only AFTER a buffered report has been published (or after policy determined none was
/// required). Every refusal here is fail-closed: the report stays published, exactly as the broad
/// path leaves it, and there is NO broad fallback — re-entering the broad emitter would publish a
/// second report.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalFaultTransition {
    /// The faulting task is blocked and `Faulted`. The queue advance is NOT performed here:
    /// the EXISTING post-lock drain owns selection and the incoming-context apply, exactly as it
    /// does for FutexWait.
    Committed { faulted_tid: u64 },
    /// The task or its ASID moved before the transition began. Nothing was mutated by this
    /// transition; the already-published report is retained, matching current semantics.
    RefusedIdentityChanged,
    /// The task-transition barrier rejected the victim before the scheduler mutation.
    RefusedTransitionRejected,
    /// The scheduler removed a different task than the one validated.
    RefusedVictimChanged,
}

/// U9-FT3 §1 — the buffered fault-report admission verdict.
///
/// Produced by a read-only preflight. ONLY [`Self::BufferedEligible`] may proceed on the split
/// route; every other verdict is a pre-mutation refusal that leaves the unchanged broad path to
/// handle the fault exactly as it does today.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferedFaultAdmission {
    BufferedEligible {
        endpoint_idx: usize,
        generation: u64,
        queued_before: usize,
        via_fault_handler: bool,
    },
    /// A receiver is waiting on the endpoint. The broad emitter would deliver DIRECTLY to it via
    /// `complete_blocked_recv_for_waiter`, which has no split twin — so this stays broad.
    WaiterPresent {
        endpoint_idx: usize,
    },
    BufferFull {
        endpoint_idx: usize,
    },
    /// The route named an endpoint slot that no longer exists, or whose generation moved.
    EndpointStale {
        endpoint_idx: usize,
    },
    /// Neither a fault-handler nor a supervisor endpoint is registered — the existing
    /// `TASK_FAULT_NO_SUPERVISOR_ROUTE` case.
    NoRoute,
}

/// U9-FT3 §2 — the outcome of the rank-3 buffered commit.
///
/// Every `Refused*` variant is raised BEFORE the enqueue, so broad fallback remains legal.
/// `Buffered` means the report is published and there is no way back.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferedFaultCommit {
    /// Published. `woke` is ALWAYS false on this path: buffered publication wakes nobody, and
    /// that is a valid outcome, not a degraded one.
    Buffered {
        endpoint_idx: usize,
        generation: u64,
        queued_after: usize,
        woke: bool,
    },
    /// A waiter arrived between preflight and commit. Refused before enqueue.
    RefusedWaiterArrived {
        endpoint_idx: usize,
    },
    RefusedBufferFull {
        endpoint_idx: usize,
    },
    RefusedEndpointStale {
        endpoint_idx: usize,
    },
    /// The report payload could not be built — the existing `reason=message` case.
    RefusedMessageBuild,
}

/// U9-FT2 §3 — THE one effective-fault-policy rule.
///
/// The task's `fault_policy_override` wins if set, otherwise the kernel default. Both
/// `KernelState::effective_fault_policy_for` and the off-lock
/// `SharedKernel::read_terminal_fault_policy_shared` delegate here, so the rule exists once.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn evaluate_fault_policy(
    task_override: Option<FaultPolicy>,
    kernel_default: FaultPolicy,
) -> FaultPolicy {
    task_override.unwrap_or(kernel_default)
}

/// U9-FT2 §3 — THE one fault-report route rule.
///
/// Exactly what `emit_fault_report_for_fault` resolves: the fault-handler endpoint if one is
/// registered, otherwise the supervisor endpoint. Returns `(endpoint_idx, via_fault_handler)`;
/// `None` is the `TASK_FAULT_NO_SUPERVISOR_ROUTE` case.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn evaluate_fault_report_route(
    fault_handler_endpoint: Option<usize>,
    supervisor_endpoint: Option<usize>,
) -> Option<(usize, bool)> {
    fault_handler_endpoint
        .map(|idx| (idx, true))
        .or_else(|| supervisor_endpoint.map(|idx| (idx, false)))
}

/// U9-FT2 §2 — THE one demand-backed-region policy, as a pure function of gathered facts.
///
/// `KernelState::fault_addr_in_demand_backed_region` and the off-lock twin both delegate here, so
/// the brk-window and stack-growth-window rules exist in exactly one place. The rule is
/// unchanged: the address is demand-backed if it lies in the task's brk range, or within
/// [`DEMAND_STACK_GROWTH_WINDOW`] below its user stack top.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn evaluate_demand_backed_region(
    brk_bounds: Option<(usize, usize)>,
    user_stack_top: Option<crate::kernel::vm::VirtAddr>,
    fault_addr: u64,
) -> bool {
    if let Some((base, end)) = brk_bounds
        && fault_addr >= base as u64
        && fault_addr < end as u64
    {
        return true;
    }
    user_stack_top
        .map(|top| {
            let low = top.0.saturating_sub(DEMAND_STACK_GROWTH_WINDOW);
            fault_addr >= low && fault_addr < top.0
        })
        .unwrap_or(false)
}

/// U9-FT2 §2 — THE one COW-mark policy, as a pure function of the gathered set membership.
///
/// Trivial by construction, but pinned as a named owner so neither form can drift into a
/// different notion of "is COW" (for example by consulting the PTE write bit instead of the
/// software mark).
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn evaluate_cow_marked(marked_in_owner_set: bool) -> bool {
    marked_in_owner_set
}

/// U9-FT2 §2 — the kernel/fallback boundary predicate, in ONE place.
///
/// Evaluated by BOTH classification forms BEFORE any VM read, exactly where the original broad
/// classifier evaluated it, so a kernel-space fault still costs no rank-5 acquisition. It is a
/// pure function of the address; `FaultInfo` carries no privilege-origin bit and none is
/// invented.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn page_fault_addr_is_kernel_space(page: crate::kernel::vm::VirtAddr) -> bool {
    page.0 >= crate::kernel::vm::KERNEL_SPACE_BASE
}

/// U9-FT2 §2 — THE one pure PageFault classification evaluator.
///
/// A free function of named facts: no `self`, no lock, no allocation, no mutation. Both the broad
/// `KernelState::classify_page_fault_split` and the off-lock
/// `SharedKernel::classify_page_fault_shared` delegate here, which is what makes them
/// mechanically equivalent rather than merely similar.
///
/// The caller is responsible for the identity screen (absent task / absent ASID) and for
/// [`page_fault_addr_is_kernel_space`], because both are decided before any VM read; this
/// evaluator owns the COW / demand / terminal decision and nothing else. The order is the broad
/// arm's order: COW first and only for writes, then the demand screen, then the terminal
/// fall-through.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn evaluate_page_fault_class(facts: PageFaultFacts) -> PageFaultClass {
    // U9-PAGEFAULT1 §3 — the kernel-space ADDRESS screen, and it comes first.
    //
    // Both fact-gathering forms already return `UserKernelAddress` before they reach here, so
    // this is the BACKSTOP rather than the producer — and it has to exist, because without it
    // the recovery owners' refusal of a kernel address would remain INCIDENTAL:
    // `try_handle_cow_fault` refuses one only because no kernel page is ever COW-marked, and
    // `try_handle_demand_page_fault` only because no kernel page is ever inside a demand-backed
    // region. Both are true today and neither is a rule. Testing the address here makes the
    // refusal a derived property of the class rather than a coincidence of two data structures,
    // so a future caller that builds facts some third way still cannot reach a recovery owner
    // with a kernel address.
    if page_fault_addr_is_kernel_space(facts.page) {
        return PageFaultClass::UserKernelAddress;
    }
    // COW is attempted FIRST and ONLY for writes, mirroring the broad arm.
    if matches!(facts.access, FaultAccess::Write) && facts.cow_marked {
        return PageFaultClass::CowCandidate;
    }
    // The demand screen, mirroring `try_handle_demand_page_fault`'s pre-mutation checks:
    // execute faults decline, the address must be demand-backed, and a PRESENT mapping only
    // qualifies when it already satisfies the faulting access (a write fault on a present
    // read-only page is a protection/COW fault, not a demand fault).
    if !matches!(facts.access, FaultAccess::Execute) && facts.demand_region {
        let write_satisfied = !matches!(facts.access, FaultAccess::Write) || facts.mapping_writable;
        if !facts.mapping_present || write_satisfied {
            return PageFaultClass::DemandCandidate;
        }
    }
    // Neither recovery owner would claim it, so existing policy reaches
    // `PAGE_FAULT_UNHANDLED` and the terminal fault transaction.
    PageFaultClass::TerminallyUnhandled
}

/// U9-PAGEFAULT1 §2c — the outcome of the owner-local DEMAND recovery.
///
/// The same three-way split `CowRecovery` uses, and for the same reason: "declined" and "failed"
/// have different rights. Everything named `Refused*` happened BEFORE the first mutation and may
/// fall through to the unchanged broad dispatcher, which will re-derive the same facts and do
/// whatever it would have done. Everything named `FailedClosed*` happened AFTER a frame was
/// allocated: the allocation has been rolled back exactly, but the broad path must NOT run,
/// because it would allocate a SECOND frame for a fault it never saw declined.
///
/// A demand recovery is strictly simpler than a COW one — there is no old frame, no page copy, no
/// refcount transition and no shootdown, because nothing was mapped at this address before. What
/// it keeps unchanged is the allocation/rollback discipline, step for step.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemandRecovery {
    /// An anonymous page is mapped `USER_RW` at the faulting address and the task may resume at
    /// the same instruction.
    Committed { phys: crate::kernel::vm::PhysAddr },
    /// Pre-mutation: the faulting incarnation is no longer the one that was classified.
    RefusedIdentityChanged,
    /// Pre-mutation: a mapping is already present. Another owner serviced this page between
    /// classification and here, so this route has nothing to install — and installing anyway
    /// would replace a translation it did not create.
    RefusedMappingPresent,
    /// Pre-mutation: the address is no longer inside a demand-backed region. The brk window can
    /// shrink, and a route that ignored that would map a page the task no longer owns.
    RefusedNotDemandRegion,
    /// Pre-mutation: the faulting task has no resolvable CNode, so a cap cannot be minted for it.
    RefusedNoCnode,
    /// Pre-mutation: no frame, no object slot, or no cnode slot. Nothing was left allocated.
    ///
    /// This is the arm the first live witness run exercised for real: init's address space was at
    /// `MAX_MAPPINGS`, so the broad twin's equivalent step answered
    /// `VM_FULL reason=mapping_bookkeeping_full`.
    RefusedAllocation,
    /// Post-allocation: the freshly minted cap did not resolve to the frame it was minted for.
    /// The allocation is rolled back.
    ///
    /// Each `FailedClosed*` carries the EXACT `KernelError` the broad arm produces at the same
    /// step, so the trap the caller finally reports is the one it would have reported anyway.
    FailedClosedResolve(crate::kernel::boot::KernelError),
    /// Post-allocation: installing the mapping failed. The allocation is rolled back and the
    /// address space is untouched — `map_page` either inserted the entry or it did not.
    FailedClosedMap(crate::kernel::boot::KernelError),
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl DemandRecovery {
    /// True only for the outcomes that mutated nothing and may therefore reach the broad arm.
    pub(crate) fn may_fall_back_to_broad(self) -> bool {
        matches!(
            self,
            Self::RefusedIdentityChanged
                | Self::RefusedMappingPresent
                | Self::RefusedNotDemandRegion
                | Self::RefusedNoCnode
                | Self::RefusedAllocation
        )
    }

    /// The refusal/failure reason as the marker text, so no call site invents its own spelling.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Committed { .. } => "committed",
            Self::RefusedIdentityChanged => "identity_changed",
            Self::RefusedMappingPresent => "mapping_present",
            Self::RefusedNotDemandRegion => "not_demand_region",
            Self::RefusedNoCnode => "no_cnode",
            Self::RefusedAllocation => "allocation",
            Self::FailedClosedResolve(_) => "resolve_failed",
            Self::FailedClosedMap(_) => "map_failed",
        }
    }

    /// U9-PAGEFAULT2 §3 — the canonical settlement for a pre-mutation refusal.
    pub(crate) fn pre_mutation_settlement(self) -> Option<PreMutationSettlement> {
        use PreMutationSettlement as S;
        Some(match self {
            // Another owner installed the mapping. The route's PRE-transaction screen sends
            // exactly this shape to the stale-translation owner, which retires the cached
            // negative walk before the instruction retries — returning straight to the
            // instruction would skip that and re-fault on the stale walk.
            Self::RefusedMappingPresent => S::ReenterFamilyOwner,
            // The brk window moved. Not a demand page any more; the terminal owner takes it,
            // exactly as the broad handler's `Ok(false)` leads to `PAGE_FAULT_UNHANDLED`.
            Self::RefusedNotDemandRegion => S::ContinueFamily,
            Self::RefusedIdentityChanged => S::ContinueFamily,
            Self::RefusedNoCnode => {
                S::ResourceFailure(crate::kernel::boot::KernelError::InvalidCapability)
            }
            Self::RefusedAllocation => {
                S::ResourceFailure(crate::kernel::boot::KernelError::MemoryObjectFull)
            }
            Self::Committed { .. } | Self::FailedClosedResolve(_) | Self::FailedClosedMap(_) => {
                return None;
            }
        })
    }

    /// The error a `FailedClosed*` outcome must report, or `None` for everything else.
    pub(crate) fn failed_closed_error(self) -> Option<crate::kernel::boot::KernelError> {
        match self {
            Self::FailedClosedResolve(e) | Self::FailedClosedMap(e) => Some(e),
            _ => None,
        }
    }
}

/// U9-PF §1 — the authorized routing matrix, in ONE place.
///
/// A class is routed off the broad dispatcher only where an EXISTING live witness proves the
/// route at base `adcf229`. Measured there: x86_64 witnesses COW `path=private_copy` twice
/// (ASIDs 1 and 13) under the `VM_COW=1` profile; AArch64 witnesses the terminal user fault once
/// (tid 1, read at `0x0`) in its core profile; the demand class witnesses ZERO faults on all
/// three architectures. Everything else stays on the unchanged broad path, refused before any
/// mutation.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageFaultRoute {
    /// x86_64 COW candidate — the owner-local COW transaction.
    SplitCow,
    /// AArch64 terminal user fault — the owner-local terminal fault transaction.
    SplitTerminal,
    /// U9-PAGEFAULT1 §2c — demand candidate, the owner-local demand recovery.
    SplitDemand,
    /// The unchanged broad path, entered before any mutation.
    Broad,
}

/// The single evaluator of that matrix. Architecture is a parameter rather than a `cfg!` read so
/// the matrix is exhaustively testable for all three ports from the hosted suite.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn page_fault_route_for(arch: &str, class: PageFaultClass) -> PageFaultRoute {
    match (arch, class) {
        // Live-witnessed at base: 2 x private_copy COW faults under VM_COW=1.
        ("x86_64", PageFaultClass::CowCandidate) => PageFaultRoute::SplitCow,
        // U9-A64-COW2 §3/§4 — live-witnessed here for the first time: 6 x private_copy COW
        // faults per boot with parent and child both completing the userspace isolation check.
        //
        // The witness could not exist before. AArch64's post-Fork COW workload died at the FIRST
        // recovered fault, because `apply_restored_thread_state`'s argument mirror zeroed x0..x5
        // on every non-syscall resume (U9-A64-COW2 §1/§2). With that repaired the class is
        // ordinary, and it needs no second policy: the transaction, its refusal set and its
        // rollback are architecture-neutral, and the two arch-specific obligations are already
        // discharged. `arch_map_page` ends in AArch64's own
        // `dsb ishst; tlbi vaae1is; dsb ish; isb` — an inner-shareable BROADCAST plus completion
        // plus context synchronization — so by the time the transaction returns the new
        // translation is visible domain-wide and this PE is synchronized, and the `eret` to EL0
        // is itself context-synchronizing. `complete_unmap_shootdown_split` therefore gates only
        // the OLD-frame reclaim on AArch64, which is exactly the fail-safe direction.
        ("aarch64", PageFaultClass::CowCandidate) => PageFaultRoute::SplitCow,
        // U9-PAGEFAULT1 §2 — RISC-V, live-witnessed here for the first time.
        //
        // This row read "RISC-V is deliberately absent — it has no independent COW witness of its
        // own", and that was exactly right at the time. It has one now, and it needed no new
        // workload: `run_vm_cow_fork_witness_early` was already architecture-neutral and already
        // dispatched on every port, and RISC-V already provisions the slot-13 selector it reads.
        // What was missing was a profile that armed BOTH the oracle knob and the sender-wake
        // sub-knob together, which is what `provision_init_ipc_recv_proof_sender_wake_e2`
        // requires.
        //
        // Measured baseline, row absent: SIX COW faults per boot — two forks, parent and child
        // each writing the same virtual address in its own ASID, with both isolation checks
        // passing from userspace — every one of them reaching the broad dispatcher. The
        // transaction is architecture-neutral and unchanged; RISC-V needed none of the
        // per-architecture obligations the AArch64 row documents, because its `arch_map_page`
        // ends in its own `sfence.vma` and the `sret` to U-mode is itself synchronizing.
        ("riscv64", PageFaultClass::CowCandidate) => PageFaultRoute::SplitCow,
        // Live-witnessed at base: 1 x terminal user read at 0x0 in the core profile.
        //
        // U9-PAGEFAULT1 §2 — x86_64 and RISC-V join it, each on a witness of its own.
        //
        // This row read "AArch64 only" because AArch64 was the only port with a terminal fault to
        // watch. That was measured, not assumed: the x86_64 core profile produces ZERO page
        // faults of any class, so there was nothing to admit. §2 gave the terminal-fault oracle
        // — the same deliberate unhandled read at address 0, the same slot-5 selector, the same
        // default-off knob AArch64 has used since 199E-A64CALL — a provisioning point on the
        // other two ports, and measured each BASELINE first: one fault per boot, reaching the
        // broad dispatcher, with the report buffered to the supervisor endpoint and the task
        // terminated. The rows were added after that, against the chain the baseline printed.
        //
        // The route itself needed no per-architecture policy. What it needed was the queue-
        // advance drain on each bridge to admit the `Faulted` outgoing state, which the AArch64
        // drain has done since U9-FT4 — the x86_64 drain's comment already claimed it did, and
        // did not.
        ("x86_64" | "aarch64" | "riscv64", PageFaultClass::TerminallyUnhandled) => {
            PageFaultRoute::SplitTerminal
        }
        // U9-PAGEFAULT1 §2c/§3 — the demand class, on all three architectures.
        //
        // This row read "EVERY demand candidate stays broad, on every architecture: zero live
        // witnesses" and that was accurate: nothing in any profile produced a demand fault, so
        // the class could not be admitted on evidence. §3 built the witness — `VmBrk` grows the
        // break and leaves the pages lazy, so touching inside the grown window is a demand fault
        // by construction — and measured the baseline on each port, all three 8/8 recovered with
        // the faulting instruction retried and the register file intact.
        //
        // The transaction is architecture-neutral in substance, not by assertion: it allocates a
        // frame, mints a cap and installs one `USER_RW` mapping through the same owners the COW
        // transaction already drives on every port, and it needs none of the per-architecture
        // obligations COW does — no old frame, no page copy, no refcount transition, no
        // shootdown, because nothing was mapped at the faulting address before. The one
        // architecture-sensitive step, dropping a cached negative walk, is the same
        // `invalidate_page` the broad demand handler calls at the same point.
        ("x86_64" | "aarch64" | "riscv64", PageFaultClass::DemandCandidate) => {
            PageFaultRoute::SplitDemand
        }
        // Any other architecture keeps the unchanged broad path.
        (_, PageFaultClass::DemandCandidate) => PageFaultRoute::Broad,
        // U9-PAGEFAULT1 §3 — a user access to a kernel address is an ORDINARY user fault and
        // settles through the terminal owner, on the port that has one.
        //
        // Its broad settlement is byte-for-byte the terminal settlement: both recovery owners
        // decline it, `PAGE_FAULT_UNHANDLED` prints, the report is emitted and the task is
        // terminated. The class exists to make that arrival REASONED rather than incidental —
        // and to keep the kernel's own fate out of a userspace address choice.
        ("x86_64" | "aarch64" | "riscv64", PageFaultClass::UserKernelAddress) => {
            PageFaultRoute::SplitTerminal
        }
        // No other architecture/class pair has a witness.
        _ => PageFaultRoute::Broad,
    }
}

/// U9-FT §2 — where a fault report would be published, captured as a named fact.
///
/// The route is exactly what `emit_fault_report_for_fault` resolves: the fault-handler endpoint
/// if one is registered, otherwise the supervisor endpoint. `generation` is the endpoint's real
/// generation, read from the IPC owner — it is NOT fabricated, and it is the only generation
/// this snapshot carries because it is the only one source provides for this transaction.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FaultReportTarget {
    pub endpoint_idx: usize,
    pub generation: u64,
    /// True when the route resolved to the fault-handler endpoint, false for the supervisor
    /// endpoint. Preserved because the existing markers print it as `target=`.
    pub via_fault_handler: bool,
    pub waiters_before: usize,
    pub queued_before: usize,
}

/// U9-FT §2 — the owner-local terminal-fault policy snapshot.
///
/// A NAMED snapshot, never a positional tuple, carrying only source-authoritative facts needed
/// for terminal settlement. It carries no invented generation: `ThreadControlBlock` has no
/// general task-incarnation counter (U9-PF §1), so the task coordinate is `{tid, asid}` and the
/// only generation present is the endpoint's.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalFaultPolicySnapshot {
    pub tid: u64,
    pub asid: Option<crate::kernel::vm::Asid>,
    pub status: TaskStatus,
    pub policy: FaultPolicy,
    /// `None` when neither a fault-handler nor a supervisor endpoint is registered — exactly the
    /// case the existing emitter reports as `TASK_FAULT_NO_SUPERVISOR_ROUTE`.
    pub target: Option<FaultReportTarget>,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl TerminalFaultPolicySnapshot {
    /// Does existing policy require the faulting task to be terminated? `NotifyAndContinue`
    /// reports and lets the task continue; `KillTask` blocks it and marks it `Faulted`.
    pub(crate) fn terminates_task(self) -> bool {
        matches!(self.policy, FaultPolicy::KillTask)
    }
}

/// U9-FT §2 — a typed refusal raised BEFORE any mutation.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalFaultPolicyRefusal {
    /// No task is current on this CPU.
    NoCurrentTask,
    /// The named tid is not the current task — a stale identity.
    NotCurrentTask,
    /// The tid has no TCB.
    TaskNotFound,
}

#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
impl TerminalFaultPolicyRefusal {
    /// U9-PAGEFAULT3 §3 — which settlement this refusal takes, as the marker prints it.
    ///
    /// `NotCurrentTask` no longer reads `retry_instruction`. The refusal is raised because the
    /// owner has proved the faulting task is not this CPU's current task, so the one thing the
    /// route may not do is return through that task's frame — and the marker says which
    /// settlement actually runs rather than restating an outcome the route stopped producing.
    pub(crate) fn settlement_marker(self) -> &'static str {
        match self {
            Self::NotCurrentTask => "authenticated_entering_frame",
            Self::NoCurrentTask | Self::TaskNotFound => "fatal_task_missing",
        }
    }
}

const STRICT_UNKNOWN_TRAPS: bool = !cfg!(feature = "hosted-dev");
const DEMAND_STACK_GROWTH_WINDOW: u64 = 8 * 1024 * 1024;
#[allow(dead_code)]
const DEBUG_TIMER_LOG: bool = false;

// Stage 137: arch-specific PTE flag check for demand-page verification.
// Returns true iff the PTE grants user-mode read access (and write if need_write).
#[cfg(target_arch = "x86_64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let writable = (pte.0 & PageTableEntry::WRITABLE) != 0;
    user && (!need_write || writable)
}

#[cfg(target_arch = "aarch64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let read_only = (pte.0 & PageTableEntry::READ_ONLY) != 0;
    user && (!need_write || !read_only)
}

#[cfg(target_arch = "riscv64")]
fn demand_pte_flags_ok(
    pte: crate::arch::selected_isa::page_table::PageTableEntry,
    need_write: bool,
) -> bool {
    use crate::arch::selected_isa::page_table::PageTableEntry;
    let user = (pte.0 & PageTableEntry::USER) != 0;
    let readable = (pte.0 & PageTableEntry::READ) != 0;
    let writable = (pte.0 & PageTableEntry::WRITE) != 0;
    user && readable && (!need_write || writable)
}
/// Supervisor fault notification wire ABI payload length.
///
/// Layout (little-endian):
/// - bytes [0..8): faulting tid (u64)
/// - bytes [8..16): fault address (u64)
/// - byte [16]: access kind (0=Read, 1=Write, 2=Execute)
pub(crate) const SUPERVISOR_FAULT_REPORT_WIRE_LEN: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisorFaultReportWire {
    pub(crate) faulting_tid: u64,
    pub(crate) fault_addr: u64,
    pub(crate) access: FaultAccess,
}

impl SupervisorFaultReportWire {
    pub(crate) fn encode(self) -> [u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN] {
        let mut payload = [0u8; SUPERVISOR_FAULT_REPORT_WIRE_LEN];
        payload[..8].copy_from_slice(&self.faulting_tid.to_le_bytes());
        payload[8..16].copy_from_slice(&self.fault_addr.to_le_bytes());
        payload[16] = match self.access {
            FaultAccess::Read => 0,
            FaultAccess::Write => 1,
            FaultAccess::Execute => 2,
        };
        payload
    }

    // Stage 174: available in production (used by the fault-delivery proof to
    // verify the delivered report round-trips) as well as tests.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SUPERVISOR_FAULT_REPORT_WIRE_LEN {
            return None;
        }
        let mut tid = [0u8; 8];
        let mut addr = [0u8; 8];
        tid.copy_from_slice(&bytes[..8]);
        addr.copy_from_slice(&bytes[8..16]);
        let access = match bytes[16] {
            0 => FaultAccess::Read,
            1 => FaultAccess::Write,
            2 => FaultAccess::Execute,
            _ => return None,
        };
        Some(Self {
            faulting_tid: u64::from_le_bytes(tid),
            fault_addr: u64::from_le_bytes(addr),
            access,
        })
    }
}

impl KernelState {
    fn endpoint_fault_report_waiter(&self, endpoint_idx: usize) -> Option<ThreadId> {
        self.with_ipc_state(|ipc| ipc.endpoint_waiter_tid(endpoint_idx))
    }

    fn endpoint_fault_report_stats(&self, endpoint_idx: usize) -> Option<(u64, usize, usize)> {
        self.with_ipc_state(|ipc| {
            let generation = *ipc.endpoint_generations.get(endpoint_idx)?;
            let queued = ipc
                .endpoints
                .get(endpoint_idx)?
                .as_ref()
                .map(|endpoint| kernel_ref(endpoint).queued())?;
            let waiters = usize::from(ipc.endpoint_waiter_present(endpoint_idx));
            Some((generation, waiters, queued))
        })
    }

    fn fault_addr_in_demand_backed_region(&self, tid: u64, fault_addr: u64) -> bool {
        // U9-FT2 §2: gather the facts here, but leave the RULE to the one owner that the
        // off-lock twin also calls.
        let brk_bounds = self.task_brk_bounds(tid);
        let user_stack_top = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.user_stack_top)
        });
        evaluate_demand_backed_region(brk_bounds, user_stack_top, fault_addr)
    }

    /// Stage 163H: proof-gated, fully-decoded page-table-entry diagnostic. Logs the
    /// SOFTWARE shadow flags (writable / cow / demand-region) alongside the ACTIVE
    /// hardware CR3's decoded PTE bits (present / writable / user / nx + raw) for the
    /// faulting page, so a software-vs-hardware mismatch is unambiguous. The hardware
    /// walk reads the REAL active CR3 (`read_hw_cr3`), not an ASID-indexed resolve,
    /// so it reflects exactly what the CPU walks.
    fn pf_proof_log_hw_pte(
        &self,
        label: &str,
        tid: u64,
        asid: crate::kernel::vm::Asid,
        page: crate::kernel::vm::VirtAddr,
    ) {
        let sw = self.with_user_spaces(|s| s.get(asid).and_then(|a| a.resolve(page)));
        let sw_writable = sw.map(|m| m.flags.write as u8).unwrap_or(0);
        let sw_cow = self.is_cow_page(asid, page) as u8;
        let sw_demand = self.fault_addr_in_demand_backed_region(tid, page.0) as u8;
        #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
        {
            let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
            let hw_root = hw_cr3 & !0xfffu64;
            let (_pml4e, _pdpte, _pde, hw_pte) =
                crate::arch::x86_64::page_table::hw_pte_walk_verbose(hw_root, page.0);
            crate::yarm_log!(
                "{} tid={} asid={} va=0x{:x} cr3=0x{:x} raw=0x{:x} present={} writable={} user={} nx={} cow_sw={} writable_sw={} demand_sw={}",
                label,
                tid,
                asid.0,
                page.0,
                hw_cr3,
                hw_pte,
                (hw_pte & 1) as u8,
                ((hw_pte >> 1) & 1) as u8,
                ((hw_pte >> 2) & 1) as u8,
                ((hw_pte >> 63) & 1) as u8,
                sw_cow,
                sw_writable,
                sw_demand
            );
        }
        #[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
        {
            crate::yarm_log!(
                "{} tid={} asid={} va=0x{:x} cow_sw={} writable_sw={} demand_sw={} hw_pte=unavailable_target",
                label,
                tid,
                asid.0,
                page.0,
                sw_cow,
                sw_writable,
                sw_demand
            );
        }
    }

    /// U9-FT §2 — the owner-local terminal-fault policy read.
    ///
    /// Takes `&self`: it performs no IPC, scheduler or VM mutation and cannot allocate. A stale
    /// or missing task is a TYPED refusal raised before any mutation, never a panic and never a
    /// silent default.
    ///
    /// ONE POLICY IMPLEMENTATION. The policy itself is not re-derived here — it delegates to the
    /// existing `effective_fault_policy_for`, which is the same function the broad
    /// `fault_current_task_with_fault` consults. There is exactly one FaultPolicy implementation
    /// and both routes read it.
    ///
    /// NO CPU PARAMETER. Terminal fault policy does not depend on the CPU: it is the task's
    /// `fault_policy_override` if set, else the kernel default. Threading a `CpuId` through would
    /// imply a dependence that source does not have. The caller's CPU identity is carried by
    /// `PageFaultFacts` instead, where it IS load-bearing.
    ///
    /// RANKS. Three SEQUENTIAL acquisitions, never nested in reverse: task (rank 2) for identity
    /// and status, fault (rank 8) for the endpoint route, IPC (rank 3) for the endpoint
    /// generation and queue state. `effective_fault_policy_for` internally nests fault (8) inside
    /// task (2), which is ascending and therefore legal; that nesting is preserved exactly rather
    /// than restructured.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn read_terminal_fault_policy_split(
        &self,
        tid: u64,
    ) -> Result<TerminalFaultPolicySnapshot, TerminalFaultPolicyRefusal> {
        // Validate the exact current task the way the existing terminal path does: it resolves
        // `current_tid()` and faults THAT task, so a caller naming a different tid is stale.
        let current = self
            .current_tid()
            .ok_or(TerminalFaultPolicyRefusal::NoCurrentTask)?;
        if current != tid {
            return Err(TerminalFaultPolicyRefusal::NotCurrentTask);
        }
        // Rank 2 — identity and status, from the one task owner.
        let found = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| (tcb.asid, tcb.status))
        });
        let Some((asid, status)) = found else {
            return Err(TerminalFaultPolicyRefusal::TaskNotFound);
        };
        // The ONE policy implementation, shared with the broad path.
        let policy = self.effective_fault_policy_for(tid);
        // Rank 8 — the endpoint route, resolved exactly as the existing emitter resolves it:
        // fault-handler first, then supervisor.
        let route = self.with_fault_state(|faults| {
            evaluate_fault_report_route(faults.fault_handler_endpoint, faults.supervisor_endpoint)
        });
        // Rank 3 — the endpoint's REAL generation and queue state. A route that names an
        // endpoint slot which no longer exists yields `None`, exactly the stale-supervisor case
        // the existing emitter refuses to publish into.
        let target = route.and_then(|(endpoint_idx, via_fault_handler)| {
            self.endpoint_fault_report_stats(endpoint_idx).map(
                |(generation, waiters_before, queued_before)| FaultReportTarget {
                    endpoint_idx,
                    generation,
                    via_fault_handler,
                    waiters_before,
                    queued_before,
                },
            )
        });
        Ok(TerminalFaultPolicySnapshot {
            tid,
            asid,
            status,
            policy,
            target,
        })
    }

    /// U9-PF §1 — the PURE Phase-A PageFault classification.
    ///
    /// Takes `&self`, so no mutation of PTE, task, capability or refcount state is structurally
    /// expressible, and it allocates nothing. It reproduces the ORDER of the existing broad
    /// `TrapEvent::PageFault` arm exactly — COW first and only for writes, then the demand
    /// screen, then the terminal fall-through — so the split route and the broad route can never
    /// disagree about which class a fault belongs to.
    ///
    /// Both verdict predicates it consults are themselves pure: `is_cow_page` reads
    /// `memory.cow_pages`, and `fault_addr_in_demand_backed_region` reads the brk bounds and
    /// `user_stack_top`. Every `Ok(false)` decline in both recovery handlers is reached before
    /// that handler's first mutation, which is what makes this extraction sound.
    ///
    /// IDENTITY. There is no per-task incarnation counter on `ThreadControlBlock` — the only
    /// generations it carries are per-purpose (`blocked_recv_generation`,
    /// `blocked_send_generation`, `async_preempt_generation`, `reply_record_generation`). The
    /// authoritative incarnation coordinate for a fault is therefore `{tid, asid}`, exactly as
    /// both existing handlers use it: a reused numeric TID occupying a fresh address space
    /// carries a different ASID. No new generation field is invented here.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn classify_page_fault_split(
        &self,
        cpu: crate::kernel::scheduler::CpuId,
        fault: FaultInfo,
    ) -> (PageFaultClass, Option<PageFaultFacts>) {
        // U9-PAGEFAULT1 §2 — THE ORIGIN TEST, and it comes first.
        //
        // This function used to record that "`FaultInfo` carries no privilege-origin bit and the
        // broad arm performs no origin test, so none is invented here". The bit exists now, set
        // by each decoder from the architectural source — x86_64 `#PF` error-code bit 2, AArch64
        // `ESR_EC_*_LOW` versus `_CUR`, RISC-V from a bridge that admits no supervisor fault at
        // all — so the test is no longer an invention.
        //
        // It precedes the current-task and address reads DELIBERATELY. Those two answer "is there
        // a user task and is this a user address", and a kernel fault on a user address with a
        // user task current satisfies both. Classifying it from them would hand a kernel bug to
        // the recovery owners, which would mint a frame, replace a mapping and resume the kernel
        // at the faulting instruction as though a user page had been demanded.
        if matches!(fault.origin, crate::kernel::trap::FaultOrigin::Supervisor) {
            return (
                PageFaultClass::KernelOrAbsentTask(UnattributableFault::SupervisorOrigin),
                None,
            );
        }
        // Two more causes reach the same class, and for the same structural reason: without a
        // `{tid, asid}` coordinate no facts can be built, so no owner can revalidate and no
        // report can name a victim.
        let Some(tid) = self.current_tid() else {
            return (
                PageFaultClass::KernelOrAbsentTask(UnattributableFault::NoCurrentTask),
                None,
            );
        };
        let Some(asid) = self.task_asid(tid) else {
            return (
                PageFaultClass::KernelOrAbsentTask(UnattributableFault::NoAddressSpace),
                None,
            );
        };
        // U9-PAGEFAULT1 §3 — the kernel-space ADDRESS test, still decided HERE, before any VM
        // read, and still costing no rank-5 acquisition. What changed is its ANSWER.
        //
        // It used to `return (KernelOrAbsentTask, None)`, which let a user task's choice of
        // address decide whether the kernel could attribute the fault at all. It can: the tid and
        // the ASID are both in hand one line above. What the address decides is that NO RECOVERY
        // OWNER MAY RUN — a class, not an absence of facts.
        //
        // The mapping facts are recorded ABSENT rather than looked up, and that is a statement
        // about this class, not a shortcut: no recovery owner may consult them, and the terminal
        // owner that settles this class reads only `{tid, asid}`. Looking them up would buy a
        // rank-5 and a rank-6 acquisition for a fault whose settlement cannot depend on either.
        let page = fault.addr.page_align_down();
        if page_fault_addr_is_kernel_space(page) {
            return (
                PageFaultClass::UserKernelAddress,
                Some(PageFaultFacts {
                    cpu,
                    tid,
                    asid,
                    page,
                    access: fault.access,
                    mapping_present: false,
                    mapping_writable: false,
                    cow_marked: false,
                    demand_region: false,
                }),
            );
        }
        let mapping = self.with_user_spaces(|s| s.get(asid).and_then(|a| a.resolve(page)));
        let cow_marked = self.is_cow_page(asid, page);
        let demand_region = self.fault_addr_in_demand_backed_region(tid, page.0);
        let facts = PageFaultFacts {
            cpu,
            tid,
            asid,
            page,
            access: fault.access,
            mapping_present: mapping.is_some(),
            mapping_writable: mapping.map(|m| m.flags.write).unwrap_or(false),
            cow_marked,
            demand_region,
        };
        // U9-FT2 §2: the COW / demand / terminal decision belongs to the ONE evaluator, which
        // the off-lock twin also calls. This form owns only the fact-gathering.
        (evaluate_page_fault_class(facts), Some(facts))
    }

    pub(crate) fn try_handle_demand_page_fault(
        &mut self,
        fault: crate::arch::trap::FaultInfo,
    ) -> Result<bool, KernelError> {
        if matches!(fault.access, FaultAccess::Execute) {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let Some(asid) = self.task_asid(tid) else {
            return Ok(false); // No user address space → not a demand-paged fault
        };
        let page = fault.addr.page_align_down();
        if page.0 >= crate::kernel::vm::KERNEL_SPACE_BASE {
            return Ok(false);
        }
        if !self.fault_addr_in_demand_backed_region(tid, page.0) {
            return Ok(false);
        }
        let existing = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(crate::kernel::vm::VmError::InvalidAsid))?
            .resolve(page);
        if let Some(mapping) = existing {
            // Stage 163G fix: the page is already in the address space. Only treat
            // this as a demand fault (stale-TLB re-walk) if the EXISTING mapping
            // actually satisfies the faulting access. A WRITE fault on a present
            // read-only page is a protection/COW fault, NOT a demand fault — masking
            // it here (INVLPG + claim handled) would loop forever on an unchanged
            // RO PTE. Decline so the caller routes it to COW / task-fault instead.
            let write_satisfied =
                !matches!(fault.access, FaultAccess::Write) || mapping.flags.write;
            if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                crate::yarm_log!(
                    "PF_PROOF_DEMAND_CONSIDER tid={} asid={} va=0x{:x} write_fault={} sw_writable={} write_satisfied={}",
                    tid,
                    asid.0,
                    page.0,
                    matches!(fault.access, FaultAccess::Write) as u8,
                    mapping.flags.write as u8,
                    write_satisfied as u8
                );
            }
            if !write_satisfied {
                if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                    crate::yarm_log!(
                        "PF_PROOF_DEMAND_DECLINE tid={} asid={} va=0x{:x} reason=present_write_not_satisfied",
                        tid,
                        asid.0,
                        page.0
                    );
                }
                return Ok(false);
            }
            // Stage 137: the page is already in VmSpace but the TLB may hold a
            // stale not-present entry from the original fault.  INVLPG flushes
            // that entry so the CPU re-walks the hardware page table and finds
            // the valid PTE instead of re-faulting indefinitely.
            let proof = crate::kernel::boot::ipc_recv_proof_sender_wake_active();
            if matches!(fault.access, FaultAccess::Write) {
                // Stage 163I: a WRITE fault on a page that is ALREADY present
                // and writable is not a fresh demand map — it is a stale /
                // under-permissioned-translation loop. The leaf PTE is writable,
                // so plain per-page INVLPG provably does not clear it (observed
                // present+write+user error 0x7 recurring with cr3 unchanged and
                // the leaf raw PTE = ...007). Two real causes are repaired here:
                //   1. an intermediate paging entry that lacks USER|WRITABLE, so
                //      the AND-of-levels check denies the write regardless of the
                //      leaf (repair_user_path_intermediates widens it in place);
                //   2. a stale local-CPU TLB entry the single-page INVLPG missed
                //      (flush_tlb_local_full reloads CR3 to drop the whole space).
                if proof {
                    crate::yarm_log!(
                        "PF_PROOF_TLB_STALE_CANDIDATE tid={} asid={} va=0x{:x} sw_writable=1",
                        tid,
                        asid.0,
                        page.0
                    );
                }
                let repaired =
                    crate::arch::selected_isa::page_table::repair_user_path_intermediates(
                        asid, page,
                    );
                if proof {
                    crate::yarm_log!(
                        "PF_PROOF_INTERMEDIATE_REPAIR tid={} asid={} va=0x{:x} levels_upgraded={}",
                        tid,
                        asid.0,
                        page.0,
                        repaired
                    );
                    crate::yarm_log!("PF_PROOF_INVLPG_BEGIN va=0x{:x}", page.0);
                }
                crate::arch::selected_isa::page_table::invalidate_page(page);
                if proof {
                    crate::yarm_log!("PF_PROOF_INVLPG_DONE va=0x{:x}", page.0);
                    crate::yarm_log!("PF_PROOF_CR3_RELOAD_BEGIN va=0x{:x}", page.0);
                }
                crate::arch::selected_isa::page_table::flush_tlb_local_full();
                if proof {
                    crate::yarm_log!("PF_PROOF_CR3_RELOAD_DONE va=0x{:x}", page.0);
                    crate::yarm_log!(
                        "PF_PROOF_DEMAND_HANDLE_OK tid={} asid={} va=0x{:x} reason=already_writable_after_flush",
                        tid,
                        asid.0,
                        page.0
                    );
                }
                return Ok(true);
            }
            crate::arch::selected_isa::page_table::invalidate_page(page);
            return Ok(true);
        }

        let (_id, mem_cap) = self.alloc_anonymous_memory_object()?;
        let flags = crate::kernel::vm::PageFlags::USER_RW;
        // Stage 8: asid resolved plan-first above (line 98); identical to
        // map_user_page_in_current_asid_with_caps under the global lock since
        // current_tid cannot change between the plan-first resolution and here.
        self.map_user_page_in_asid_with_caps(asid, mem_cap, page, flags)?;

        #[cfg(feature = "hosted-dev")]
        self.with_memory_state_mut(|memory| {
            for byte in 0..crate::kernel::vm::PAGE_SIZE {
                memory.user_memory.insert((asid.0, page.0 + byte as u64), 0);
            }
        });

        Ok(true)
    }

    fn emit_fault_report_for_fault(&mut self, faulted_tid: u64, fault: FaultInfo) {
        crate::yarm_log!("TASK_FAULT_REPORT_BEGIN tid={}", faulted_tid);
        // Stage 174 (FAULT-DELIVERY): default-off delivery-lifecycle markers. The
        // endpoint routing / message build / direct-recv / queue behavior below is
        // UNCHANGED; the markers only expose the phase boundaries.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_ENDPOINT_LOOKUP_BEGIN");
        }
        let route = self.with_fault_state(|faults| {
            faults
                .fault_handler_endpoint
                .map(|endpoint_idx| (endpoint_idx, "fault-handler"))
                .or_else(|| {
                    faults
                        .supervisor_endpoint
                        .map(|endpoint_idx| (endpoint_idx, "supervisor"))
                })
        });
        let Some((endpoint_idx, target)) = route else {
            if fault_delivery {
                crate::yarm_log!("FAULT_DELIVERY_ENDPOINT_LOOKUP_FAIL reason=missing");
            }
            crate::yarm_log!(
                "TASK_FAULT_NO_SUPERVISOR_ROUTE tid={} reason=no-fault-or-supervisor-endpoint",
                faulted_tid
            );
            return;
        };
        if fault_delivery {
            crate::yarm_log!(
                "FAULT_DELIVERY_ENDPOINT_LOOKUP_OK endpoint={}",
                endpoint_idx
            );
        }
        let Some((generation, waiters_before, queued_before)) =
            self.endpoint_fault_report_stats(endpoint_idx)
        else {
            if fault_delivery {
                // Route resolved to an endpoint slot that no longer exists — a
                // stale supervisor fault channel.
                crate::yarm_log!(
                    "FAULT_DELIVERY_STALE_SUPERVISOR endpoint={} target={}",
                    endpoint_idx,
                    target
                );
            }
            crate::yarm_log!(
                "TASK_FAULT_REPORT_ENQUEUE_FAIL tid={} endpoint={} reason=missing-endpoint",
                faulted_tid,
                endpoint_idx
            );
            return;
        };
        crate::yarm_log!(
            "TASK_FAULT_REPORT_TARGET tid={} endpoint={} generation={}",
            faulted_tid,
            endpoint_idx,
            generation
        );
        crate::yarm_log!(
            "TASK_FAULT_REPORT_QUEUE_STATE_BEFORE endpoint={} waiters={} queued={}",
            endpoint_idx,
            waiters_before,
            queued_before
        );

        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_MSG_BUILD_BEGIN tid={}", faulted_tid);
        }
        let payload = SupervisorFaultReportWire {
            faulting_tid: faulted_tid,
            fault_addr: fault.addr.0,
            access: fault.access,
        }
        .encode();

        let msg = match Message::new(0, &payload) {
            Ok(msg) => msg,
            Err(_) => {
                if fault_delivery {
                    crate::yarm_log!("FAULT_DELIVERY_MSG_BUILD_FAIL reason=message");
                }
                crate::yarm_log!("TASK_FAULT_REPORT_FAIL tid={} reason=message", faulted_tid);
                return;
            }
        };
        if fault_delivery {
            crate::yarm_log!(
                "FAULT_DELIVERY_MSG_BUILD_OK tid={} bytes={}",
                faulted_tid,
                msg.len
            );
        }
        crate::yarm_log!(
            "TASK_FAULT_REPORT_SENDER tid={} sender_tid=0 opcode={} len={}",
            faulted_tid,
            msg.opcode,
            msg.len
        );
        if let Some(waiter_tid) = self.endpoint_fault_report_waiter(endpoint_idx) {
            crate::yarm_log!(
                "TASK_FAULT_REPORT_BLOCKED_WAITER_FOUND endpoint={} waiter_tid={}",
                endpoint_idx,
                waiter_tid.0
            );
            if self.is_task_recv_v2_blocked(waiter_tid.0) {
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_BLOCKED_COMPLETE_BEGIN endpoint={} waiter_tid={}",
                    endpoint_idx,
                    waiter_tid.0
                );
                if fault_delivery {
                    crate::yarm_log!(
                        "FAULT_DELIVERY_DIRECT_RECV_BEGIN supervisor_tid={}",
                        waiter_tid.0
                    );
                }
                match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                    Ok(()) => {
                        if fault_delivery {
                            crate::yarm_log!(
                                "FAULT_DELIVERY_DIRECT_RECV_WRITEBACK_OK supervisor_tid={}",
                                waiter_tid.0
                            );
                        }
                        self.ipc_clear_plain_receiver_waiter_only(endpoint_idx, waiter_tid);
                        crate::yarm_log!(
                            "TASK_FAULT_REPORT_WAKE_RUNNABLE endpoint={} waiter_tid={}",
                            endpoint_idx,
                            waiter_tid.0
                        );
                        match self.apply_split_receiver_wake_plan(waiter_tid) {
                            Ok(()) => {
                                let (generation_after, waiters_after, queued_after) = self
                                    .endpoint_fault_report_stats(endpoint_idx)
                                    .unwrap_or((generation, usize::MAX, usize::MAX));
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_QUEUE_STATE_AFTER endpoint={} waiters={} queued={}",
                                    endpoint_idx,
                                    waiters_after,
                                    queued_after
                                );
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_BLOCKED_COMPLETE_OK endpoint={} waiter_tid={}",
                                    endpoint_idx,
                                    waiter_tid.0
                                );
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_SENT tid={} target={}",
                                    faulted_tid,
                                    target
                                );
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_SENT tid={} target={} endpoint={} generation={}",
                                    faulted_tid,
                                    target,
                                    endpoint_idx,
                                    generation_after
                                );
                                if fault_delivery {
                                    // Direct completion delivered the report inline —
                                    // NOTHING was left queued (no duplicate/stranding)
                                    // and no waiter left orphaned on the endpoint.
                                    if queued_after != 0 && queued_after != usize::MAX {
                                        crate::yarm_log!(
                                            "FAULT_DELIVERY_DUPLICATE_MSG fault_tid={} endpoint={} queued={}",
                                            faulted_tid,
                                            endpoint_idx,
                                            queued_after
                                        );
                                    }
                                    if waiters_after != 0 && waiters_after != usize::MAX {
                                        crate::yarm_log!(
                                            "FAULT_DELIVERY_ORPHANED_WAITER endpoint={} waiters={}",
                                            endpoint_idx,
                                            waiters_after
                                        );
                                    }
                                    crate::yarm_log!(
                                        "FAULT_DELIVERY_DIRECT_RECV_DONE fault_tid={} supervisor_tid={}",
                                        faulted_tid,
                                        waiter_tid.0
                                    );
                                }
                            }
                            Err(err) => {
                                if fault_delivery {
                                    crate::yarm_log!(
                                        "FAULT_DELIVERY_WRITEBACK_FAIL supervisor_tid={} reason={:?}",
                                        waiter_tid.0,
                                        err
                                    );
                                }
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_BLOCKED_COMPLETE_FAIL endpoint={} waiter_tid={} reason={:?}",
                                    endpoint_idx,
                                    waiter_tid.0,
                                    err
                                );
                                crate::yarm_log!(
                                    "TASK_FAULT_REPORT_FAIL tid={} reason={:?}",
                                    faulted_tid,
                                    err
                                );
                            }
                        }
                    }
                    Err(err) => {
                        if fault_delivery {
                            crate::yarm_log!(
                                "FAULT_DELIVERY_WRITEBACK_FAIL supervisor_tid={} reason={:?}",
                                waiter_tid.0,
                                err
                            );
                        }
                        crate::yarm_log!(
                            "TASK_FAULT_REPORT_BLOCKED_COMPLETE_FAIL endpoint={} waiter_tid={} reason={:?}",
                            endpoint_idx,
                            waiter_tid.0,
                            err
                        );
                        crate::yarm_log!(
                            "TASK_FAULT_REPORT_FAIL tid={} reason={:?}",
                            faulted_tid,
                            err
                        );
                    }
                }
                return;
            }
        }

        crate::yarm_log!(
            "TASK_FAULT_REPORT_ENQUEUE_BEGIN tid={} endpoint={} generation={}",
            faulted_tid,
            endpoint_idx,
            generation
        );
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_QUEUE_BEGIN fault_tid={}", faulted_tid);
        }

        // send_message_to_endpoint_and_wake enqueues under ipc_state_lock
        // (rank 3) and wakes outside the lock (task lock rank 2 < ipc rank 3).
        match self.send_message_to_endpoint_and_wake(endpoint_idx, msg) {
            Ok(()) => {
                let (generation_after, waiters_after, queued_after) = self
                    .endpoint_fault_report_stats(endpoint_idx)
                    .unwrap_or((generation, usize::MAX, usize::MAX));
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_QUEUE_STATE_AFTER endpoint={} waiters={} queued={}",
                    endpoint_idx,
                    waiters_after,
                    queued_after
                );
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_ENQUEUE_OK tid={} endpoint={} queued={} woke={}",
                    faulted_tid,
                    endpoint_idx,
                    queued_after,
                    usize::from(waiters_before > 0)
                );
                if fault_delivery {
                    crate::yarm_log!("FAULT_DELIVERY_QUEUE_OK fault_tid={}", faulted_tid);
                }
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_SENT tid={} target={}",
                    faulted_tid,
                    target
                );
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_SENT tid={} target={} endpoint={} generation={}",
                    faulted_tid,
                    target,
                    endpoint_idx,
                    generation_after
                );
            }
            Err(err) => {
                if fault_delivery {
                    crate::yarm_log!("FAULT_DELIVERY_QUEUE_FULL fault_tid={}", faulted_tid);
                }
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_ENQUEUE_FAIL tid={} endpoint={} reason={:?}",
                    faulted_tid,
                    endpoint_idx,
                    err
                );
                crate::yarm_log!(
                    "TASK_FAULT_REPORT_FAIL tid={} reason={:?}",
                    faulted_tid,
                    err
                );
            }
        }
    }

    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let fault = self.with_fault_state(|faults| faults.last_fault);
        let Some(fault) = fault else {
            return;
        };
        self.emit_fault_report_for_fault(faulted_tid, fault);
    }

    fn fault_current_task_for_fault(&mut self, fault: FaultInfo) -> Result<(), KernelError> {
        self.fault_current_task_with_fault(Some(fault))
    }

    /// Hosted/test-only entry to the private fault path, so the WA3A transition-barrier
    /// tests can drive the real production code rather than a copy of it.
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn fault_current_task_for_test(&mut self) -> Result<(), KernelError> {
        self.fault_current_task()
    }

    fn fault_current_task(&mut self) -> Result<(), KernelError> {
        let fault = self.with_fault_state(|faults| faults.last_fault);
        self.fault_current_task_with_fault(fault)
    }

    fn fault_current_task_with_fault(
        &mut self,
        fault_opt: Option<FaultInfo>,
    ) -> Result<(), KernelError> {
        let cpu = self.current_cpu();
        // Diagnostic: log the fault before acting. TrapEvent::PageFault callers
        // pass the current FaultInfo explicitly so report/log behavior does not
        // depend on re-reading global last_fault; legacy syscall/raw callers can
        // still pass the diagnostic last_fault snapshot.
        {
            let cur_tid = self.current_tid().unwrap_or(u64::MAX);
            crate::yarm_log!(
                "TASK_FAULT_CURRENT tid={} fault_addr=0x{:x} access={:?}",
                cur_tid,
                fault_opt.map(|f| f.addr.0).unwrap_or(0),
                fault_opt.map(|f| f.access)
            );
        }
        let running_tid = self.current_tid().ok_or_else(|| {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!(
                    "TASK_MISSING site=fault_current_task/current_tid cpu={}",
                    cpu.0
                );
            }
            KernelError::TaskMissing
        })?;
        if let Some(fault) = fault_opt {
            self.emit_fault_report_for_fault(running_tid, fault);
        } else {
            self.emit_fault_report(running_tid);
        }

        if self.effective_fault_policy_for(running_tid) == FaultPolicy::NotifyAndContinue {
            return Ok(());
        }

        // Stage 174 (FAULT-DELIVERY): default-off task-stop markers. The
        // block-current + Faulted transition below is UNCHANGED.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_STOP_BEGIN tid={}", running_tid);
        }
        // Stage 199D-WA3A: validate the victim BEFORE the authoritative scheduler mutation.
        // `block_current_cpu` clears `current` at rank 1 and is not undone below, so a status
        // check placed after it would be a check after an irreversible commit. The exact
        // precondition — the fault victim is the current task and is still `Running` — is
        // therefore evaluated at rank 2 first, and a refusal touches neither domain.
        if let Err(refusal) = self.with_tcbs(|tcbs| {
            crate::kernel::task_transition::task_transition_would_be_accepted(
                tcbs,
                running_tid,
                None,
                crate::kernel::task_transition::TaskTransition::FaultRunningCurrent,
            )
        }) {
            crate::kernel::task_transition::log_transition_refusal(
                "fault_current_task_with_fault",
                running_tid,
                crate::kernel::task_transition::TaskTransition::FaultRunningCurrent,
                refusal,
            );
            return Err(KernelError::TaskMissing);
        }
        let faulted_tid = self.block_current_cpu().ok_or_else(|| {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!(
                    "TASK_MISSING site=fault_current_task/block_current cpu={}",
                    cpu.0
                );
            }
            KernelError::TaskMissing
        })?;
        // The scheduler must have blocked out exactly the task we validated. Anything else is
        // a different victim and is refused before the status write.
        if faulted_tid != running_tid {
            crate::yarm_log!(
                "TASK_TRANSITION_REFUSED site=fault_current_task_with_fault tid={} transition=fault_running_current reason=victim_changed observed_tid={}",
                running_tid,
                faulted_tid
            );
            return Err(KernelError::TaskMissing);
        }
        self.with_tcbs_mut(|tcbs| {
            crate::kernel::task_transition::apply_task_transition(
                tcbs,
                faulted_tid,
                None,
                crate::kernel::task_transition::TaskTransition::FaultRunningCurrent,
            )
            .map_err(|refusal| {
                crate::kernel::task_transition::log_transition_refusal(
                    "fault_current_task_with_fault",
                    faulted_tid,
                    crate::kernel::task_transition::TaskTransition::FaultRunningCurrent,
                    refusal,
                );
                KernelError::TaskMissing
            })?;
            Ok::<_, KernelError>(())
        })?;
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_STOP_OK tid={}", faulted_tid);
        }
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    /// Canonical 199E-R1F — an UNSUPPORTED user instruction faults the offending task, and only
    /// that task.
    ///
    /// This is the fail-closed terminal for the FP/vector policy. Userspace is built soft-float
    /// and every U-mode return forces `sstatus.FS`/`VS` Off, so a floating-point or vector
    /// instruction reaching the CPU raises an illegal-instruction trap. It must not be resumed
    /// (the instruction would trap again forever), it must not be emulated, and it must not take
    /// the whole kernel down for one task's bad instruction.
    ///
    /// It therefore routes into the EXISTING per-task user-fault policy verbatim — the same
    /// `fault_current_task_with_fault` a user-unhandled page fault uses: emit the fault report,
    /// honour `FaultPolicy` (`NotifyAndContinue` reports and returns; `KillTask` validates the
    /// victim, blocks it, marks it `Faulted`), and dispatch a replacement. No new mechanism, no
    /// new policy knob, and no second way for a task to die.
    ///
    /// `pc` is the faulting instruction's address, reported as an `Execute` fault because that is
    /// what happened: the instruction at that address could not be executed. It is deliberately
    /// NOT reported as a page fault — the mapping is fine; the instruction is not supported.
    pub(crate) fn fault_current_task_unsupported_instruction(
        &mut self,
        pc: crate::kernel::vm::VirtAddr,
    ) -> Result<(), KernelError> {
        crate::yarm_log!(
            "USER_UNSUPPORTED_INSTRUCTION tid={} pc=0x{:x} policy=fail_closed reason=fp_or_vector_off result=ok",
            self.current_tid().unwrap_or(u64::MAX),
            pc.0
        );
        // U9-PAGEFAULT1 §2: a USER instruction the kernel refused to emulate, at a user PC.
        self.fault_current_task_for_fault(crate::kernel::trap::FaultInfo::user(
            pc,
            crate::kernel::trap::FaultAccess::Execute,
        ))
    }

    #[cfg(test)]
    pub(crate) fn emit_fault_report_for_fault_for_test(
        &mut self,
        faulted_tid: u64,
        fault: crate::kernel::trap::FaultInfo,
    ) {
        self.emit_fault_report_for_fault(faulted_tid, fault);
    }

    /// Stage 174 (FAULT-DELIVERY): one-shot, self-contained fault-delivery proof.
    ///
    /// Runs at most once (a `compare_exchange` latch) when `yarm.fault_delivery=1`
    /// and a real user task (tid != 0) with a CNode is current. It exercises the
    /// classify → message-build → endpoint-lookup → queue → dequeue → invariant
    /// lifecycle on a SCRATCH endpoint — it never touches the real supervisor
    /// fault channel, never faults a real task, and fully tears down (revokes the
    /// scratch caps + frees the endpoint slot) so it consumes no net slots and
    /// leaves no residual state. Diagnostic only: it changes no fault/IPC behavior
    /// and swallows all errors into a `FAULT_DELIVERY_*` marker.
    ///
    /// The dequeue reads the endpoint queue DIRECTLY (`Endpoint::recv`) rather than
    /// via the `ipc_recv` syscall path, so it never blocks or dispatches the live
    /// current task. The real classify/direct-recv/queue/task-stop markers are
    /// emitted on the live fault path (see `handle_trap` / `emit_fault_report_for_fault`
    /// / `fault_current_task_with_fault`); this proof makes the required markers
    /// deterministic even when no real user fault occurs during the smoke.
    pub(crate) fn maybe_run_fault_delivery_proof(&mut self) {
        if !crate::kernel::boot::fault_delivery_enabled() {
            return;
        }
        let Some(tid) = self.current_tid() else {
            return;
        };
        if tid == 0 {
            return; // need a real user task with a CNode
        }
        let Some(cnode) = self.current_task_cnode() else {
            return;
        };
        if !crate::kernel::boot::fault_delivery_proof_try_start() {
            return; // one-shot
        }
        crate::yarm_log!("FAULT_DELIVERY_PROOF_BEGIN tid={}", tid);

        // Scratch faulting identity — a synthetic tid that is NOT a real task, so
        // no real service is disturbed by the demonstration.
        let scratch_fault_tid = 0xF17E_0001u64;
        let scratch_addr = 0xDEAD_0000u64;

        // Classification: demonstrate the user-unhandled decision deterministically.
        // (The live classifier in `handle_trap` emits the same marker on real
        // faults, and CLASSIFY_HANDLED kind=cow/demand on handled faults.)
        crate::yarm_log!(
            "FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED tid={} vector=14 addr=0x{:x} access={:?}",
            scratch_fault_tid,
            scratch_addr,
            FaultAccess::Write
        );

        // Message build: real wire encode of the supervisor fault report.
        crate::yarm_log!("FAULT_DELIVERY_MSG_BUILD_BEGIN tid={}", scratch_fault_tid);
        let payload = SupervisorFaultReportWire {
            faulting_tid: scratch_fault_tid,
            fault_addr: scratch_addr,
            access: FaultAccess::Write,
        }
        .encode();
        let msg = match Message::new(0, &payload) {
            Ok(m) => {
                crate::yarm_log!(
                    "FAULT_DELIVERY_MSG_BUILD_OK tid={} bytes={}",
                    scratch_fault_tid,
                    m.len
                );
                m
            }
            Err(_) => {
                crate::yarm_log!("FAULT_DELIVERY_MSG_BUILD_FAIL reason=message");
                crate::yarm_log!(
                    "FAULT_DELIVERY_PROOF_DONE tid={} result=msg_build_fail",
                    tid
                );
                return;
            }
        };

        // Endpoint: create a SCRATCH endpoint (self-contained; not the real
        // supervisor). Its send/recv caps live in the current cnode only until the
        // teardown at the end of the proof.
        crate::yarm_log!("FAULT_DELIVERY_ENDPOINT_LOOKUP_BEGIN");
        let (endpoint_idx, send_cap, recv_cap) = match self.create_endpoint(4) {
            Ok(triple) => triple,
            Err(_) => {
                crate::yarm_log!("FAULT_DELIVERY_ENDPOINT_LOOKUP_FAIL reason=missing");
                crate::yarm_log!("FAULT_DELIVERY_PROOF_DONE tid={} result=endpoint_fail", tid);
                return;
            }
        };
        crate::yarm_log!(
            "FAULT_DELIVERY_ENDPOINT_LOOKUP_OK endpoint={}",
            endpoint_idx
        );

        // Queue exactly one fault message (no waiter → queued path).
        crate::yarm_log!("FAULT_DELIVERY_QUEUE_BEGIN fault_tid={}", scratch_fault_tid);
        let queued_after_send = match self.send_message_to_endpoint_and_wake(endpoint_idx, msg) {
            Ok(()) => {
                crate::yarm_log!("FAULT_DELIVERY_QUEUE_OK fault_tid={}", scratch_fault_tid);
                self.with_ipc_state(|ipc| {
                    ipc.endpoints[endpoint_idx]
                        .as_ref()
                        .map(|ep| kernel_ref(ep).queued())
                        .unwrap_or(0)
                })
            }
            Err(_) => {
                crate::yarm_log!("FAULT_DELIVERY_QUEUE_FULL fault_tid={}", scratch_fault_tid);
                0
            }
        };

        // Dequeue exactly once — directly from the endpoint queue (does NOT block
        // or dispatch the current task; the syscall recv path is avoided on purpose).
        crate::yarm_log!("FAULT_DELIVERY_DEQUEUE_BEGIN supervisor_tid={}", tid);
        let dequeued = self.with_ipc_state_mut(|ipc| {
            ipc.endpoints[endpoint_idx]
                .as_mut()
                .and_then(|ep| super::kernel_mut(ep).recv())
        });
        let queued_after_recv = self.with_ipc_state(|ipc| {
            ipc.endpoints[endpoint_idx]
                .as_ref()
                .map(|ep| kernel_ref(ep).queued())
                .unwrap_or(0)
        });
        match dequeued
            .as_ref()
            .and_then(|m| SupervisorFaultReportWire::decode(m.as_slice()))
        {
            Some(decoded) if decoded.faulting_tid == scratch_fault_tid => {
                crate::yarm_log!(
                    "FAULT_DELIVERY_DEQUEUE_OK fault_tid={} supervisor_tid={}",
                    decoded.faulting_tid,
                    tid
                );
            }
            Some(decoded) => {
                // The delivered report carries the wrong faulting identity — the
                // sender/identity integrity invariant is broken.
                crate::yarm_log!(
                    "FAULT_DELIVERY_BAD_SENDER expected={} got={}",
                    scratch_fault_tid,
                    decoded.faulting_tid
                );
            }
            None => {
                crate::yarm_log!(
                    "FAULT_DELIVERY_WRITEBACK_FAIL supervisor_tid={} reason=decode",
                    tid
                );
            }
        }

        // Invariants: exactly one queued, then exactly one dequeued; nothing
        // stranded and no duplicate left behind.
        if queued_after_send != 1 {
            crate::yarm_log!(
                "FAULT_DELIVERY_QUEUE_LEAK endpoint={} queued_after_send={}",
                endpoint_idx,
                queued_after_send
            );
        }
        if queued_after_recv != 0 {
            crate::yarm_log!(
                "FAULT_DELIVERY_STRANDED_QUEUE endpoint={} queued_after_recv={}",
                endpoint_idx,
                queued_after_recv
            );
        }
        if queued_after_send == 1 && queued_after_recv == 0 && dequeued.is_some() {
            crate::yarm_log!("FAULT_DELIVERY_INVARIANT_OK tid={}", tid);
        }

        // Teardown: revoke the scratch caps and free the endpoint slot so the proof
        // leaves NO residual state (consumes no net cap slots or endpoints).
        let _ = self.revoke_capability_in_cnode(cnode, send_cap);
        let _ = self.revoke_capability_in_cnode(cnode, recv_cap);
        self.with_ipc_state_mut(|ipc| {
            ipc.endpoints[endpoint_idx] = None;
        });
        crate::yarm_log!("FAULT_DELIVERY_PROOF_DONE tid={} result=ok", tid);
    }

    pub fn handle_trap(
        &mut self,
        trap: Trap,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        match trap {
            Trap::Syscall => {
                self.clear_last_fault();
                let trapframe = frame.ok_or(TrapHandleError::MissingTrapFrame)?;
                let _ = self.sync_current_thread_from_frame(trapframe);
                // Stage 178B (CROSS-ARCH-D6): reliable arch-neutral hook for the
                // one-shot read-only cross-arch D6 restore-path audit. The syscall
                // entry ALWAYS runs with the syscalling user task current (tid != 0)
                // and its trapframe just synced — unlike the timer tick, whose
                // `tid != 0` gate is not satisfied on the AArch64/RISC-V idle-context
                // tick, so the timer-only hook never fired there. The one-shot latch
                // makes this a single audit regardless of which path reaches it first.
                // Read-only + default-off (`yarm.cross_arch_d6=1`); it live-wires no
                // restore and changes no syscall behavior.
                self.maybe_run_cross_arch_d6_audit();
                // Stage 184 (CROSS-ARCH-LIVE): default-on, one-shot cross-arch live
                // audit — attests the honest per-arch topology + graduated D2/D6/D3 +
                // syscall parity. Observability only; live-wires nothing.
                self.maybe_run_cross_arch_live_audit();
                // Stage 179 (D3-FULL): one-shot self-contained D3 VM anon-map/unmap
                // proof on the reliable arch-neutral syscall path (a real user task is
                // current). Default-off (`yarm.d3_full=1`); drives the real VM
                // primitives on a scratch ASID with full teardown — no production VM
                // ABI change, local flush live, remote shootdown deferred.
                self.maybe_run_d3_full_proof();
                // Stage 181 (GRADUATE-KNOBS): one-shot verification of the graduated
                // x86_64 -smp1 unlock (syscall path; reliable across arches). Verifies
                // the accepted seam gates are on + the D3 scratch check; DEFERS under
                // SMP. The gates were set at cmdline apply — this only reports.
                self.maybe_run_unlock_graduated_proof();
                // Stage 183 (SMP-LIVE): one-shot read-only x86_64 SMP-liveness audit
                // (syscall path; shared one-shot latch with the timer-path hook).
                self.maybe_run_x86_smp_unlock_audit();
                // Encode normal user syscall errors into the frame instead of
                // propagating as TrapHandleError. All three arch entry points
                // (AArch64 yarm_aarch64_vector_entry, x86_64 halt_forever,
                // RISC-V) treat Err(TrapHandleError) as a fatal kernel halt.
                // Normal SyscallError values (InvalidArgs, MissingRight, …)
                // must be returned to userspace as x0/error_code, not halt the kernel.
                if let Err(e) = dispatch_syscall(self, trapframe) {
                    trapframe.set_err(e.code());
                }
                if trapframe.error_code() == Some(SyscallError::PageFault.code()) {
                    self.fault_current_task()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::TimerInterrupt => {
                self.hal.acknowledge_interrupt(self.current_cpu(), 0);
                // x86_64: During bootstrap, borrow_kernel_for_boot() holds a raw
                // &mut KernelState without the SpinLock. The timer ISR acquires the
                // SpinLock via with_cpu(), creating aliased mutable references — UB.
                // Guard: skip tick/yield until signal_bootstrap_scheduler_ready() is
                // called (after all user tasks are spawned and enqueued). EOI + re-arm
                // keeps the timer alive without corrupting mid-bootstrap kernel state.
                #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
                if !crate::arch::x86_64::descriptor_tables::bootstrap_scheduler_is_ready() {
                    crate::yarm_log!(
                        "X86_BOOTSTRAP_TIMER_IRQ_EOI_ONLY cpu={}",
                        self.current_cpu().0
                    );
                    self.hal.program_timer_deadline(
                        self.current_cpu(),
                        crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
                    );
                    return Ok(());
                }
                let (_tick, should_preempt) = self.tick_scheduler_timer();
                let _ = self
                    .process_ipc_timeout_deadlines(_tick.0)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?;
                // U9-TIMER3 §2 / U9-TIMER4 §2: ALL FIVE one-shot proofs that used to run HERE —
                // cap/CNode, fault-delivery, spawn-lifecycle, global-state and SMP-ready — are
                // driven from the boot ownership point instead
                // (`run_one_shot_diagnostic_proofs_at_first_dispatch`), which is where their one
                // real prerequisite is first satisfied. Their callsites are removed rather than
                // left inert, because leaving them is what made those knobs reasons for the split
                // timer route to refuse: an armed profile sent every tick of the boot to this arm,
                // long after the one-shot bodies had finished.
                //
                // Stage 178 (CROSS-ARCH-D6): one-shot read-only per-arch D6 restore-path
                // audit. Runs at most once when `yarm.cross_arch_d6=1` and a real user
                // task is current; no-op otherwise. Read-only (observes the incoming
                // task's trapframe/ASID restore state + honest per-arch DEFERRED). It
                // live-wires no restore. Arch-neutral, diagnostic only.
                self.maybe_run_cross_arch_d6_audit();
                // Stage 184 (CROSS-ARCH-LIVE): default-on, one-shot cross-arch live
                // audit — attests the honest per-arch topology + graduated D2/D6/D3 +
                // syscall parity. Observability only; live-wires nothing.
                self.maybe_run_cross_arch_live_audit();
                // Stage 179 (D3-FULL): one-shot D3 VM anon-map/unmap proof (timer path;
                // x86_64 primary). One-shot latch shared with the syscall-path hook.
                self.maybe_run_d3_full_proof();
                // Stage 181 (GRADUATE-KNOBS): one-shot graduated-unlock verification
                // (timer path). Shared one-shot latch with the syscall-path hook.
                self.maybe_run_unlock_graduated_proof();
                // Stage 183 (SMP-LIVE): one-shot read-only x86_64 SMP-liveness audit.
                // Emits the SMP-unlock readiness verdict / AP-admission blocker only when
                // booted with >1 present CPU; no-op on -smp 1. Changes no behavior.
                self.maybe_run_x86_smp_unlock_audit();
                // Emit timer health markers unconditionally but only for the
                // first few ticks so that the smoke test can verify the timer
                // fires and the scheduler advances without flooding the UART.
                // (BOOTSTRAP_TIMER_DEADLINE_TICKS / 16 ≈ 3 ms/tick on QEMU;
                //  at 90 s we would get ~30 000 ticks — far too many to log.)
                // Canonical 199E: AArch64 now delivers this same production tick, so it
                // shares the identical bounded emission rather than gaining a marker family of
                // its own. x86_64 keeps the exact code and bound it already had.
                #[cfg(all(
                    not(feature = "hosted-dev"),
                    any(
                        target_arch = "x86_64",
                        target_arch = "aarch64",
                        target_arch = "riscv64"
                    )
                ))]
                {
                    use core::sync::atomic::{AtomicU64, Ordering};
                    static TIMER_LOG_EMITTED: AtomicU64 = AtomicU64::new(0);
                    let log_seq = TIMER_LOG_EMITTED.fetch_add(1, Ordering::Relaxed);
                    if log_seq < 4 {
                        crate::yarm_log!("YARM_TIMER_EOI_DONE cpu={}", self.current_cpu().0);
                        crate::yarm_log!(
                            "YARM_SCHED_TICK cpu={} tick={} preempt={}",
                            self.current_cpu().0,
                            _tick.0,
                            should_preempt as u8
                        );
                        crate::yarm_log!(
                            "YARM_TIMER_IRQ_DELIVERED cpu={} tick={}",
                            self.current_cpu().0,
                            _tick.0
                        );
                    }
                }
                #[cfg(all(
                    not(feature = "hosted-dev"),
                    not(any(
                        target_arch = "x86_64",
                        target_arch = "aarch64",
                        target_arch = "riscv64"
                    ))
                ))]
                if DEBUG_TIMER_LOG {
                    crate::yarm_log!("YARM_TIMER_EOI_DONE cpu={}", self.current_cpu().0);
                    crate::yarm_log!(
                        "YARM_SCHED_TICK cpu={} tick={} preempt={}",
                        self.current_cpu().0,
                        _tick.0,
                        should_preempt as u8
                    );
                    crate::yarm_log!(
                        "YARM_TIMER_IRQ_DELIVERED cpu={} tick={}",
                        self.current_cpu().0,
                        _tick.0
                    );
                }
                if should_preempt {
                    // U9-TIMER2 §4 — the LIVE half of this package's closure claim, measured
                    // where arrivals are.
                    //
                    // The population it retires is exactly this: a preempting tick on a CPU with
                    // NO current task, which then reaches `yield_current` and, through
                    // `on_preempt_current_cpu_selection()`, genuinely dequeues and dispatches onto
                    // the idle CPU. Counting it HERE — inside the broad `Trap::TimerInterrupt`
                    // arm, ahead of the call that performs it — names that population and nothing
                    // else. A marker inside `yield_current` would be broader: its
                    // kernel-internal callers reach the same selection with no current task for
                    // reasons that have nothing to do with a timer.
                    //
                    // Same placement rule as `IPC_SEND_BROAD_ENTRY` and
                    // `FUTEX_WAIT_BROAD_ENTRY`: at the arrival, before anything can divert it. The
                    // source half is the split route's own settlement; this is the live half.
                    if self.current_tid().is_none() {
                        crate::yarm_log!(
                            "TIMER_IDLE_ADVANCE_BROAD_ENTRY cpu={} result=broad_entry",
                            self.current_cpu().0
                        );
                    }
                    self.yield_current()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                self.hal.program_timer_deadline(
                    self.current_cpu(),
                    crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
                );
                Ok(())
            }
            Trap::PageFault | Trap::ExternalInterrupt | Trap::Unknown => Ok(()),
        }
    }

    pub fn control_plane_set_process_cnode_slots_via_syscall(
        &mut self,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), TrapHandleError> {
        let Ok(target_pid_arg) = usize::try_from(target_pid) else {
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        };
        let mut frame = TrapFrame::new(
            Syscall::ControlPlaneSetCnodeSlots as usize,
            [target_pid_arg, slot_capacity, 0, 0, 0, 0],
        );
        // After the Stage 81A parity fix, handle_trap encodes syscall errors
        // into the frame instead of propagating them as TrapHandleError.
        // Translate the frame error code back so callers retain the expected
        // Result<(), TrapHandleError> contract (policy denials stay visible).
        self.handle_trap(Trap::Syscall, Some(&mut frame))?;
        if let Some(code) = frame.error_code() {
            return Err(TrapHandleError::Syscall(SyscallError::from_code(code)));
        }
        Ok(())
    }

    pub fn handle_selected_arch_trap_entry(
        &mut self,
        cpu: crate::kernel::scheduler::CpuId,
        context: crate::arch::trap_entry::ArchTrapContext,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        crate::arch::trap_entry::handle_trap_entry(self, cpu, context, frame)
    }

    pub fn handle_trap_event(
        &mut self,
        event: TrapEvent,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        self.handle_trap_event_with_fault_bookkeeping_mode(
            event,
            frame,
            FaultBookkeepingMode::RecordInHandleTrapEvent,
        )
    }

    pub(crate) fn handle_trap_event_with_fault_bookkeeping_mode(
        &mut self,
        event: TrapEvent,
        frame: Option<&mut TrapFrame>,
        fault_bookkeeping_mode: FaultBookkeepingMode,
    ) -> Result<(), TrapHandleError> {
        if matches!(
            fault_bookkeeping_mode,
            FaultBookkeepingMode::RecordInHandleTrapEvent
        ) {
            if let Some(fault) = event.fault() {
                self.record_fault(fault);
                if let Some(frame) = frame.as_ref() {
                    self.record_fault_frame_snapshot(frame);
                }
            }
        }

        match event {
            TrapEvent::PageFault(fault) => {
                // U9-PAGEFAULT1 §2 — MEASURE BROAD ARRIVALS POSITIVELY.
                //
                // `PAGE_FAULT_ENTRY` cannot do this job. Every split route emits it too, in this
                // arm's position, precisely so the marker stream stays faithful to what an
                // observer saw before the owner changed — which means a count of it is a count
                // of ALL page faults, not of the ones that reached the broad dispatcher.
                //
                // This marker prints only here, so the residual population is counted rather
                // than inferred from the absence of something else. It carries the privilege
                // origin because that is the one fact the broad arm still does not test and the
                // split classifiers do.
                crate::yarm_log!(
                    "PF1_BROAD_ARRIVAL cpu={} tid={} addr=0x{:x} access={:?} origin={:?} broad_lock=1",
                    self.current_cpu().0,
                    self.current_tid().unwrap_or(u64::MAX),
                    fault.addr.0,
                    fault.access,
                    fault.origin
                );
                crate::yarm_log!(
                    "PAGE_FAULT_ENTRY tid={} addr=0x{:x} access={:?} rip=0x{:x}",
                    self.current_tid().unwrap_or(u64::MAX),
                    fault.addr.0,
                    fault.access,
                    frame.as_ref().map(|f| f.saved_pc).unwrap_or(0)
                );
                // Stage 174 (FAULT-DELIVERY): default-off classification marker. The
                // fault classification (handled COW/demand vs user-unhandled) below is
                // UNCHANGED; this only exposes the decision boundary.
                if crate::kernel::boot::fault_delivery_enabled() {
                    crate::yarm_log!(
                        "FAULT_DELIVERY_CLASSIFY_BEGIN tid={} vector=14 rip=0x{:x}",
                        self.current_tid().unwrap_or(u64::MAX),
                        frame.as_ref().map(|f| f.saved_pc).unwrap_or(0)
                    );
                }
                // Stage 163G: proof-gated page-fault classification diagnostics
                // (active only under the sender-wake sub-knob, so normal boots are
                // not polluted). Reveals why a present write fault routes to demand:
                // whether the page is found, writable, COW-marked, and demand-backed.
                if crate::kernel::boot::ipc_recv_proof_sender_wake_active()
                    && let Some(tid) = self.current_tid()
                    && let Some(asid) = self.task_asid(tid)
                {
                    let page = fault.addr.page_align_down();
                    let mapping =
                        self.with_user_spaces(|s| s.get(asid).and_then(|a| a.resolve(page)));
                    let cow = self.is_cow_page(asid, page);
                    let demand = self.fault_addr_in_demand_backed_region(tid, page.0);
                    crate::yarm_log!(
                        "PF_PROOF_CLASSIFY tid={} asid={} va=0x{:x} access={:?}",
                        tid,
                        asid.0,
                        fault.addr.0,
                        fault.access
                    );
                    crate::yarm_log!(
                        "PF_PROOF_LOOKUP_MAPPING tid={} asid={} va=0x{:x} found={} writable={} cow={} demand={} phys=0x{:x}",
                        tid,
                        asid.0,
                        page.0,
                        mapping.is_some() as u8,
                        mapping.map(|m| m.flags.write as u8).unwrap_or(0),
                        cow as u8,
                        demand as u8,
                        mapping.map(|m| m.phys.0).unwrap_or(0)
                    );
                    // Stage 163H: decode the ACTIVE CR3's hardware PTE before any
                    // handling, so a software-writable / hardware-faulting mismatch is
                    // unambiguous (the hardware walk reads the real active CR3).
                    self.pf_proof_log_hw_pte("PF_PROOF_HW_PTE_BEFORE", tid, asid, page);
                }
                if matches!(fault.access, FaultAccess::Write) {
                    if let Some(tid) = self.current_tid()
                        && let Some(asid) = self.task_asid(tid)
                        && self
                            .try_handle_cow_fault(asid, fault.addr)
                            .map_err(SyscallError::from)
                            .map_err(TrapHandleError::Syscall)?
                    {
                        crate::yarm_log!("PAGE_FAULT_HANDLED_COW");
                        // Stage 174: handled COW fault — NOT delivered to supervisor.
                        if crate::kernel::boot::fault_delivery_enabled() {
                            crate::yarm_log!("FAULT_DELIVERY_CLASSIFY_HANDLED kind=cow");
                        }
                        return Ok(());
                    }
                }
                if self
                    .try_handle_demand_page_fault(fault)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)?
                {
                    // Stage 137: verify the hardware PTE is accessible before
                    // declaring the fault handled.  Also fix ASID/CR3 if the
                    // task's address space differs from what the HAL recorded.
                    let page = fault.addr.page_align_down();
                    let need_write = matches!(fault.access, FaultAccess::Write);
                    let tid = self.current_tid().unwrap_or(u64::MAX);
                    let task_asid = self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0));
                    let active_asid_num = self.d6_diag_active_asid_num();
                    let active_asid = crate::kernel::vm::Asid(active_asid_num as u16);
                    let task_pte =
                        crate::arch::selected_isa::page_table::resolve_page(task_asid, page);
                    let active_pte = if active_asid.0 != task_asid.0 {
                        crate::arch::selected_isa::page_table::resolve_page(active_asid, page)
                    } else {
                        task_pte
                    };
                    let task_present = task_pte.is_some();
                    let active_present = active_pte.is_some();
                    let task_flags = task_pte.map(|p| p.0).unwrap_or(0);
                    let active_flags = active_pte.map(|p| p.0).unwrap_or(0);
                    crate::yarm_log!(
                        "PAGE_FAULT_DEMAND_VERIFY tid={} page=0x{:x} task_asid={} active_asid={} task_present={} active_present={} task_flags=0x{:x} active_flags=0x{:x}",
                        tid,
                        page.0,
                        task_asid.0,
                        active_asid.0,
                        task_present,
                        active_present,
                        task_flags,
                        active_flags,
                    );
                    let pte_ok = task_pte
                        .map(|p| demand_pte_flags_ok(p, need_write))
                        .unwrap_or(false);
                    // Stage 163H: the running task MUST execute on its OWN ASID's
                    // page table. The previous condition only corrected CR3 when the
                    // active entry was ABSENT, which missed the observed fork-child
                    // case: the active page table is a DIFFERENT ASID holding a
                    // stale/wrong but PRESENT entry (active_flags=0x80000007, phys
                    // 0x80000000) while the task's own ASID maps the page correctly
                    // (task_flags=...104dd007). The CPU then walks the wrong table and
                    // re-faults forever. Switch whenever the active table is a
                    // different ASID whose PTE for this page disagrees with the task's
                    // correct mapping, then invalidate so the CPU re-walks the right
                    // table. (When active == task, flags match and we never switch.)
                    let active_mismatch =
                        active_asid.0 != task_asid.0 && active_flags != task_flags;
                    if pte_ok && active_mismatch {
                        if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                            crate::yarm_log!(
                                "PF_PROOF_DEMAND_SWITCH_CR3 tid={} page=0x{:x} from_asid={} to_asid={} active_flags=0x{:x} task_flags=0x{:x}",
                                tid,
                                page.0,
                                active_asid.0,
                                task_asid.0,
                                active_flags,
                                task_flags
                            );
                        }
                        self.hal.switch_address_space(self.current_cpu(), task_asid);
                        crate::arch::selected_isa::page_table::invalidate_page(page);
                    }
                    // Stage 138: hardware CR3 PTE walk to confirm the CPU will
                    // actually see the page as accessible after demand mapping.
                    // Software VM resolve says present, but the CPU may be
                    // walking a different (stale) page table.
                    // Only performed on real x86_64 hardware; hosted-dev (test)
                    // mode has no real page tables so hw_demand_ok is trivially true.
                    #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
                    let hw_demand_ok = {
                        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
                        let hw_root = hw_cr3 & !0xfffu64;
                        let (pml4e, pdpte, pde, hw_pte) =
                            crate::arch::x86_64::page_table::hw_pte_walk_verbose(hw_root, page.0);
                        // Stage 163I: effective access rights are the logical-AND
                        // of the bits across EVERY paging-structure entry used to
                        // translate the address (Intel SDM Vol. 3A 4.6), not just
                        // the leaf. A writable+user leaf under an intermediate
                        // that lacks USER/WRITABLE is still inaccessible and faults
                        // present+write+user forever; the leaf-only check masked
                        // that and let HANDLED_DEMAND loop. Require the whole walk
                        // to grant the access before declaring it satisfied.
                        let walk = [pml4e, pdpte, pde, hw_pte];
                        let eff_present = walk.iter().all(|e| (e & 1) != 0);
                        let eff_writable = walk.iter().all(|e| (e & 2) != 0);
                        let eff_user = walk.iter().all(|e| (e & 4) != 0);
                        let hw_present = (hw_pte & 1) != 0;
                        let hw_user = (hw_pte & 4) != 0;
                        let hw_writable = (hw_pte & 2) != 0;
                        crate::yarm_log!(
                            "PAGE_FAULT_POST_DEMAND_HW_PTE_WALK cr3=0x{:016x} va=0x{:016x} pml4e=0x{:016x} pdpte=0x{:016x} pde=0x{:016x} pte=0x{:016x} present={} user={} writable={} eff_present={} eff_user={} eff_writable={}",
                            hw_cr3,
                            page.0,
                            pml4e,
                            pdpte,
                            pde,
                            hw_pte,
                            hw_present as u8,
                            hw_user as u8,
                            hw_writable as u8,
                            eff_present as u8,
                            eff_user as u8,
                            eff_writable as u8,
                        );
                        eff_present && eff_user && (!need_write || eff_writable)
                    };
                    #[cfg(any(not(target_arch = "x86_64"), feature = "hosted-dev"))]
                    let hw_demand_ok = true;
                    // Stage 163H: decode the ACTIVE CR3's PTE AFTER demand handling +
                    // any CR3 correction, so the next run shows whether the active
                    // hardware mapping is now writable (matching the task ASID).
                    if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                        self.pf_proof_log_hw_pte("PF_PROOF_HW_PTE_AFTER", tid, task_asid, page);
                    }
                    if pte_ok && hw_demand_ok {
                        crate::yarm_log!("PAGE_FAULT_HANDLED_DEMAND");
                        // Stage 174: handled demand fault — NOT delivered to supervisor.
                        if crate::kernel::boot::fault_delivery_enabled() {
                            crate::yarm_log!("FAULT_DELIVERY_CLASSIFY_HANDLED kind=demand");
                        }
                        return Ok(());
                    }
                }
                crate::yarm_log!(
                    "PAGE_FAULT_UNHANDLED tid={} addr=0x{:x} access={:?} rip=0x{:x}",
                    self.current_tid().unwrap_or(u64::MAX),
                    fault.addr.0,
                    fault.access,
                    frame.as_ref().map(|f| f.saved_pc).unwrap_or(0)
                );
                // Stage 174: unhandled user fault — routes to supervisor delivery.
                if crate::kernel::boot::fault_delivery_enabled() {
                    crate::yarm_log!(
                        "FAULT_DELIVERY_CLASSIFY_USER_UNHANDLED tid={} vector=14 addr=0x{:x} access={:?}",
                        self.current_tid().unwrap_or(u64::MAX),
                        fault.addr.0,
                        fault.access
                    );
                }
                self.fault_current_task_for_fault(fault)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall)
            }
            TrapEvent::ExternalInterrupt(irq) => {
                let irq_state = crate::arch::irq_guard::irq_save();
                let route_result = self
                    .route_external_irq(irq)
                    .map_err(SyscallError::from)
                    .map_err(TrapHandleError::Syscall);
                crate::arch::irq_guard::external_irq_eoi(irq);
                crate::arch::irq_guard::irq_restore(irq_state);
                route_result?;
                self.handle_trap(Trap::ExternalInterrupt, frame)
            }
            TrapEvent::Syscall => self.handle_trap(Trap::Syscall, frame),
            TrapEvent::TimerInterrupt => self.handle_trap(Trap::TimerInterrupt, frame),
            TrapEvent::Unknown { arch_code } => {
                crate::yarm_log!(
                    "unknown trap event cpu={} arch_code=0x{:x}",
                    self.current_cpu().0,
                    arch_code
                );
                // Stage 174 (FAULT-DELIVERY): an unknown/kernel trap stays FATAL — it
                // is NOT reclassified as a supervisor-deliverable user fault. This
                // marker only records the classification; the fatal policy below is
                // UNCHANGED.
                if crate::kernel::boot::fault_delivery_enabled() {
                    crate::yarm_log!(
                        "FAULT_DELIVERY_CLASSIFY_KERNEL_FATAL vector=0x{:x}",
                        arch_code
                    );
                }
                if STRICT_UNKNOWN_TRAPS {
                    panic!(
                        "strict unknown trap policy: cpu={} arch_code=0x{:x}",
                        self.current_cpu().0,
                        arch_code
                    );
                }
                self.handle_trap(Trap::Unknown, frame)
            }
        }
    }
}
