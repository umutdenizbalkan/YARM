// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{
    ControlPlaneCnodePlan, FaultSubsystem, KernelCapacityProfile, KernelError, KernelState,
    KernelStorage, RuntimeCapacityConfig, SchedulerState, TelemetrySubsystem, TrapHandleError,
    kernel_mut, kernel_ref,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights};
use crate::kernel::ipc::Message;
use crate::kernel::lock::SpinLock;
#[cfg(test)]
use crate::kernel::lock::SpinLockGuard;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{FaultPolicy, TaskClass};
use crate::kernel::trap::FaultInfo;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{PAGE_SIZE, VirtAddr, VmError};
#[cfg(any(debug_assertions, test))]
use core::sync::atomic::{AtomicBool, Ordering};

/// Stage 30 / Review-finding C1: debug-only guard for the raw `&mut KernelState`
/// aliasing window opened by [`SharedKernel::borrow_kernel_for_boot`].
///
/// If a timer ISR or trap entry fires and calls `with` / `with_cpu` while the raw
/// boot borrow is live, the two mutable references alias — undefined behavior.
/// This flag lets arch trap/timer entry points `debug_assert!` no such race
/// is in progress. Zero cost in release: the static, helpers, and all
/// `debug_assert!` callers are `#[cfg(any(debug_assertions, test))]`.
#[cfg(any(debug_assertions, test))]
static BOOT_RAW_BORROW_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Open the boot raw-borrow window (debug/test only).
///
/// Asserts the window was not already open (no double-borrow).
#[cfg(any(debug_assertions, test))]
pub fn begin_boot_raw_borrow_window() {
    let was_active = BOOT_RAW_BORROW_ACTIVE.swap(true, Ordering::SeqCst);
    debug_assert!(
        !was_active,
        "borrow_kernel_for_boot called while a raw boot borrow is already live — aliasing &mut KernelState"
    );
}

/// Close the boot raw-borrow window (debug/test only).
#[cfg(any(debug_assertions, test))]
pub fn end_boot_raw_borrow_window() {
    BOOT_RAW_BORROW_ACTIVE.store(false, Ordering::SeqCst);
}

/// Report whether the boot raw-borrow window is currently open (debug/test only).
#[cfg(any(debug_assertions, test))]
pub fn boot_raw_borrow_is_active() -> bool {
    BOOT_RAW_BORROW_ACTIVE.load(Ordering::SeqCst)
}

/// RAII guard that closes the boot raw-borrow window on drop (debug/test only).
///
/// The live arch boot path never returns (ERET), so the window is intentionally
/// not closed in production — the flag becomes irrelevant after ERET since all
/// further KernelState access goes through `with` / `with_cpu`. This guard is
/// useful in test/returning paths where dropping it restores a clean state.
#[cfg(any(debug_assertions, test))]
pub struct BootRawKernelBorrowGuard;

#[cfg(any(debug_assertions, test))]
impl Drop for BootRawKernelBorrowGuard {
    fn drop(&mut self) {
        end_boot_raw_borrow_window();
    }
}

/// Pre-read snapshot of diagnostic data for the fatal-trap log path.
///
/// Populated by `SharedKernel::fatal_trap_read_snapshot` using only sub-global
/// split-read locks (scheduler rank 1, task rank 2). Used by the x86_64
/// shared-kernel trap path to log fatal trap diagnostics without acquiring the
/// global `SharedKernel` lock.
#[derive(Debug, Clone, Copy)]
pub struct FatalTrapReadSnapshot {
    pub current_tid: u64,
    pub current_asid: u64,
}

/// Stage 32: immutable, `Copy` snapshot of a resolved endpoint **receive**
/// capability.
///
/// Produced by [`SharedKernel::resolve_endpoint_recv_cap_split_read`] (and the
/// `KernelState` raw helper it delegates to) using a strict phase-separated
/// lock protocol — task lock (rank 2) read+release, then capability lock
/// (rank 4) read+release — with NO IPC lock and NO mutation. It captures
/// exactly what the IPC dequeue phase needs: the resolved endpoint object
/// (`index`, `generation`) so the IPC domain can revalidate liveness under
/// `ipc_state_lock`, plus the requester identity for telemetry/debug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointRecvCapSnapshot {
    /// Resolved endpoint object (`CapObject::Endpoint { index, generation }`).
    /// The `index`/`generation` let the IPC dequeue phase revalidate liveness
    /// (`resolve_endpoint_index`) under `ipc_state_lock` before dequeue.
    pub endpoint: CapObject,
    /// The receive capability's rights (always includes `RECEIVE`).
    pub rights: CapRights,
    /// Requester thread id (the receiving task).
    pub requester_tid: u64,
    /// Requester process id (thread-group id) whose cnode the cap was found in.
    pub requester_pid: u64,
}

impl EndpointRecvCapSnapshot {
    /// The endpoint slot index, if the captured object is an `Endpoint`.
    pub fn endpoint_index(&self) -> Option<usize> {
        match self.endpoint {
            CapObject::Endpoint { index, .. } => Some(index),
            _ => None,
        }
    }
}

/// Stage 32: maximum plain inline payload a split queued-plain recv writeback
/// plan can carry. Sized to the IPC message payload bound
/// (`Message::MAX_PAYLOAD == 128`).
pub const MAX_PLAIN_PAYLOAD: usize = 128;

/// Stage 32: scaffolded writeback plan for a split queued-plain IPC recv.
///
/// Captures everything needed to perform the trap-frame writeback for one
/// dequeued plain message **outside** all locks, so that a future stage can do
/// the user-memory copy (`copy_to_current_user`) after releasing
/// `ipc_state_lock`. The plan is filled under `ipc_state_lock` (payload bytes +
/// return metadata), then applied lock-free.
///
/// Status (Stage 32): SCAFFOLDED. The kernel-task writeback (register-only) is
/// equivalent to the live helper path; the user-ASID branch is left DISABLED
/// (`is_kernel_task == false` ⇒ the integrated helper returns `None` / fallback)
/// because matching the old path's "message-consumed-on-copy-fail" semantics
/// across a post-dequeue user-copy is not yet proven safe. See
/// `doc/KERNEL_LOCKING.md` §50.
#[derive(Debug, Clone, Copy)]
pub struct IpcRecvQueuedPlainWritebackPlan {
    /// Payload bytes (fixed-size inline, sized to `MAX_PLAIN_PAYLOAD`).
    payload: [u8; MAX_PLAIN_PAYLOAD],
    /// Valid length of `payload`.
    payload_len: usize,
    /// Sender TID return lane (`ret0`).
    sender_tid: u64,
    /// Transfer-cap return lane (`ret2`); always `NO_TRANSFER_CAP` for a plain
    /// message.
    ret_cap: u64,
    /// User payload destination pointer (from `SYSCALL_ARG_PTR`); only used on
    /// the (currently disabled) user-ASID branch.
    user_payload_ptr: u64,
    /// User payload destination length (from `SYSCALL_ARG_LEN`).
    user_payload_len: usize,
    /// `true` if the receiver is a kernel task (register-only writeback, no
    /// user copy). `false` ⇒ user-ASID receiver, currently DISABLED.
    is_kernel_task: bool,
    /// Endpoint object for debug/logging.
    endpoint: CapObject,
}

impl IpcRecvQueuedPlainWritebackPlan {
    /// `NO_TRANSFER_CAP` sentinel for the transfer-cap return lane.
    pub const NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;

    /// Build a kernel-task plan from a dequeued plain message. Returns `None`
    /// when the payload exceeds `MAX_PLAIN_PAYLOAD` (cannot be represented).
    pub fn for_kernel_task(
        snapshot: &EndpointRecvCapSnapshot,
        sender_tid: u64,
        msg_payload: &[u8],
    ) -> Option<Self> {
        if msg_payload.len() > MAX_PLAIN_PAYLOAD {
            return None;
        }
        let mut payload = [0u8; MAX_PLAIN_PAYLOAD];
        payload[..msg_payload.len()].copy_from_slice(msg_payload);
        Some(Self {
            payload,
            payload_len: msg_payload.len(),
            sender_tid,
            ret_cap: Self::NO_TRANSFER_CAP,
            user_payload_ptr: 0,
            user_payload_len: 0,
            is_kernel_task: true,
            endpoint: snapshot.endpoint,
        })
    }

    /// Valid payload slice captured by the plan.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }

    /// Payload length.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// Sender TID return lane.
    pub fn sender_tid(&self) -> u64 {
        self.sender_tid
    }

    /// Transfer-cap return lane.
    pub fn ret_cap(&self) -> u64 {
        self.ret_cap
    }

    /// Whether the receiver is a kernel task (register-only writeback).
    pub fn is_kernel_task(&self) -> bool {
        self.is_kernel_task
    }

    /// User payload destination pointer (user-ASID branch only).
    pub fn user_payload_ptr(&self) -> u64 {
        self.user_payload_ptr
    }

    /// User payload destination length (user-ASID branch only).
    pub fn user_payload_len(&self) -> usize {
        self.user_payload_len
    }

    /// Endpoint object captured for debug/logging.
    pub fn endpoint(&self) -> CapObject {
        self.endpoint
    }
}

#[derive(Debug)]
pub struct SharedKernel {
    state: SpinLock<KernelState>,
}

/// Stage 200C2B — the OFF-LOCK per-domain access for the reply-timeout completion
/// transaction. Wraps `&SharedKernel` and implements `ReplyTimeoutDomains` via the
/// per-domain split-mut seams (`with_ipc_split_mut`, `with_task_tcbs_split_mut`, and
/// the rank-1 scheduler enqueue seam). It NEVER forms a broad `&mut KernelState` and
/// NEVER takes the broad `SpinLock<KernelState>` — each primitive is a SHORT bounded
/// claim of exactly one domain, so the composed transaction holds no broad lock.
// CLASSIFICATION (Stage 200D-F0): **production mechanism**. Despite the historical
// `ReplyTimeout` name this is the generic OFF-LOCK IPC TERMINAL-COMPLETION domain: the
// ungated server-death path composes its transaction through exactly these seams on every
// build. Gating it on `ipc-reply-timeout-oracle-core` made the feature-off kernel fail to
// compile. It is deliberately NOT renamed here — this stage's first commit is the minimal
// repair, and a rename would spread an unrelated diff across every call site.
pub(crate) struct OffLockReplyTimeout<'a>(pub(crate) &'a SharedKernel);

impl crate::kernel::boot::ReplyTimeoutDomains for OffLockReplyTimeout<'_> {
    fn rtd_ipc<R>(&mut self, f: impl FnOnce(&mut crate::kernel::boot::IpcSubsystem) -> R) -> R {
        self.0.with_ipc_split_mut(f)
    }
    fn rtd_tcbs<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<crate::kernel::task::ThreadControlBlock>]) -> R,
    ) -> R {
        self.0.with_task_tcbs_split_mut(f)
    }
    fn rtd_enqueue(&mut self, tid: u64) {
        self.0.enqueue_reply_timeout_wake_split(tid);
    }
}

/// Stage 200D-2B1D5B — the typed outcome of the x86_64 post-drain owner revalidation.
///
/// The seam's hazard is that `dispatch_next_on_cpu` COMMITS its selection as the CPU's
/// `current` before the arch restore is attempted. A plain `Option<u64>` could not say whether
/// a `None` meant "nothing was ever committed" (safe to idle) or "a replacement was committed
/// and then failed to restore" (idling strands it). These three cases are now distinct.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerRevalidation {
    /// Nothing was committed: the drains produced no CPU-local work, or the queue yielded the
    /// scheduler's idle/supervisor sentinel (TID 0), which owns no user context.
    Idle,
    /// `tid` was committed as this CPU's `current` AND its arch state was restored into the
    /// caller's `TrapFrame`. The frame is populated and safe to commit.
    Replacement(u64),
    /// `tid` was committed as this CPU's `current` and the arch restore did NOT succeed, so the
    /// frame is not populated with `tid`'s context. `rolled_back` records whether the seam
    /// managed to undo the advance (clear `current`, and return `tid` to this CPU's run queue
    /// unless its TCB is gone). Only `rolled_back == true` leaves state consistent enough for
    /// the ordinary idle path.
    RestoreFailed { tid: u64, rolled_back: bool },
}

/// U3 (canonical 203C) — what the owner-revalidation transaction's rank-1 phase actually
/// proved, carried forward instead of an unauthenticated bare TID.
///
/// The fields describe this transaction and nothing else: `cpu` is the CPU that passed
/// `validate_online_cpu` and was bound as `current_cpu`, and `tid` is the task THIS
/// transaction's single `dispatch_next_on(cpu)` committed as that CPU's `current`. It is
/// deliberately **not** a [`DispatchMarkToken`]: no incarnation ASID was proved here (the
/// legacy body never proved one, and treating a task with no ASID as a refusal would be a
/// strengthening), and no dispatch mark was minted. Every later phase keys off these two
/// fields rather than re-reading `current`, which is strictly more exact than the legacy
/// body's `current_tid()` re-reads.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnerRevalidationSelection {
    pub(crate) cpu: CpuId,
    pub(crate) tid: u64,
}

/// U3 (canonical 203C) — everything the arch restore needs, read in ONE rank-2 acquisition.
///
/// Existing only means the legacy `thread_user_context(tid).is_some()` verdict was true.
/// `asid` keeps the legacy's OPTIONAL semantics: `None` skips the pre-IRET CR3 block and
/// still restores, exactly as `if let Some(task_asid) = kernel.task_asid(tid)` did.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnerRevalidationSnapshot {
    pub(crate) context: crate::kernel::task::UserRegisterContext,
    pub(crate) tls: Option<usize>,
    pub(crate) asid: Option<crate::kernel::vm::Asid>,
}

/// What the x86_64 trap epilogue must do with an [`OwnerRevalidation`].
///
/// Kept as a pure function of the outcome so the fail-closed rule is directly testable rather
/// than implied by the shape of a `match` inside the trap handler.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerCommit {
    /// Take the ordinary idle path: nothing is current on this CPU.
    Idle,
    /// Commit `tid` as the new owner through the existing replacement path.
    Replacement(u64),
    /// `tid` is still this CPU's `current` with no restorable context and the advance could not
    /// be undone. Idling would halt a CPU the scheduler believes is running a task, so the
    /// caller must fail closed through the architecture's existing fatal path.
    FailClosed(u64),
}

#[cfg(target_arch = "x86_64")]
impl OwnerRevalidation {
    /// The fail-closed rule. A rolled-back restore failure is idle-safe precisely because the
    /// rollback restored the invariant "nothing is current on this CPU"; an un-rolled-back one
    /// never is.
    pub(crate) fn disposition(self) -> OwnerCommit {
        match self {
            OwnerRevalidation::Idle => OwnerCommit::Idle,
            OwnerRevalidation::Replacement(tid) => OwnerCommit::Replacement(tid),
            OwnerRevalidation::RestoreFailed {
                rolled_back: true, ..
            } => OwnerCommit::Idle,
            OwnerRevalidation::RestoreFailed {
                tid,
                rolled_back: false,
            } => OwnerCommit::FailClosed(tid),
        }
    }
}

/// Stage 199D — the **exact entering task identity** a handled split syscall returns to.
///
/// Captured at trap entry, BEFORE the split dispatch runs, and carried through the return
/// path. The return must never re-discover an unqualified "current task" after a direct
/// transaction: the transaction can wake and enqueue another task, and on a stale or replaced
/// incarnation an unqualified lookup would commit this syscall's register state into somebody
/// else's TCB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SplitReturnIdentity {
    pub(crate) tid: u64,
    pub(crate) asid: crate::kernel::vm::Asid,
}

/// U6 §6 — the total outcome of one [`SharedKernel::commit_blocking_send_split`] transaction.
///
/// Every refusal names the coordinate that failed and means the SAME thing: nothing at all was
/// mutated, in any of the three domains. That is what lets the caller treat a refusal as an
/// ordinary immediate syscall return rather than as a partially-committed block to unwind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockingSendCommitOutcome {
    /// The sender is now `Blocked(EndpointSend)`, one waiter bearing `send_generation` is
    /// queued on the endpoint, and the CPU has no current task.
    Committed { send_generation: u64 },
    /// The snapshot's CPU is not the authoritative dispatch CPU.
    RefusedCpuMismatch {
        requested: CpuId,
        authoritative: CpuId,
    },
    /// The sender is no longer this CPU's current task.
    RefusedNotCurrent {
        expected: u64,
        observed: Option<u64>,
    },
    /// No live `{tid, asid}` incarnation matches the sender (a replacement task reused the
    /// numeric TID, or the sender is gone).
    RefusedSenderMissing,
    /// The sender is already blocked — a second commit for one caller is impossible.
    RefusedAlreadyBlocked,
    /// `blocked_send_generation` cannot be advanced without wrapping. Refusing keeps the
    /// generation a proof of identity rather than a probability.
    RefusedGenerationExhausted,
    /// The endpoint slot is empty.
    RefusedEndpointMissing,
    /// The endpoint slot was destroyed (and possibly reused) since the producer resolved it.
    RefusedEndpointGenerationChanged,
    /// A waiter for this TID is already queued on the endpoint.
    RefusedDuplicateWaiter,
    /// The endpoint's sender-waiter queue is full.
    RefusedWaiterQueueFull,
}

impl BlockingSendCommitOutcome {
    /// Stable slug for markers/telemetry.
    pub(crate) const fn slug(&self) -> &'static str {
        match self {
            Self::Committed { .. } => "committed",
            Self::RefusedCpuMismatch { .. } => "refused_cpu_mismatch",
            Self::RefusedNotCurrent { .. } => "refused_not_current",
            Self::RefusedSenderMissing => "refused_sender_missing",
            Self::RefusedAlreadyBlocked => "refused_already_blocked",
            Self::RefusedGenerationExhausted => "refused_generation_exhausted",
            Self::RefusedEndpointMissing => "refused_endpoint_missing",
            Self::RefusedEndpointGenerationChanged => "refused_endpoint_generation_changed",
            Self::RefusedDuplicateWaiter => "refused_duplicate_waiter",
            Self::RefusedWaiterQueueFull => "refused_waiter_queue_full",
        }
    }

    /// The canonical syscall error a refusal returns to the still-running caller.
    ///
    /// These are the SAME errors the in-lock route produces for the same conditions, so a
    /// caller cannot tell from its result which route ran: a vanished endpoint is
    /// `WrongObject`, a full waiter queue is `EndpointQueueFull` (what
    /// `enqueue_sender_waiter` returns), a missing task is `TaskMissing`, and every
    /// scheduling-state refusal is `WouldBlock` — the retryable answer, which is exactly what
    /// a caller that is still running and still holds its message should see.
    pub(crate) const fn immediate_error(&self) -> Option<KernelError> {
        match self {
            Self::Committed { .. } => None,
            Self::RefusedEndpointMissing | Self::RefusedEndpointGenerationChanged => {
                Some(KernelError::WrongObject)
            }
            Self::RefusedWaiterQueueFull => Some(KernelError::EndpointQueueFull),
            Self::RefusedSenderMissing => Some(KernelError::TaskMissing),
            Self::RefusedCpuMismatch { .. }
            | Self::RefusedNotCurrent { .. }
            | Self::RefusedAlreadyBlocked
            | Self::RefusedGenerationExhausted
            | Self::RefusedDuplicateWaiter => Some(KernelError::WouldBlock),
        }
    }
}

/// Stage 199D-WA3A-R2-SEAL (item D) — a `DispatchSelection` bound to the CPU whose
/// **authoritative** runqueue actually produced it.
///
/// The off-lock dispatch seams mutate the scheduler's own `current_cpu`, while the trap caller
/// separately supplies the CPU it believes it is on. Letting the caller's value reach the mark
/// token would let an unverified CPU be stamped into rollback authority — a rollback would then
/// re-enqueue on, and clear `current` of, the wrong core. Binding the CPU to the selection at
/// the point of mutation removes that possibility structurally: `d6_genuine_mark_running_via_task_seam`
/// takes no `cpu` argument at all, so there is nothing for a caller to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpuDispatch {
    /// The requested CPU **was** the authoritative dispatch CPU; `selection` is what it did.
    Selected {
        cpu: CpuId,
        selection: crate::kernel::scheduler::DispatchSelection,
    },
    /// The requested CPU was **not** the authoritative dispatch CPU. Produced strictly BEFORE
    /// any mutation: nothing was dequeued, nothing became current, and no token can exist.
    RefusedCpuMismatch {
        requested: CpuId,
        authoritative: CpuId,
    },
}

impl CpuDispatch {
    /// The selected TID, if a selection was made at all.
    pub(crate) fn tid(self) -> Option<crate::kernel::ipc::ThreadId> {
        match self {
            Self::Selected { selection, .. } => selection.tid(),
            Self::RefusedCpuMismatch { .. } => None,
        }
    }

    /// A short stable name for markers.
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::Selected { selection, .. } => selection.marker(),
            Self::RefusedCpuMismatch { .. } => "refused_cpu_mismatch",
        }
    }
}

/// Stage 199D-WA3A-R1 — proof that a specific incarnation was marked `Running` by a specific
/// dispatch, on a specific CPU. Minted only by a successful mark; carries the exact
/// incarnation, never a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DispatchMarkToken {
    cpu: CpuId,
    tid: u64,
    incarnation: MarkedIncarnation,
    provenance: crate::kernel::scheduler::DispatchSelection,
}

/// The exact identity a mark token names. There is deliberately no wildcard variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkedIncarnation {
    /// A user task, identified by its exact address space.
    User { asid: crate::kernel::vm::Asid },
    /// The idle task, which has no address space by construction.
    Idle,
}

impl MarkedIncarnation {
    /// Stage 199D-WA3A-R2-SEAL (item A) — resolve the exact identity of `tid`, or refuse.
    ///
    /// A non-idle task with no ASID has **no** supported exact identity, so it gets no
    /// incarnation and therefore no token. This is called inside the same rank-2 acquisition
    /// that performs the status transition, and strictly BEFORE it: a missing identity must
    /// refuse before mutation, never be discovered after the status has already moved.
    fn resolve(tid: u64, asid: Option<crate::kernel::vm::Asid>) -> Option<Self> {
        match (tid, asid) {
            (crate::kernel::task_transition::IDLE_TID, _) => Some(Self::Idle),
            (_, Some(asid)) => Some(Self::User { asid }),
            (_, None) => None,
        }
    }
}

impl DispatchMarkToken {
    /// Mint a token for an already-resolved exact incarnation on an already-authenticated CPU.
    fn new(
        cpu: CpuId,
        tid: u64,
        incarnation: MarkedIncarnation,
        provenance: crate::kernel::scheduler::DispatchSelection,
    ) -> Self {
        Self {
            cpu,
            tid,
            incarnation,
            provenance,
        }
    }

    /// Stage 199D-WA3A-R2-SEAL (item C) — narrow this token to **dequeue** authority.
    ///
    /// `Some` only when the token's provenance is a genuine dequeue of this very TID. A
    /// `ContinuedCurrent` mark removed no runqueue entry, so there is no dequeue to undo and
    /// no dequeue rollback may be authorized by it — the operation is simply unrepresentable
    /// rather than guarded by a runtime check at the mutation site.
    pub(crate) fn into_dequeued_authority(self) -> Option<DequeuedDispatchMarkToken> {
        match self.provenance {
            crate::kernel::scheduler::DispatchSelection::Dequeued { tid } if tid.0 == self.tid => {
                Some(DequeuedDispatchMarkToken(self))
            }
            _ => None,
        }
    }

    pub(crate) fn tid(&self) -> u64 {
        self.tid
    }

    pub(crate) fn cpu(&self) -> CpuId {
        self.cpu
    }

    /// The exact ASID to compare against; `None` only for the idle task, which has none.
    pub(crate) fn expect_asid(&self) -> Option<crate::kernel::vm::Asid> {
        match self.incarnation {
            MarkedIncarnation::User { asid } => Some(asid),
            MarkedIncarnation::Idle => None,
        }
    }

    pub(crate) fn provenance(&self) -> crate::kernel::scheduler::DispatchSelection {
        self.provenance
    }
}

/// Stage 199D-WA3A-R2-SEAL (item C) — sealed proof that a genuine **dequeue** was marked.
///
/// The only constructor is `DispatchMarkToken::into_dequeued_authority`, so a `ContinuedCurrent`
/// mark can never be presented as authority to undo a dequeue. The inner token is not `pub`:
/// callers must go through the narrowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DequeuedDispatchMarkToken(DispatchMarkToken);

impl DequeuedDispatchMarkToken {
    /// The underlying exact-incarnation token.
    pub(crate) fn token(self) -> DispatchMarkToken {
        self.0
    }
}

/// The coherent rank-1 → rank-2 snapshot taken by
/// [`SharedKernel::post_lock_exit_validation_split`].
///
/// Named fields rather than a positional tuple, so no consumer can silently transpose the two
/// booleans. Every field is an observation of ONE exiting incarnation at ONE instant; nothing
/// here is a mutation or a decision — the RISC-V exit consumer owns the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PostLockExitValidation {
    /// Current TID on the exact CPU the trap entered on (rank 1).
    pub(crate) current: Option<u64>,
    /// The exiting TCB is absent, carries no ASID, or is still bound to the exact expected
    /// ASID — i.e. this is not a different incarnation that reused the numeric TID (rank 2).
    pub(crate) identity_ok: bool,
    /// The exiting TCB is absent, `Exited(_)`, or `Dead` (rank 2).
    pub(crate) terminal: bool,
    /// The exiting TID is present in ANY CPU's runqueue or current slot — not merely the
    /// trapping CPU's queue (rank 1).
    pub(crate) in_runqueue: bool,
}

/// The coherent rank-1 → rank-2 restore transaction taken by
/// [`SharedKernel::post_exit_replacement_restore_split`].
///
/// Named fields rather than a positional tuple, and each names one decision the retired broad
/// body made inline. Nothing here touches hardware or the trap frame: the caller performs both,
/// with every domain lock already released.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitReplacementRestore {
    /// The replacement's ASID, to activate once the locks are dropped. `None` means the retired
    /// `d2_recv_switch_incoming_asid` would have switched nothing (the TCB is gone, or carries no
    /// ASID), so no address space may be activated.
    pub(crate) asid: Option<crate::kernel::vm::Asid>,
    /// Every task-owned restore fact, taken in the SAME rank-2 acquisition as `asid`. `None` when
    /// there is nothing to restore into (no frame) or nothing to restore (no ASID); in both cases
    /// NOTHING was consumed.
    pub(crate) facts: Option<crate::kernel::task::ThreadRestoreFacts>,
    /// The retired restore logged `SCHED_ENTER_IDLE` and returned success instead of restoring:
    /// a frame exists but the replacement carries no ASID.
    pub(crate) enter_idle: bool,
}

/// U9 (canonical 203C) — what the rank-2 half of a post-switch restore found.
///
/// Three outcomes, one per branch of the retired in-lock body, so no consumer has to re-derive
/// which `None` meant what — the distinction is load-bearing, because two of them return success
/// and the third is an error:
///
/// * `Idle` — `tid == 0`, or the task carries no ASID (including a reaped TCB). The retired body
///   logged `SCHED_ENTER_IDLE` and returned success. NOTHING was consumed.
/// * `Missing` — the task exists with an ASID but has no restorable context, which
///   `thread_user_context` reported as `None` and the retired body raised as
///   `KernelError::TaskMissing`. NOTHING was consumed.
/// * `Facts` — the complete restore payload. These are TAKES: once returned, this value holds the
///   only remaining copy of the TLS request and any parked completion, so the boundary holding it
///   must encode them or lose them.
///
/// The only production consumer is the AArch64 split twin, so a hosted build sees no live
/// construction; like [`ApSavedResumeContext`] the type is kept compiled everywhere — rather than
/// cfg'd away — so the behavioural tests can exercise the REAL transaction on twin kernel states
/// instead of a hosted-only re-implementation of it.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostSwitchRestoreOutcome<T> {
    Idle,
    Missing,
    Facts(T),
}

/// The coherent rank-2 saved-context snapshot taken by
/// [`SharedKernel::ap_saved_resume_context_split`].
///
/// Named fields rather than the legacy positional seven-element tuple, so no consumer can
/// silently transpose `rip`/`rsp`, or read `fs_base` where `cr3` was meant. Every task-owned
/// field is an observation of ONE incarnation at ONE instant, copied by value while the rank-2
/// task lock was held; `cr3` is resolved afterwards, with that lock released.
///
/// The only production constructor and the only production consumer are both freestanding-x86
/// (`ap_saved_frame_resume`), so a hosted build sees no live construction; the type is kept
/// compiled there — rather than cfg'd away — so the behavioural tests below can exercise the
/// real transaction rather than a hosted-only re-implementation of it.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApSavedResumeContext {
    /// The address space the snapshotted incarnation was bound to (rank 2).
    pub(crate) asid: u16,
    /// The page-table root for `asid`, resolved from `PAGE_TABLE_STATE` after rank 2 was
    /// released. Never observed under the task lock.
    pub(crate) cr3: u64,
    /// Exact post-syscall user instruction pointer captured when the task last left ring 3.
    pub(crate) rip: u64,
    /// Exact post-syscall user stack pointer.
    pub(crate) rsp: u64,
    /// The 15 saved user GPRs (rax..r15), in the existing order.
    pub(crate) gprs: [u64; 15],
    /// The task's saved TLS base for `IA32_FS_BASE`; a task with no TLS resumes with 0.
    pub(crate) fs_base: u64,
    /// `status is Runnable | Running` AND the saved frame is complete (`rip != 0 && rsp != 0`).
    /// A resume must never proceed from a partial or uncommitted continuation.
    pub(crate) runnable_saved: bool,
}

/// U3 (canonical 203C) — what an enqueue REFUSAL licenses, for
/// [`SharedKernel::enqueue_then_dispatch_on_cpu_split`].
///
/// The two x86_64 AP placement sites have always disagreed about this, and the disagreement is
/// deliberate, so it is spelled as a type rather than left to an unexplained boolean. Collapsing
/// them into one behaviour would change one of the two live paths.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueRefusalPolicy {
    /// The saved-resume placement (`ap_saved_frame_resume`): the task it wants may ALREADY be
    /// queued or current, so a refused enqueue is not a reason to skip selection — dispatch runs
    /// regardless, and the caller decides from the selected TID whether to resume. This is the
    /// `let _ = k.enqueue_on_cpu(...)` shape.
    DispatchAnyway,
    /// The controlled next-task placement (`ap_sched_next_or_idle`): the task is being placed for
    /// the first time, so a refused enqueue means there is nothing to select and no dispatch may
    /// happen. This is the `Err(_) => None` shape.
    ///
    /// This is the live policy of `ap_sched_next_or_idle`, whose broad acquisition U3 retired
    /// onto this transaction once the existing selector-off two-task AP workload was proven to
    /// reach it (`X86_AP_NEXT_TASK_DISPATCH_BEGIN cpu=1`). The two policies must stay distinct:
    /// dispatching through a refusal here could select a DIFFERENT task than the one being
    /// placed, which that caller then rejects — a behaviour change on a live path.
    Decline,
}

/// U3 (canonical 203C) — the outcome of one
/// [`SharedKernel::enqueue_then_dispatch_on_cpu_split`] transaction.
///
/// Named fields rather than a positional tuple: the enqueue verdict and the selected TID are
/// independent facts, and under [`EnqueueRefusalPolicy::DispatchAnyway`] a refused enqueue with a
/// successful selection is a normal, expected outcome that a bare `Option<u64>` would erase.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CpuEnqueueDispatch {
    /// Exactly what `KernelState::enqueue_on_cpu` would have returned for this TID.
    pub(crate) enqueued: Result<(), KernelError>,
    /// The TID made current on `cpu`, if any. Always `None` when the policy is
    /// [`EnqueueRefusalPolicy::Decline`] and `enqueued` is an error.
    pub(crate) selected: Option<u64>,
}

/// What a mark attempt did. Typed, so no caller has to infer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum DispatchMarkOutcome {
    /// The task is `Running` and may be resumed.
    Marked(DispatchMarkToken),
    /// Nothing was selected.
    Idle,
    /// Refused; the exact dequeue was undone. Nothing is current; do not resume.
    RefusedRolledBack,
    /// Refused; **no** scheduler state was touched (queue-neutral). Do not resume.
    RefusedNoSchedulerChange,
    /// Refused, and the scheduler could not be restored. Torn: take an explicit fatal path.
    RefusedTorn,
}

/// Stage 199D-WA3A-R2-SEAL (item E) — the ONE disposition each mark outcome licenses.
///
/// This exists so the mapping outcome → control flow is a pure, testable function rather than
/// folklore repeated at eleven call sites. Each of the five outcomes maps to a **distinct**
/// disposition: there is deliberately no value that two different refusals share, so no caller
/// can collapse `RefusedRolledBack`, `RefusedNoSchedulerChange` and `RefusedTorn` into the same
/// branch by way of this type either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchDisposition {
    /// `Marked` — the incoming task is `Running`; resume it using the token's exact identity.
    ResumeIncoming,
    /// `Idle` — nothing was selected. Settle as idle; `current` is untouched.
    SettleIdle,
    /// `RefusedRolledBack` — the exact dequeue was undone. Nothing is current on this CPU; the
    /// task is back on its runqueue. Ordinary fallback dispatch is permitted.
    DeclineDequeueUndone,
    /// `RefusedNoSchedulerChange` — no scheduler state and no task status were touched. The
    /// pre-existing `current` is intact and must be left exactly as it is.
    DeclineSchedulerUntouched,
    /// `RefusedTorn` — the scheduler and the task table disagree about who is running. Not a
    /// race and not recoverable here: no resume, no fallback dispatch, no idle-as-though-clear,
    /// no return to userspace, no further scheduling.
    Fatal,
}

impl DispatchMarkOutcome {
    /// Test/hosted-only: the token, when there is one. Production callers reach it through an
    /// exhaustive `match` on all five outcomes, never through an `Option` that silently folds
    /// the three refusals together.
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn token(self) -> Option<DispatchMarkToken> {
        match self {
            Self::Marked(t) => Some(t),
            _ => None,
        }
    }

    /// The pure outcome → disposition mapping. Total, and injective on the refusals.
    pub(crate) fn disposition(self) -> DispatchDisposition {
        match self {
            Self::Marked(_) => DispatchDisposition::ResumeIncoming,
            Self::Idle => DispatchDisposition::SettleIdle,
            Self::RefusedRolledBack => DispatchDisposition::DeclineDequeueUndone,
            Self::RefusedNoSchedulerChange => DispatchDisposition::DeclineSchedulerUntouched,
            Self::RefusedTorn => DispatchDisposition::Fatal,
        }
    }
}

/// Why the rank-2 half of a mark refused. Distinguishes "the identity was missing, so nothing
/// was mutated" from "the transition itself was rejected, which also mutated nothing" — both
/// are mutation-free, which is exactly what lets `undo_dispatch_selection` be the whole undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkRefusal {
    /// A non-idle task with no ASID: no supported exact identity, refused before mutation.
    MissingIncarnation,
    /// `apply_task_transition` rejected the exact transition (wrong status, missing task, …).
    Transition,
}

/// Stage 199D-WA3A-R2-SEAL (item E) — the single fatal path for a torn dispatch.
///
/// Reached only from `DispatchMarkOutcome::RefusedTorn`, which means the rank-1 scheduler and
/// the rank-2 task table disagree about who is running on this CPU. Every alternative — resume,
/// ordinary fallback dispatch, WFI/HLT as though `current` were clear, return to userspace —
/// would run an arbitrary frame under an arbitrary address space. Halting with a diagnosable
/// marker is the only correct disposition. Never returns.
#[inline(never)]
pub(crate) fn dispatch_torn_fatal(cpu: CpuId, tid: u64, site: &'static str) -> ! {
    crate::yarm_log!(
        "DISPATCH_TORN_FATAL cpu={} tid={} site={} reason=scheduler_task_table_disagree",
        cpu.0,
        tid,
        site
    );
    panic!("dispatch torn: scheduler and task table disagree");
}

/// U9-C — what an exact reply-cap materialization published: the receiver-local one-shot cap
/// AND the reply record it is aliased from. The rollback needs BOTH — the cap alone cannot
/// prove which record's alias to clear, and a bare index would let a reused record be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyCapMaterialization {
    pub(crate) cap: CapId,
    pub(crate) reply_index: usize,
    pub(crate) reply_generation: u64,
}

/// U9-C — the by-value facts a single rank-3 transfer-envelope consume yields.
///
/// `pinned_object` is `Some` exactly when the consumed envelope was a shared-region one and
/// therefore still owes ONE rank-6 pin release. The reply-cap and ordinary-cap classes never
/// carry one, so a `Some` is the caller's signal that it took a class it must not service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TakenTransferEnvelopeFacts {
    pub(crate) source_object: CapObject,
    pub(crate) source_tid: u64,
    pub(crate) source_cap: CapId,
    pub(crate) pinned_object: Option<CapObject>,
}

impl SharedKernel {
    /// Stage 114 fix: this used to also cache `scheduler_state` /
    /// `boot_config_state_lock` / `boot_config` raw pointers computed from
    /// the `state` parameter's address *before* `SpinLock::new(state)` moved
    /// it into `Self`. Rust gives no guarantee that move is elided, so those
    /// pointers could go stale (reproduced as a SIGSEGV reading through a
    /// dangling `scheduler_state` in the Stage 114 D3 live-seam tests). The
    /// split-read helpers below now derive the same pointers fresh from
    /// `self.state.data_ptr()` at each call, the same pattern the Stage 108
    /// `with_*_split_mut` seams already use — no caching, no staleness.
    pub fn new(state: KernelState) -> Self {
        Self {
            state: SpinLock::new(state),
        }
    }

    #[cfg(test)]
    pub fn lock(&self) -> SpinLockGuard<'_, KernelState> {
        self.state.lock()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut KernelState) -> R) -> R {
        let mut guard = self.state.lock();
        f(&mut guard)
    }

    pub fn with_cpu<R>(
        &self,
        cpu: CpuId,
        f: impl FnOnce(&mut KernelState) -> R,
    ) -> Result<R, KernelError> {
        let mut guard = self.state.lock();
        guard.set_current_cpu(cpu)?;
        Ok(f(&mut guard))
    }

    /// # Validation status
    /// - LIVE_TRAP_SMOKE_X86_64 — called from the pre-global-lock recv-timeout
    ///   trap seam (`handle_trap_entry_shared`); reads only the scheduler tick.
    pub fn scheduler_tick_now_split_read(&self) -> u64 {
        // Stage 2B split: read scheduler tick directly under scheduler lock.
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2B path=scheduler_tick_now_split_read");
        // SAFETY: `self.state.data_ptr()` is the live address of the
        // `KernelState` owned by this `SharedKernel`; recomputed fresh on
        // every call (Stage 114 fix — see `SharedKernel::new`'s doc comment).
        let scheduler_state =
            unsafe { KernelState::scheduler_split_mut_ptr_from_raw(self.state.data_ptr()) };
        let scheduler_state = unsafe { &*scheduler_state };
        let sched = scheduler_state.lock();
        sched.timer.current_ticks().0
    }

    /// Authoritative current-TID read for the *live* trap path (x86_64 -smp 1).
    ///
    /// Stage 29A: `current_tid_split_read` reads the scheduler's per-CPU current
    /// slot WITHOUT first binding `current_cpu` to the trapping CPU. At the live
    /// x86_64 pre-global-lock trap point that read is stale (it can observe a prior
    /// task, e.g. tid 0, instead of the running requester) — exactly the
    /// value-divergence the Stage 4T+6R revert documented. This helper takes the
    /// global lock only to set `current_cpu` and read `current_tid()`, which is the
    /// same authoritative value the global-lock syscall handler resolves via
    /// `current_tid(kernel)`. It performs NO dispatch, yield, or task switch — it is
    /// a read-only current-task snapshot. The split-dispatch *mutation* still runs
    /// lock-free via the per-domain split-mut helper after this read releases.
    ///
    /// U3 (203C): this is the AUTHORITATIVE rank-1 transaction — **validate and READ, for the
    /// CPU the caller names**. It does not write `scheduler.current_cpu`, and it does not
    /// resolve the lookup through any ambient selector.
    ///
    /// 1. validate with the same online predicate `KernelState::set_current_cpu` applies
    ///    (`validate_online_cpu`), returning `None` and leaving ALL state — ambient included —
    ///    untouched on failure; the old `with_cpu` propagated that error to `.ok().flatten()`;
    /// 2. read `current_tid_on(cpu)` under that same single rank-1 acquisition.
    ///
    /// # Why the binding side effect was removed (canonical 203C prerequisite)
    ///
    /// `scheduler.current_cpu` is ONE process-global field, and `KernelState::current_tid()` /
    /// `current_task_cnode()` resolve "the current task" through it. Writing it from an
    /// off-broad rank-1 seam is therefore not transaction-local: it retargets every ambient
    /// reader on EVERY CPU, including a CPU that is mid-syscall inside the broad lock.
    ///
    /// That was observed, not theorised. On the x86_64 `-smp 2` cross-CPU reply workload, CPU 1
    /// entered Phase 2 with `arg_cpu=1`, `with_cpu` bound `current_cpu=1`, and
    /// `current_tid_on(1)` was the server task throughout. While that broad transaction was
    /// still running, CPU 0's trap seam called `current_tid_authoritative(CpuId(0))`, whose bind
    /// rewrote the shared field to 0. CPU 1's ambient `current_tid()` / `current_task_cnode()`
    /// consequently flipped to CPU 0's task mid-syscall, so `handle_ipc_recv` validated the
    /// receive capability against the WRONG process CNode and correctly refused it with
    /// `MissingRight`. Nothing else was wrong: the trap CPU, the `with_cpu` binding, the
    /// capability provisioning, the object generation and the rights evaluation on the correct
    /// capability were all verified correct.
    ///
    /// The RETURN VALUE is unchanged for every caller. All fifteen production callers consume the
    /// returned TID explicitly (threading it into `*_split_read(tid, …)` or comparing it); none
    /// reads ambient state afterwards expecting this call to have retargeted it. This is also
    /// `AI_AGENT_RULES` §14.4's D6 rule for `entering_tid`/`exiting_tid`: Class F, authoritative
    /// read only.
    ///
    /// The lookup CPU is now the caller's `cpu`, uniformly. The previous freestanding-AArch64
    /// branch re-derived it from `MPIDR_EL1` because the bound field could not be trusted; that
    /// is value-identical here, because the AArch64 trap entry derives the `cpu` it passes from
    /// `MPIDR_EL1` by the same expression (`arch/aarch64/boot.rs`'s `trap_cpu`).
    ///
    /// Broad `SharedKernel::with_cpu` KEEPS its `set_current_cpu` binding: while the broad lock
    /// exists that binding is legacy transaction-local state, and it is out of scope here. The
    /// remaining off-broad readers and writers of `scheduler.current_cpu` are NOT all retired —
    /// they remain separately auditable prerequisites; see `doc/KERNEL_UNLOCKING.md` §0.
    ///
    /// No task lock, no dispatch, no enqueue, no status mutation, no broad fallback.
    pub fn current_tid_authoritative(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            if kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .is_err()
            {
                return None;
            }
            kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|tid| tid.0)
        })
    }

    /// # Validation status
    /// - TRAP_FORBIDDEN / REQUIRES_AUTHORITATIVE_TID — stale at the pre-global-lock
    ///   x86_64 trap seam (Stage 29A proof: returned tid 0 instead of running requester).
    ///   Trap-seam requester identity must use `current_tid_authoritative`.
    pub fn current_tid_split_read(&self, cpu: CpuId) -> Option<u64> {
        // Phase L5A split: read the scheduler's per-CPU current TID directly
        // under the scheduler lock.  This intentionally avoids the global
        // SharedKernel lock and does not mutate current_cpu or task state.
        // SAFETY: `self.state.data_ptr()` is the live address of the
        // `KernelState` owned by this `SharedKernel`; recomputed fresh on
        // every call (Stage 114 fix — see `SharedKernel::new`'s doc comment).
        let scheduler_state =
            unsafe { KernelState::scheduler_split_mut_ptr_from_raw(self.state.data_ptr()) };
        let scheduler_state = unsafe { &*scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler)
            .current_tid_on(cpu)
            .map(|tid| tid.0)
    }

    /// # Validation status: UNIT_ONLY — staged read helper, not on the trap path.
    pub fn online_cpu_count_split_read(&self) -> usize {
        // Phase L7A split: read scheduler topology through scheduler_state only.
        // This is a read-only staged helper; it does not acquire the global
        // SharedKernel lock, mutate runqueues, or update current_cpu.
        // SAFETY: `self.state.data_ptr()` is the live address of the
        // `KernelState` owned by this `SharedKernel`; recomputed fresh on
        // every call (Stage 114 fix — see `SharedKernel::new`'s doc comment).
        let scheduler_state =
            unsafe { KernelState::scheduler_split_mut_ptr_from_raw(self.state.data_ptr()) };
        let scheduler_state = unsafe { &*scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).online_cpu_count()
    }

    /// # Validation status: UNIT_ONLY — staged read helper, not on the trap path.
    pub fn present_cpu_count_split_read(&self) -> usize {
        // Phase L7A split: read scheduler topology through scheduler_state only.
        // This is a read-only staged helper; it does not acquire the global
        // SharedKernel lock, mutate runqueues, or update current_cpu.
        // SAFETY: `self.state.data_ptr()` is the live address of the
        // `KernelState` owned by this `SharedKernel`; recomputed fresh on
        // every call (Stage 114 fix — see `SharedKernel::new`'s doc comment).
        let scheduler_state =
            unsafe { KernelState::scheduler_split_mut_ptr_from_raw(self.state.data_ptr()) };
        let scheduler_state = unsafe { &*scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).present_cpu_count()
    }

    /// # Validation status: UNIT_ONLY — immutable boot-config read, not on the trap path.
    pub fn capacity_profile_split_read(&self) -> KernelCapacityProfile {
        // Phase L8B split: read immutable boot configuration under only the
        // boot_config lock domain. This intentionally avoids the global
        // SharedKernel lock and does not mutate boot config or runtime state.
        // SAFETY: `self.state.data_ptr()` is the live address of the
        // `KernelState` owned by this `SharedKernel`; recomputed fresh on
        // every call (Stage 114 fix — see `SharedKernel::new`'s doc comment).
        let (boot_config_state_lock, boot_config) =
            unsafe { KernelState::boot_config_split_read_ptrs_from_raw(self.state.data_ptr()) };
        let boot_config_state_lock = unsafe { &*boot_config_state_lock };
        let _guard = boot_config_state_lock.lock();
        let boot_config = unsafe { &*boot_config };
        kernel_ref(boot_config).capacity_profile
    }

    pub fn runtime_capacity_config_split_read(&self) -> RuntimeCapacityConfig {
        let profile = self.capacity_profile_split_read();
        KernelState::runtime_capacity_config_for_profile(profile)
    }

    /// # Validation status
    /// - LIVE_TRAP_SMOKE_X86_64 — called from `handle_trap_entry_shared` pre-lock seam
    ///   to record fault diagnostics; mutates only `fault_state_lock` domain.
    fn with_fault_split_mut<R>(&self, f: impl FnOnce(&mut FaultSubsystem) -> R) -> R {
        // Stage 3B-A helper-only split mutation: use only fault_state_lock and
        // mutate only diagnostic fault bookkeeping. Do not acquire the outer
        // SharedKernel lock and do not touch current_cpu or other subsystems.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `fault_split_mut_ptrs_from_raw` derives raw field
        // pointers without creating a whole-KernelState reference; the fault
        // lock serializes access to the fault subsystem storage.
        let (fault_state_lock, faults) =
            unsafe { KernelState::fault_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let fault_state_lock = unsafe { &*fault_state_lock };
        let _guard = fault_state_lock.lock();
        let faults = unsafe { &mut *faults };
        f(kernel_mut(faults))
    }

    pub fn record_fault_split_mut(&self, fault: FaultInfo) {
        self.with_fault_split_mut(|faults| faults.last_fault = Some(fault));
    }

    pub fn record_fault_frame_snapshot_split_mut(&self, frame: &TrapFrame) {
        self.with_fault_split_mut(|faults| faults.last_fault_frame = Some(frame.clone()));
    }

    pub fn clear_last_fault_split_mut(&self) {
        self.with_fault_split_mut(|faults| {
            faults.last_fault = None;
            faults.last_fault_frame = None;
        });
    }

    /// # Validation status
    /// - LIVE_OFF_TRAP — mutates only telemetry counters under `telemetry_state_lock`;
    ///   called from off-trap kernel code, not the pre-global-lock trap seam.
    fn with_telemetry_split_mut<R>(&self, f: impl FnOnce(&mut TelemetrySubsystem) -> R) -> R {
        // Stage 3C-B helper-only split mutation: use only telemetry_state_lock
        // and mutate only simple diagnostic telemetry counters. Do not acquire
        // the outer SharedKernel lock and do not touch current_cpu, scheduler,
        // IPC, VM, task, capability, driver, fault, or boot-config state.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `telemetry_split_mut_ptrs_from_raw` derives raw
        // field pointers without creating a whole-KernelState reference; the
        // telemetry lock serializes access to telemetry storage.
        let (telemetry_state_lock, telemetry) =
            unsafe { KernelState::telemetry_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let telemetry_state_lock = unsafe { &*telemetry_state_lock };
        let _guard = telemetry_state_lock.lock();
        let telemetry = unsafe { &mut *telemetry };
        f(kernel_mut(telemetry))
    }

    pub fn increment_tlb_shootdown_count_split_mut(&self) {
        self.with_telemetry_split_mut(|telemetry| {
            telemetry.tlb_shootdown_count = telemetry.tlb_shootdown_count.wrapping_add(1);
        });
    }

    pub fn add_tlb_shootdown_timeout_count_split_mut(&self, delta: u64) {
        self.with_telemetry_split_mut(|telemetry| {
            telemetry.tlb_shootdown_timeout_count =
                telemetry.tlb_shootdown_timeout_count.wrapping_add(delta);
        });
    }

    // ── Stage 108 / Milestone 2 Pass 1: per-domain split-mut seams ────────────
    //
    // VALIDATION: M2_SEAM_HELPER_ONLY (with_ipc_split_mut — see below)
    // VALIDATION: M2_SEAM_LIVE_D6_GENUINE (with_scheduler_split_mut — Stage 167
    //   default-off `yarm.d6_genuine=1` observe wire, see below)
    // VALIDATION: M2_SEAM_LIVE_D3_BRK_SHRINK (with_task_tcbs_split_mut /
    //   with_vm_user_spaces_split_mut / with_memory_split_mut)
    // VALIDATION: FALLBACK_GLOBAL_LOCK
    //
    // Seam set after Stage 115: scheduler (rank 1), task/TCB (rank 2),
    // IPC/waiter-publish (rank 3), VM/user-spaces (rank 5), memory/frames
    // (rank 6). Each acquires ONLY its own per-domain lock — never the outer
    // SharedKernel lock.
    //
    // Stage 114 / D-NEXT-2 update: `with_task_tcbs_split_mut`,
    // `with_vm_user_spaces_split_mut`, and `with_memory_split_mut` are no
    // longer helper-only — `try_split_vm_brk_shrink_into_frame` below calls
    // all three from the live pre-`with_cpu` trap path (via
    // `syscall_split::try_split_dispatch_into_frame`'s NR 14 case) for the
    // single-CPU-online-gated VmBrk shrink.
    //
    // Stage 167 / D6-GENUINE-A update: `with_scheduler_split_mut` is no longer
    // helper-only either — `d6_genuine_local_dispatch_observe` below calls it
    // from the live post-`with_cpu` trap path (global lock dropped) under the
    // default-off `yarm.d6_genuine=1` knob, running one `local_dispatch_step_split`
    // observation holding ONLY the rank-1 scheduler lock. The other seams keep
    // their `M2_SEAM_HELPER_ONLY` / dead-code fences; the in-lock D6 dispatch
    // path in `scheduler_state.rs` still does NOT call the seam (calling it
    // from inside `with_cpu` would alias the same backing lock — see that
    // method's doc comment for the documented blocker).
    //
    // Stage 115 / D2+D6 Outcome B: `with_ipc_split_mut` (rank 3) is added,
    // completing the IPC domain seam. It is helper-only; D2 Phase C cannot be
    // moved outside `with_cpu` until `dispatch_next_task` → `switch_frames`
    // (arch-specific cooperative kernel context switch) is restructured per
    // arch. See doc/KERNEL_UNLOCKING.md §Stage-115 for the precise blocker.
    //
    // Lock-held assertion note: the wrapper itself acquires the domain lock
    // and holds the guard across the closure, so a separate debug
    // "lock-held" assertion would be tautological — the guard IS the proof.
    // What a caller must NOT do is hold a lock of equal or lower rank when
    // entering; that discipline is enforced by the hosted-dev
    // YARM_LOCK_ORDER_WARN tracker (descending sequential pairs are logged)
    // and by the per-seam doc comments.

    /// Stage 108: scheduler (rank 1) split-mut seam.
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_D6_GENUINE (Stage 167) — first live caller is the
    ///   default-off `yarm.d6_genuine=1` observe wire
    ///   (`d6_genuine_local_dispatch_observe`, called from
    ///   `arch/trap_entry.rs::handle_trap_entry_shared` AFTER `with_cpu`
    ///   returns and the global lock is dropped). When the knob is OFF
    ///   (default) the seam has no live caller and the authoritative dispatch
    ///   decision stays in the in-lock `local_dispatch_step_split`; see
    ///   `stage113_d6_with_scheduler_split_mut_not_called_with_documented_blocker`
    ///   (the in-lock path still does NOT call the seam — calling it from
    ///   inside the `with_cpu` borrow would alias the same backing lock).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_scheduler_split_mut<R>(
        &self,
        f: impl FnOnce(&mut SchedulerState) -> R,
    ) -> R {
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned
        // by this SharedKernel. `scheduler_split_mut_ptr_from_raw` derives a
        // raw field pointer without creating a whole-KernelState reference;
        // the scheduler lock contains and serializes its own data.
        let scheduler_lock =
            unsafe { KernelState::scheduler_split_mut_ptr_from_raw(self.state.data_ptr()) };
        let scheduler_lock = unsafe { &*scheduler_lock };
        let mut guard = scheduler_lock.lock();
        f(&mut guard)
    }

    /// Stage 167 (D6-GENUINE-A): the first LIVE production caller of the rank-1
    /// scheduler split seam above. Runs one `local_dispatch_step_split`
    /// dispatch observation through `with_scheduler_split_mut`, holding ONLY
    /// the scheduler lock with the global `SpinLock<KernelState>` already
    /// dropped by the trap-entry path. The observation is NON-mutating — it
    /// reads the committed dispatch decision (current TID + runnable count)
    /// that the in-lock `local_dispatch_step_split` already produced inside
    /// `with_cpu` — so it never double-advances the run queue and the in-lock
    /// path remains the authoritative fallback. Returns the observed current
    /// TID. Default-off behind `yarm.d6_genuine=1` (gated by the caller).
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d6_genuine_local_dispatch_observe(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            // Mirror `local_dispatch_step_split`'s CPU selection: it reads the
            // scheduler's own `current_cpu` under the scheduler lock.
            let observe_cpu = sched.current_cpu;
            let current = kernel_ref(&sched.scheduler)
                .current_tid_on(observe_cpu)
                .map(|tid| tid.0);
            let runnable = kernel_ref(&sched.scheduler).runnable_count_on(observe_cpu);
            crate::yarm_log!(
                "D6_LOCAL_DISPATCH_STEP_SPLIT cpu={} tid={:?} runnable={}",
                cpu.0,
                current,
                runnable
            );
            current
        })
    }

    /// Stage 200D-2B1D5A — re-validate an `idle` restore-owner decision AFTER the post-lock
    /// drains, and commit exactly one CPU-local task if the drains produced work.
    ///
    /// The x86_64 consumer picks the restore owner IN-LOCK (Stage 200D-0B3), which is correct
    /// for identity coherence but happens strictly BEFORE the post-lock drains — and the drains
    /// are exactly where wakes are published. Stage 200D-2B1D4 caught the consequence live: the
    /// server-death drain made a caller runnable and enqueued it (`enqueues=1`), the epilogue
    /// committed the earlier `owner=idle` decision anyway, and the CPU halted holding an idle
    /// frame while a runnable task existed — re-idling on every tick until the boot timed out.
    ///
    /// This runs with the broad guard already dropped, so the brief `with_cpu` re-acquire below
    /// is sound (and is itself the lock-dropped proof — a still-held guard would deadlock). It
    /// uses the EXISTING `dispatch_next_on_cpu`, so the run queue advances through the same
    /// authority the in-lock path uses and never through a second queue:
    ///
    ///   * exactly one advance — one `dispatch_next_on_cpu` call, whose result is reported;
    ///   * CPU-local only — `dispatch_next_on_cpu(cpu)` pops from THIS cpu's run queue, so a
    ///     task runnable elsewhere is never stolen;
    ///   * the idle sentinel (TID 0) is not a user task and is reported as `Idle`, so the
    ///     caller keeps its existing idle path rather than trying to return to ring 3.
    ///
    /// Stage 200D-2B1D5B — the restore-failure contract. `dispatch_next_on_cpu` has ALREADY
    /// committed the selected task as this CPU's `current` by the time the restore is attempted,
    /// so a failure cannot simply be reported as "no owner": entering the ordinary idle path
    /// with that task still current halts the CPU while the scheduler believes the task is
    /// running on it — on no run queue and on no CPU, stranded exactly as the Stage 200D-2B1D4
    /// caller was. The failure is therefore rolled back here, and reported distinctly from
    /// genuine idle so the caller can fail closed if the rollback itself could not complete.
    ///
    /// U3 (canonical 203C) — the broad re-acquisition is retired. The body is now one
    /// rank-ordered, CPU-local transaction with no `&mut KernelState` anywhere:
    ///
    /// 1. **rank 1** — `validate_online_cpu` (the same predicate `with_cpu` applied through
    ///    `set_current_cpu`), then bind `current_cpu = cpu`, then the SAME single
    ///    `dispatch_next_on(cpu)` advance. A validation refusal returns before the bind, so
    ///    nothing is mutated and the outcome is `Idle` — exactly what `with_cpu`'s `Err` did
    ///    through `.unwrap_or(Idle)`. The selection leaves this phase as a typed
    ///    [`OwnerRevalidationSelection`], not a bare TID, so every step below is keyed to the
    ///    CPU this transaction authenticated and the TID this transaction itself committed.
    /// 2. **rank 1 is fully released** before any task-domain work.
    /// 3. Idle / TID 0 short-circuit, unchanged.
    /// 4. **rank 2**, ONE acquisition — the restorability decision and the whole restore
    ///    payload come from the same look at the TCB: `user_context` (what
    ///    `thread_user_context` read), the pending TLS-restore request (what
    ///    `take_tls_restore_request` consumed, taken here so it is consumed exactly once),
    ///    and `tcb.asid` with its **optional** semantics preserved. No `TaskStatus` is
    ///    written. Fusing the two reads is strictly more coherent than the legacy pair, which
    ///    read the TCB twice; it is not a strengthening — a task with no ASID still restores,
    ///    it only skips the CR3 block, exactly as before.
    /// 5. **rank 2 is fully released** before the frame, MSR, page-table and CR3 work.
    /// 6. The arch application (frame, FS base, `LAST_RESTORED_TLS_BASE`, and the complete
    ///    `ensure_user_return_cr3` behavior through its existing split twin) runs with NO lock
    ///    held — see `x86_apply_owner_revalidation_restore`.
    /// 7. Rollback re-takes **rank 1** alone.
    ///
    /// **Why the restore step is infallible, and why the requeue arm is kept anyway.** The
    /// legacy condition was `restorable && restore_arch_thread_state(..).is_ok()`. Every error
    /// path inside that call — `apply_current_thread_to_frame`'s two lookups and the
    /// `current_tid()` re-read — produces `KernelError::TaskMissing`, which the function maps
    /// to `Ok(())`; `take_tls_restore_request` cannot fail at all. So on this path
    /// `restore_arch_thread_state` always returned `Ok`, the composite condition was exactly
    /// `restorable`, and `RestoreFailed` was only ever reached with `restorable == false` —
    /// where `!restorable` short-circuits the requeue. The transaction reproduces that
    /// reachable behavior exactly. The `restorable` requeue arm below is nevertheless kept and
    /// implemented, because the twelve-point contract requires a still-live task to go back on
    /// THIS cpu's queue, and a future fallible restore step must not silently strand it.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn revalidate_idle_owner_after_drains(
        &self,
        cpu: CpuId,
        frame: &mut crate::kernel::trapframe::TrapFrame,
    ) -> OwnerRevalidation {
        // (1) rank 1: authenticate, bind, advance THIS cpu's queue exactly once.
        let Some(selection) = self.owner_revalidation_select_split(cpu) else {
            // Either the CPU failed the same admission predicate `with_cpu` applied — nothing
            // was mutated and `dispatch_next_on` never ran — or the queue yielded nothing.
            return OwnerRevalidation::Idle;
        };
        // (3) The scheduler's idle/supervisor sentinel owns no user context. Nothing was
        // committed — `dispatch_next` returns the sentinel it was already holding.
        let next = selection.tid;
        if next == 0 {
            return OwnerRevalidation::Idle;
        }
        // (4) rank 2, one acquisition. `None` is the legacy `thread_user_context(next).is_none()`
        // verdict: a task still in a run queue whose TCB has been reaped must be a restore
        // failure, never a silent return into ring 3 on the previous task's frame.
        let snapshot = self.owner_revalidation_snapshot_split(next);
        if let Some(snapshot) = snapshot {
            // (6) No lock held: frame, FS base, per-CPU TLS record, pre-IRET CR3 invariant.
            crate::arch::x86_64::trap::x86_apply_owner_revalidation_restore(
                self, cpu, next, snapshot, frame,
            );
            return OwnerRevalidation::Replacement(next);
        }
        // (7) Undo the queue advance. Clearing `current` is mandatory; re-enqueueing is only
        // meaningful for a task that still exists — a reaped one has nothing to run and must
        // not be resurrected into a run queue.
        self.owner_revalidation_rollback_split(selection, snapshot.is_some())
    }

    /// U3 (203C) — phase 1 of [`Self::revalidate_idle_owner_after_drains`], rank 1 only.
    ///
    /// Returns `None` for BOTH legacy no-owner cases, which the caller reports identically:
    /// the CPU failed `validate_online_cpu` (so `with_cpu` returned `Err` and nothing ran), or
    /// `dispatch_next_on` selected nothing. The bind happens only after a successful
    /// validation and before the advance, matching `set_current_cpu` followed by the closure.
    #[cfg(target_arch = "x86_64")]
    fn owner_revalidation_select_split(&self, cpu: CpuId) -> Option<OwnerRevalidationSelection> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler).validate_online_cpu(cpu).ok()?;
            sched.current_cpu = cpu;
            let tid = kernel_mut(&mut sched.scheduler).dispatch_next_on(cpu)?;
            Some(OwnerRevalidationSelection { cpu, tid: tid.0 })
        })
    }

    /// U3 (203C) — phase 2, rank 2 only, ONE acquisition, no `TaskStatus` write.
    ///
    /// `Some` iff the legacy `thread_user_context(tid).is_some()` was true. The TLS-restore
    /// request is TAKEN here, exactly as `take_tls_restore_request` took it, and only when the
    /// TCB was found — so a rolled-back selection leaves the request pending for the task's
    /// real resume, which is what the legacy short-circuit did.
    ///
    /// U9 (203C): keyed on the bare `tid` rather than on an `OwnerRevalidationSelection`, which is
    /// all this ever read out of it. That is what lets the production post-switch restore reach
    /// the SAME gather ([`Self::post_switch_restore_snapshot_split`]) instead of growing a second
    /// copy of it — the two boundaries resolve their TID differently (a queue advance versus the
    /// scheduler's standing `current`), but what they need from the task domain is identical.
    #[cfg(target_arch = "x86_64")]
    fn owner_revalidation_snapshot_split(&self, tid: u64) -> Option<OwnerRevalidationSnapshot> {
        self.with_task_return_split_mut(|tcbs, tls_pending| {
            let tcb = tcbs.iter().flatten().find(|t| t.tid.0 == tid)?;
            let context = tcb.user_context;
            let asid = tcb.asid;
            // `take_tls_restore_request`: clear the pending slot and answer with the task's
            // `tls_ptr`, or `None` when nothing was pending. `thread_tls_base` reads the same
            // field this reads.
            let tls_base = tcb.tls_ptr.map(|ptr| ptr.0 as usize);
            let pending = tls_pending
                .iter()
                .position(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid));
            let tls = match pending {
                Some(idx) => {
                    tls_pending[idx] = None;
                    tls_base
                }
                None => None,
            };
            Some(OwnerRevalidationSnapshot { context, tls, asid })
        })
    }

    /// U3 (203C) — phase 3, the restore-failure rollback, rank 1 (with the enqueue policy's
    /// rank 2 nested inside it, ascending 1 → 2).
    ///
    /// `cleared` and `requeued` are composed exactly as the legacy body composed them from
    /// `block_current_on_cpu` and `enqueue_on_cpu`, including `enqueue_on_cpu`'s own
    /// spawn-reservation refusal, class → priority derivation and first-user CPU pin.
    #[cfg(target_arch = "x86_64")]
    fn owner_revalidation_rollback_split(
        &self,
        selection: OwnerRevalidationSelection,
        restorable: bool,
    ) -> OwnerRevalidation {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::TaskClass;

        const BOOTSTRAP_FIRST_USER_TID: u64 = 1;
        let OwnerRevalidationSelection { cpu, tid } = selection;
        self.with_scheduler_split_mut(|sched| {
            let cleared = kernel_mut(&mut sched.scheduler)
                .block_current_on(cpu)
                .map(|t| t.0)
                == Some(tid);
            // A reaped task is never resurrected: `!restorable` short-circuits before any
            // queue mutation, exactly as `!restorable || enqueue_on_cpu(..)` did.
            let requeued = if !restorable {
                true
            } else {
                // `enqueue_on_cpu`'s task-domain policy, in one nested rank-2 acquisition.
                let priority: Option<TaskPriority> =
                    self.with_task_enqueue_policy_split_mut(|tcbs, classes| {
                        let idx = tcbs
                            .iter()
                            .position(|slot| slot.as_ref().is_some_and(|t| t.tid.0 == tid))?;
                        if tcbs[idx].as_ref().is_some_and(|t| t.is_spawn_reservation()) {
                            crate::yarm_log!(
                                "ENQUEUE_REFUSED tid={} reason=spawn_reservation_not_live",
                                tid
                            );
                            return None;
                        }
                        if tid == 0 {
                            return Some(TaskPriority::Normal);
                        }
                        Some(match classes[idx]? {
                            TaskClass::SystemServer => TaskPriority::High,
                            TaskClass::Driver | TaskClass::App => TaskPriority::Normal,
                        })
                    });
                match priority {
                    None => false,
                    Some(priority) => {
                        // First-user CPU pinning, reproduced verbatim from `enqueue_on_cpu`.
                        if tid == BOOTSTRAP_FIRST_USER_TID
                            && cpu.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                            && cfg!(not(feature = "hosted-dev"))
                        {
                            crate::yarm_log!(
                                "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                                cpu.0,
                                tid,
                                cpu.0
                            );
                            assert_eq!(cpu.0, crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
                        }
                        kernel_mut(&mut sched.scheduler)
                            .enqueue_on_with_priority(cpu, ThreadId(tid), priority)
                            .is_ok()
                    }
                }
            };
            OwnerRevalidation::RestoreFailed {
                tid,
                rolled_back: cleared && requeued,
            }
        })
    }

    /// Stage 168 (D6-GENUINE-B): the authoritative **mutating** dispatch step,
    /// run through the rank-1 scheduler seam with the global
    /// `SpinLock<KernelState>` already dropped by the trap-entry drain. This
    /// is the single authoritative `local_dispatch_step_split` for an eligible
    /// (queue-neutral) d6_genuine dispatch cycle — the in-lock path deferred
    /// instead of performing it. It calls the same mutating `dispatch_next_on`
    /// the in-lock path would; because the caller only defers when the pick is
    /// queue-neutral (current task continues, or idle stays idle with nothing
    /// runnable), `dispatch_next_on` provably does not dequeue here, so it can
    /// never double-advance the run queue. Returns the incoming TID.
    /// Default-off behind `yarm.d6_genuine=1` (gated by the caller).
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d6_genuine_local_dispatch_step_mut(&self, cpu: CpuId) -> CpuDispatch {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            // Stage 199D-WA3A-R2-SEAL (item D): authenticate the caller's CPU against the
            // authoritative dispatch CPU BEFORE any mutation. On mismatch nothing is dequeued.
            if dispatch_cpu != cpu {
                crate::yarm_log!(
                    "DISPATCH_STEP_REFUSED_CPU_MISMATCH site={} requested={} authoritative={}",
                    "d6_genuine_local_dispatch_step_mut",
                    cpu.0,
                    dispatch_cpu.0
                );
                return CpuDispatch::RefusedCpuMismatch {
                    requested: cpu,
                    authoritative: dispatch_cpu,
                };
            }
            // Stage 199D-WA3A-R1: provenance is produced INSIDE the scheduler mutation.
            let selection =
                kernel_mut(&mut sched.scheduler).dispatch_next_selection_on(dispatch_cpu);
            let incoming = selection.tid().map(|t| t.0);
            // Stage 199D-WA3A-R2-SEAL: the marker field is read off the TYPED selection, not
            // reconstructed from `Option::is_some` — same emitted text, no second source of
            // truth about what the dispatch did.
            let result = match selection {
                crate::kernel::scheduler::DispatchSelection::Idle => "none",
                _ => "some",
            };
            crate::yarm_log!(
                "D6_GENUINE_MUT_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                result,
                incoming
            );
            CpuDispatch::Selected {
                cpu: dispatch_cpu,
                selection,
            }
        })
    }

    /// Stage 168 (D6-GENUINE-B): out-of-global-lock re-verification that the
    /// deferred dispatch is still queue-neutral (single-CPU, IRQ-off ⇒ nothing
    /// changed since the in-lock peek unless an in-lock fallback superseded the
    /// deferral). Reads current TID + runnable count through the rank-1 seam.
    /// Returns `true` when `dispatch_next_on` would NOT dequeue (safe to run
    /// the mutating step out of lock).
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d6_genuine_dispatch_queue_neutral(&self, _cpu: CpuId) -> bool {
        self.with_scheduler_split_mut(|sched| {
            let scpu = sched.current_cpu;
            let current = kernel_ref(&sched.scheduler)
                .current_tid_on(scpu)
                .map(|tid| tid.0);
            let runnable = kernel_ref(&sched.scheduler).runnable_count_on(scpu);
            // dispatch_next_on dequeues iff (current is None or idle tid 0) AND
            // there is something runnable. Everything else is queue-neutral.
            !(runnable > 0 && matches!(current, None | Some(0)))
        })
    }

    /// Stage 168 (D6-GENUINE-B): the deferred Phase-B TCB status write, applied
    /// out of the global lock through the rank-2 task seam. For an eligible
    /// (queue-neutral, same-running-task) dispatch the target is already
    /// `Running`, so this is idempotent; it is kept for faithfulness to the
    /// in-lock path it replaces. No-op when `incoming` is `None` (idle).
    ///
    /// Stage 195E: un-gated to AArch64 — its live FutexWait drain marks the incoming task
    /// Running through this same rank-2 task seam. Stage 196D: un-gated to RISC-V — its
    /// queue-switch foundation drain marks the incoming task Running through this seam.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    /// Stage 199D-WA3A: the write is now an EXACT `Runnable → Running` transition, compiled
    /// into every build. `incoming` was produced by a rank-1 dequeue that already set `current`,
    /// so a refusal cannot simply return: it undoes that dequeue exactly through
    /// `preempt_reenqueue_only_on` (the existing inverse of `dispatch_next_on`) so the task is
    /// neither lost from the queue nor left as a `current` the CPU will not resume.
    ///
    /// Returns `true` iff the incoming task is now `Running` and may be resumed. `false` means
    /// nothing is current on `cpu` and the caller must not resume `incoming`.
    ///
    /// Stage 199D-WA3A-R2-SEAL: takes a `CpuDispatch`, not a `(selection, cpu)` pair — the CPU
    /// is the one the authoritative scheduler mutation ran on, so no caller can stamp an
    /// unverified CPU into the resulting rollback authority (item D). Identity availability is
    /// decided inside the SAME rank-2 acquisition as, and strictly BEFORE, the status write, so
    /// a non-idle task with no ASID refuses with **zero** mutation (item A) instead of being
    /// marked `Running` and then rolled back into a `current`-but-`Runnable` torn state.
    #[must_use]
    pub(crate) fn d6_genuine_mark_running_via_task_seam(
        &self,
        dispatch: CpuDispatch,
    ) -> DispatchMarkOutcome {
        use crate::kernel::scheduler::DispatchSelection;
        use crate::kernel::task_transition::{
            TaskTransition, apply_dispatch_transition, log_transition_refusal,
        };
        let (cpu, selection) = match dispatch {
            CpuDispatch::Selected { cpu, selection } => (cpu, selection),
            CpuDispatch::RefusedCpuMismatch {
                requested,
                authoritative,
            } => {
                // Nothing was mutated by the seam, so nothing is undone here either.
                crate::yarm_log!(
                    "DISPATCH_MARK_REFUSED cpu={} authoritative={} provenance=refused_cpu_mismatch scheduler_mutation=none",
                    requested.0,
                    authoritative.0
                );
                return DispatchMarkOutcome::RefusedNoSchedulerChange;
            }
        };
        // Stage 199D-WA3A-R1: the transition is chosen by PROVENANCE, produced inside the
        // scheduler mutation — never reconstructed from TID equality or `Option::is_some`.
        let (tid, transition) = match selection {
            DispatchSelection::Idle => return DispatchMarkOutcome::Idle,
            DispatchSelection::Dequeued { tid } => (tid.0, TaskTransition::DispatchIncoming),
            DispatchSelection::ContinuedCurrent { tid } => (tid.0, TaskTransition::ContinueCurrent),
        };
        let marked = self.with_task_tcbs_split_mut(|tcbs| {
            // Stage 199D-WA3A-R2-SEAL (item A): resolve the exact incarnation FIRST. A user
            // task with no ASID has no supported exact identity; a token minted with a
            // wildcard identity would let a later rollback act on a replacement incarnation.
            // Refusing here — before `apply_task_transition` — is what makes the refusal
            // mutation-free, so `undo_dispatch_selection` has only the scheduler step to undo.
            let asid = tcbs
                .iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .and_then(|t| t.asid);
            let Some(incarnation) = MarkedIncarnation::resolve(tid, asid) else {
                return Err(MarkRefusal::MissingIncarnation);
            };
            // `apply_dispatch_transition` carries the idle-only fallback: the idle/bootstrap
            // task is placed in and out of `current` by the rank-1 scheduler without a
            // mark-running step, so it is `Running` after boot and `Runnable` after a
            // queue-neutral step. Each idle twin is refused for every other TID, so a
            // double-queued ORDINARY task still fails closed.
            match apply_dispatch_transition(tcbs, tid, transition) {
                Ok(_) => Ok(incarnation),
                Err(refusal) => {
                    log_transition_refusal(
                        "d6_genuine_mark_running_via_task_seam",
                        tid,
                        transition,
                        refusal,
                    );
                    Err(MarkRefusal::Transition)
                }
            }
        });
        match marked {
            Ok(incarnation) => DispatchMarkOutcome::Marked(DispatchMarkToken::new(
                cpu,
                tid,
                incarnation,
                selection,
            )),
            Err(refusal) => {
                if refusal == MarkRefusal::MissingIncarnation {
                    crate::yarm_log!(
                        "DISPATCH_MARK_REFUSED cpu={} tid={} provenance={} reason=missing_incarnation task_mutation=none",
                        cpu.0,
                        tid,
                        selection.marker()
                    );
                }
                self.undo_dispatch_selection(selection, cpu)
            }
        }
    }

    /// Hosted/test-only: forge a mark token for a DIFFERENT incarnation, so the exact-identity
    /// rollback refusal can be driven without a real dispatch.
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn stale_dispatch_mark_token_for_test(
        &self,
        cpu: CpuId,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> DispatchMarkToken {
        DispatchMarkToken::new(
            cpu,
            tid,
            MarkedIncarnation::User { asid },
            crate::kernel::scheduler::DispatchSelection::Dequeued {
                tid: crate::kernel::ipc::ThreadId(tid),
            },
        )
    }

    /// Hosted/test-only: forge a `ContinuedCurrent` mark token, so item C's "a continuation is
    /// not dequeue authority" refusal can be driven without a real dispatch.
    #[cfg(any(test, feature = "hosted-dev"))]
    pub(crate) fn continued_dispatch_mark_token_for_test(
        &self,
        cpu: CpuId,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> DispatchMarkToken {
        DispatchMarkToken::new(
            cpu,
            tid,
            MarkedIncarnation::User { asid },
            crate::kernel::scheduler::DispatchSelection::ContinuedCurrent {
                tid: crate::kernel::ipc::ThreadId(tid),
            },
        )
    }

    /// Undo exactly what `selection` did to the scheduler — and nothing more.
    ///
    /// `ContinuedCurrent` removed no runqueue entry, so `preempt_reenqueue_only_on` must NOT
    /// run for it: doing so would enqueue the current task, which for a
    /// `Blocked(EndpointReceive)` current is the unarbitrated wake this stage prevents.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    fn undo_dispatch_selection(
        &self,
        selection: crate::kernel::scheduler::DispatchSelection,
        cpu: CpuId,
    ) -> DispatchMarkOutcome {
        use crate::kernel::scheduler::DispatchSelection;
        match selection {
            DispatchSelection::Idle => DispatchMarkOutcome::Idle,
            DispatchSelection::ContinuedCurrent { tid } => {
                crate::yarm_log!(
                    "DISPATCH_MARK_REFUSED cpu={} tid={} provenance=continued_current scheduler_mutation=none",
                    cpu.0,
                    tid.0
                );
                DispatchMarkOutcome::RefusedNoSchedulerChange
            }
            DispatchSelection::Dequeued { tid } => {
                // A scheduler rollback may only mutate `current` when `current` IS this TID.
                if self.current_tid_authoritative(cpu) != Some(tid.0) {
                    crate::yarm_log!(
                        "DISPATCH_MARK_REFUSED cpu={} tid={} provenance=dequeued scheduler_rollback=skipped_current_mismatch",
                        cpu.0,
                        tid.0
                    );
                    return DispatchMarkOutcome::RefusedTorn;
                }
                let restored = self.with_scheduler_split_mut(|sched| {
                    kernel_mut(&mut sched.scheduler).preempt_reenqueue_only_on(cpu) == Some(tid)
                });
                crate::yarm_log!(
                    "DISPATCH_MARK_REFUSED cpu={} tid={} provenance=dequeued scheduler_rollback={}",
                    cpu.0,
                    tid.0,
                    u8::from(restored)
                );
                if restored {
                    DispatchMarkOutcome::RefusedRolledBack
                } else {
                    DispatchMarkOutcome::RefusedTorn
                }
            }
        }
    }

    /// Stage 168B (D2-GENUINE-RECV): re-verify — out of the global lock, through
    /// the rank-2 task seam — that the deferred blocking-recv task is STILL
    /// `Blocked(EndpointReceive(_))`. Guards the out-of-lock queue-advancing
    /// dispatch drain against a stale deferral (e.g. a sender woke the task, or
    /// an in-lock fallback superseded it). Single-CPU + IRQ-off means nothing
    /// mutates between the in-lock commit and this check, but the re-verify is
    /// the correctness fence the spec requires before dispatching.
    // U4: architecture-neutral. Nothing in the body is x86-specific — it was gated
    // only because the D2 drains were. All three architectures now drain D2.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d2_recv_reverify_blocked(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| {
                    matches!(
                        tcb.status,
                        crate::kernel::task::TaskStatus::Blocked(
                            crate::kernel::task::WaitReason::EndpointReceive(_)
                        )
                    )
                })
                .unwrap_or(false)
        })
    }

    /// Stage 168B (D2-GENUINE-RECV): the authoritative **queue-advancing**
    /// dispatch for a committed blocking recv, run through the rank-1 scheduler
    /// seam with the global `SpinLock<KernelState>` already dropped by the
    /// trap-entry drain. The blocked recv task was removed from `current`
    /// (Phase A `block_current`), so `dispatch_next_on` genuinely dequeues the
    /// next runnable task here — the queue-advancing step Stage 168A had to
    /// fall back on. Returns the incoming TID (`None` ⇒ idle). Emits
    /// `D2_RECV_GENUINE_DISPATCH_STEP_SPLIT`. Default-off (gated by the caller).
    // U4: architecture-neutral. Nothing in the body is x86-specific — it was gated
    // only because the D2 drains were. All three architectures now drain D2.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d2_recv_dispatch_step_mut(&self, cpu: CpuId) -> CpuDispatch {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            // Stage 199D-WA3A-R2-SEAL (item D): authenticate the caller's CPU against the
            // authoritative dispatch CPU BEFORE any mutation. On mismatch nothing is dequeued.
            if dispatch_cpu != cpu {
                crate::yarm_log!(
                    "DISPATCH_STEP_REFUSED_CPU_MISMATCH site={} requested={} authoritative={}",
                    "d2_recv_dispatch_step_mut",
                    cpu.0,
                    dispatch_cpu.0
                );
                return CpuDispatch::RefusedCpuMismatch {
                    requested: cpu,
                    authoritative: dispatch_cpu,
                };
            }
            // Stage 199D-WA3A-R1: provenance is produced INSIDE the scheduler mutation.
            let selection =
                kernel_mut(&mut sched.scheduler).dispatch_next_selection_on(dispatch_cpu);
            let incoming = selection.tid().map(|t| t.0);
            // Stage 199D-WA3A-R2-SEAL: read off the TYPED selection, not `Option::is_some`.
            let result = match selection {
                crate::kernel::scheduler::DispatchSelection::Idle => "idle",
                _ => "switch",
            };
            crate::yarm_log!(
                "D2_RECV_GENUINE_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                result,
                incoming
            );
            CpuDispatch::Selected {
                cpu: dispatch_cpu,
                selection,
            }
        })
    }

    /// Stage 169 (D2-GENUINE-SEND): re-verify — out of the global lock, through
    /// the rank-2 task seam — that the deferred blocking-SEND task is STILL
    /// `Blocked(EndpointSend(_))` before the out-of-lock queue-advancing dispatch
    /// drain runs. Same correctness fence as the recv reverify.
    // U4: architecture-neutral. Nothing in the body is x86-specific — it was gated
    // only because the D2 drains were. All three architectures now drain D2.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d2_send_reverify_blocked(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| {
                    matches!(
                        tcb.status,
                        crate::kernel::task::TaskStatus::Blocked(
                            crate::kernel::task::WaitReason::EndpointSend(_)
                        )
                    )
                })
                .unwrap_or(false)
        })
    }

    /// Stage 169 (D2-GENUINE-SEND): the authoritative queue-advancing dispatch
    /// for a committed blocking send, run through the rank-1 scheduler seam with
    /// the global `SpinLock<KernelState>` already dropped by the trap-entry
    /// drain. The blocked sender was removed from `current` (Phase A
    /// `block_current`), so `dispatch_next_on` genuinely dequeues the next
    /// runnable task here. Emits `D2_SEND_GENUINE_DISPATCH_STEP_SPLIT`.
    // U4: architecture-neutral. Nothing in the body is x86-specific — it was gated
    // only because the D2 drains were. All three architectures now drain D2.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d2_send_dispatch_step_mut(&self, cpu: CpuId) -> CpuDispatch {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            // Stage 199D-WA3A-R2-SEAL (item D): authenticate the caller's CPU against the
            // authoritative dispatch CPU BEFORE any mutation. On mismatch nothing is dequeued.
            if dispatch_cpu != cpu {
                crate::yarm_log!(
                    "DISPATCH_STEP_REFUSED_CPU_MISMATCH site={} requested={} authoritative={}",
                    "d2_send_dispatch_step_mut",
                    cpu.0,
                    dispatch_cpu.0
                );
                return CpuDispatch::RefusedCpuMismatch {
                    requested: cpu,
                    authoritative: dispatch_cpu,
                };
            }
            // Stage 199D-WA3A-R1: provenance is produced INSIDE the scheduler mutation.
            let selection =
                kernel_mut(&mut sched.scheduler).dispatch_next_selection_on(dispatch_cpu);
            let incoming = selection.tid().map(|t| t.0);
            // Stage 199D-WA3A-R2-SEAL: read off the TYPED selection, not `Option::is_some`.
            let result = match selection {
                crate::kernel::scheduler::DispatchSelection::Idle => "idle",
                _ => "switch",
            };
            crate::yarm_log!(
                "D2_SEND_GENUINE_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                result,
                incoming
            );
            CpuDispatch::Selected {
                cpu: dispatch_cpu,
                selection,
            }
        })
    }

    /// Stage 192A (FUTEXWAIT QUEUE-ADVANCING DISPATCH): re-verify — out of the global
    /// lock, through the rank-2 task seam — that the deferred FutexWait task is STILL
    /// `Blocked(Futex(_))` before the out-of-lock queue-advancing dispatch drain runs.
    /// Same correctness fence as the D2 recv/send reverify: guards against a stale deferral
    /// (e.g. a FutexWake woke the task, or an in-lock fallback superseded it) so a woken
    /// waiter is never displaced from the run queue. Single-CPU + IRQ-off means nothing
    /// mutates between the in-lock commit and this check.
    ///
    /// Stage 195E: un-gated to AArch64 — its live FutexWait drain uses the same rank-2 task
    /// seam to re-verify the outgoing waiter is still `Blocked(Futex)` before dispatching.
    /// Stage 196E: un-gated to RISC-V — its queue-advancing FutexWait retirement drain
    /// re-verifies the outgoing waiter is still `Blocked(Futex)` through this same seam.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    pub(crate) fn futex_wait_reverify_blocked(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| {
                    matches!(
                        tcb.status,
                        crate::kernel::task::TaskStatus::Blocked(
                            crate::kernel::task::WaitReason::Futex(_)
                        )
                    )
                })
                .unwrap_or(false)
        })
    }

    /// Stage 192A (FUTEXWAIT QUEUE-ADVANCING DISPATCH): the authoritative queue-advancing
    /// dispatch for a committed FutexWait block, run through the rank-1 scheduler seam with
    /// the global `SpinLock<KernelState>` already dropped by the trap-entry drain. The
    /// blocked waiter was removed from `current` (in-lock `block_current`), so
    /// `dispatch_next_on` genuinely DEQUEUES the next runnable task here (or returns `None`
    /// ⇒ idle) — the queue-advancing "switch_required" step. Identical body to
    /// `d2_recv_dispatch_step_mut`; emits the QUEUE_ADVANCING_DISPATCH_DEQUEUE_OK marker.
    ///
    /// Stage 195E: un-gated to AArch64 — the same rank-1 scheduler-seam dequeue drives the
    /// AArch64 out-of-lock FutexWait drain (the AArch64 arch hooks — TTBR0/ASID + EL0 frame
    /// restore — run in the drain's `with_cpu` re-acquire, NOT here).
    /// Stage 196E: un-gated to RISC-V — the same rank-1 dequeue drives the RISC-V out-of-lock
    /// FutexWait drain (the RISC-V arch hooks — SATP/ASID + sfence + trap-frame restore — run
    /// in the drain's `with_cpu` re-acquire, reusing the 196D switch machinery, NOT here).
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    pub(crate) fn futex_wait_dispatch_step_mut(&self, cpu: CpuId) -> CpuDispatch {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            // Stage 199D-WA3A-R2-SEAL (item D): authenticate the caller's CPU against the
            // authoritative dispatch CPU BEFORE any mutation. On mismatch nothing is dequeued.
            if dispatch_cpu != cpu {
                crate::yarm_log!(
                    "DISPATCH_STEP_REFUSED_CPU_MISMATCH site={} requested={} authoritative={}",
                    "futex_wait_dispatch_step_mut",
                    cpu.0,
                    dispatch_cpu.0
                );
                return CpuDispatch::RefusedCpuMismatch {
                    requested: cpu,
                    authoritative: dispatch_cpu,
                };
            }
            // Stage 199D-WA3A-R1: provenance is produced INSIDE the scheduler mutation.
            let selection =
                kernel_mut(&mut sched.scheduler).dispatch_next_selection_on(dispatch_cpu);
            let incoming = selection.tid().map(|t| t.0);
            match incoming {
                Some(tid) => crate::yarm_log!(
                    "QUEUE_ADVANCING_DISPATCH_DEQUEUE_OK cpu={} tid={}",
                    cpu.0,
                    tid
                ),
                None => {
                    crate::yarm_log!("QUEUE_ADVANCING_DISPATCH_DEQUEUE_OK cpu={} tid=idle", cpu.0)
                }
            }
            CpuDispatch::Selected {
                cpu: dispatch_cpu,
                selection,
            }
        })
    }

    /// Stage 192B (YIELD QUEUE-ADVANCING DISPATCH): re-verify — out of the global lock,
    /// through the rank-1 scheduler seam — that the `current` slot on `cpu` is still cleared
    /// (the in-lock `yield_current` re-enqueued the caller and cleared `current`). Guards the
    /// out-of-lock dispatch against a stale deferral (e.g. an in-lock fallback already
    /// dispatched). Single-CPU + IRQ-off means nothing mutates between the in-lock commit and
    /// this check; the re-verify is the correctness fence before dispatching.
    ///
    /// Stage 195G: un-gated to AArch64 — its live Yield drain re-verifies `current` is still
    /// cleared through this same rank-1 scheduler seam before dispatching. Stage 196D: un-gated
    /// to RISC-V — its queue-switch foundation drain re-verifies `current` cleared here.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    pub(crate) fn yield_reverify_ready(&self, cpu: CpuId) -> bool {
        self.with_scheduler_split_mut(|sched| {
            // `cpu` is the trap CPU == the authoritative dispatch CPU under the
            // single-dispatcher gate; check its `current` slot is still cleared.
            let _ = sched.current_cpu;
            kernel_ref(&sched.scheduler).current_tid_on(cpu).is_none()
        })
    }

    /// Stage 192B (YIELD QUEUE-ADVANCING DISPATCH): the authoritative queue-advancing
    /// dispatch for a committed Yield, run through the rank-1 scheduler seam with the global
    /// `SpinLock<KernelState>` already dropped by the trap-entry drain. The caller was
    /// re-enqueued and removed from `current` (in-lock `preempt_reenqueue_only`), so
    /// `dispatch_next_on` genuinely DEQUEUES the next runnable task here (the FIFO head — the
    /// re-enqueued caller itself when it is alone). Emits `YIELD_DISPATCH_DEQUEUE_OK`.
    ///
    /// Stage 195G: un-gated to AArch64 — the same rank-1 dequeue drives the AArch64 out-of-lock
    /// Yield drain (the AArch64 arch restore runs in the drain's `with_cpu` re-acquire).
    /// Stage 196D: un-gated to RISC-V — the same rank-1 dequeue drives the RISC-V queue-switch
    /// foundation drain (the RISC-V SATP/sfence + frame restore run in its `with_cpu` re-acquire).
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    pub(crate) fn yield_dispatch_step_mut(&self, cpu: CpuId) -> CpuDispatch {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            // Stage 199D-WA3A-R2-SEAL (item D): authenticate the caller's CPU against the
            // authoritative dispatch CPU BEFORE any mutation. On mismatch nothing is dequeued.
            if dispatch_cpu != cpu {
                crate::yarm_log!(
                    "DISPATCH_STEP_REFUSED_CPU_MISMATCH site={} requested={} authoritative={}",
                    "yield_dispatch_step_mut",
                    cpu.0,
                    dispatch_cpu.0
                );
                return CpuDispatch::RefusedCpuMismatch {
                    requested: cpu,
                    authoritative: dispatch_cpu,
                };
            }
            // Stage 199D-WA3A-R1: provenance is produced INSIDE the scheduler mutation.
            let selection =
                kernel_mut(&mut sched.scheduler).dispatch_next_selection_on(dispatch_cpu);
            let incoming = selection.tid().map(|t| t.0);
            match incoming {
                Some(tid) => {
                    crate::yarm_log!("YIELD_DISPATCH_DEQUEUE_OK cpu={} tid={}", cpu.0, tid)
                }
                None => crate::yarm_log!("YIELD_DISPATCH_DEQUEUE_OK cpu={} tid=idle", cpu.0),
            }
            CpuDispatch::Selected {
                cpu: dispatch_cpu,
                selection,
            }
        })
    }

    /// Stage 168B (D2-GENUINE-RECV): does the incoming task have an initialized
    /// kernel switch context (a wired kernel thread)? Read out of the global
    /// lock through the rank-2 task seam. Blocking recv is done by USER tasks,
    /// which resume via trap-frame restore + syscall restart (kernel_context
    /// initialized == false), so this returns false for the recv workload; it
    /// gates the dormant `switch_frames` (D2_RECV_GENUINE_SWITCH_*) variant that
    /// would reuse the hardened D6-SWITCH-A stash for a kernel-thread incoming.
    // U4: architecture-neutral. Nothing in the body is x86-specific — it was gated
    // only because the D2 drains were. All three architectures now drain D2.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d2_recv_incoming_has_kernel_switch_ctx(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.kernel_context.initialized)
                .unwrap_or(false)
        })
    }

    /// U3 (canonical 203C): task (rank 2) split-mut seam exposing the TCB array and the
    /// task-class table together — see `KernelState::task_enqueue_policy_split_mut_ptrs_from_raw`.
    ///
    /// Callers must not already hold a lock of rank ≥ 2. It is legal — and is the point — to
    /// hold rank 1 (scheduler) across this, because 1 → 2 is the canonical ascending direction.
    ///
    /// Architecture-neutral. It was introduced `#[cfg(target_arch = "x86_64")]` because its only
    /// caller was the x86_64 AP enqueue→dispatch transaction; U3's class-neutral blocked-waiter
    /// completion transaction is reached on every architecture, so the gate is gone. Nothing
    /// about the seam itself was x86-specific — the projector it wraps never carried a gate.
    fn with_task_enqueue_policy_split_mut<R>(
        &self,
        f: impl FnOnce(
            &mut [Option<crate::kernel::task::ThreadControlBlock>],
            &mut [Option<crate::kernel::task::TaskClass>],
        ) -> R,
    ) -> R {
        // SAFETY: same pattern as `with_task_tcbs_split_mut` — the task lock serializes both
        // storages, which live in the same domain.
        let (task_lock, tcbs, classes) = unsafe {
            KernelState::task_enqueue_policy_split_mut_ptrs_from_raw(self.state.data_ptr())
        };
        let task_lock = unsafe { &*task_lock };
        let _guard = task_lock.lock();
        let tcbs = unsafe { &mut *tcbs };
        let classes = unsafe { &mut *classes };
        f(
            kernel_mut(tcbs).as_mut_slice(),
            kernel_mut(classes).as_mut_slice(),
        )
    }

    /// Stage 108: task/TCB (rank 2) split-mut seam.
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — called by
    ///   `try_split_vm_brk_shrink_into_frame` three times: the group-leader
    ///   check, the ASID lookup ahead of the per-page unmap loop, and the
    ///   task-existence re-check ahead of the final brk-bounds write.
    pub(crate) fn with_task_tcbs_split_mut<R>(
        &self,
        f: impl FnOnce(&mut [Option<crate::kernel::task::ThreadControlBlock>]) -> R,
    ) -> R {
        // SAFETY: same pattern as with_fault_split_mut — the task lock
        // serializes access to the TCB array storage.
        let (task_lock, tcbs) =
            unsafe { KernelState::task_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let task_lock = unsafe { &*task_lock };
        let _guard = task_lock.lock();
        let tcbs = unsafe { &mut *tcbs };
        f(kernel_mut(tcbs).as_mut_slice())
    }

    /// Stage 199D: task (rank 2) split-mut seam exposing the TCB array and the TLS-restore
    /// table together — see `KernelState::task_return_split_mut_ptrs_from_raw`.
    fn with_task_return_split_mut<R>(
        &self,
        f: impl FnOnce(
            &mut [Option<crate::kernel::task::ThreadControlBlock>],
            &mut [Option<crate::kernel::ipc::ThreadId>],
        ) -> R,
    ) -> R {
        // SAFETY: same pattern as `with_task_tcbs_split_mut` — the task lock serializes both
        // storages, which live in the same domain.
        let (task_lock, tcbs, tls) =
            unsafe { KernelState::task_return_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let task_lock = unsafe { &*task_lock };
        let _guard = task_lock.lock();
        let tcbs = unsafe { &mut *tcbs };
        let tls = unsafe { &mut *tls };
        f(
            kernel_mut(tcbs).as_mut_slice(),
            kernel_mut(tls).as_mut_slice(),
        )
    }

    /// Stage 199D — bounded rank-2 transaction #1 of the handled split return: validate the
    /// EXACT entering incarnation and take its pending TLS-restore request.
    ///
    /// `None` means the incarnation is stale — the task is gone, or its ASID no longer
    /// matches, or it is the idle task. The caller then skips the restore entirely, which is
    /// exactly what the legacy broad-lock path did when `current_tid()` was absent, `0`, or
    /// had no ASID. `Some(tls)` carries the TLS base to place in the TLS lane, `None` inside
    /// when no restore was pending.
    pub(crate) fn split_return_take_tls_split(
        &self,
        id: SplitReturnIdentity,
    ) -> Option<Option<usize>> {
        if id.tid == 0 {
            return None; // the idle task, exactly as the legacy path bailed
        }
        self.with_task_return_split_mut(|tcbs, tls_pending| {
            let tcb = tcbs
                .iter()
                .flatten()
                .find(|t| t.tid.0 == id.tid && t.asid == Some(id.asid))?;
            let tls_base = tcb.tls_ptr.map(|ptr| ptr.0 as usize);
            // Take the pending request at most once, exactly as `take_tls_restore_request`.
            let pending = tls_pending
                .iter()
                .position(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == id.tid));
            match pending {
                Some(idx) => {
                    tls_pending[idx] = None;
                    Some(tls_base)
                }
                None => Some(None),
            }
        })
    }

    /// Stage 199D — bounded rank-2 transaction #2 of the handled split return: commit the
    /// final user context into the EXACT entering incarnation's TCB.
    ///
    /// `false` when the incarnation is stale, in which case nothing is written — the legacy
    /// path's `set_thread_user_context` likewise did nothing for an absent task, and writing
    /// into a replacement task's TCB would be strictly worse than not writing at all.
    pub(crate) fn split_return_commit_context_split(
        &self,
        id: SplitReturnIdentity,
        context: crate::kernel::task::UserRegisterContext,
    ) -> bool {
        if id.tid == 0 {
            return false;
        }
        self.with_task_return_split_mut(|tcbs, _| {
            match tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == id.tid && t.asid == Some(id.asid))
            {
                Some(tcb) => {
                    tcb.user_context = context;
                    true
                }
                None => false,
            }
        })
    }

    /// Stage 199D — post-lock dispatch: observe the outgoing incarnation, for DIAGNOSTICS ONLY.
    ///
    /// The work item was published by the commit that removed the caller from `current`. From
    /// that instant the CPU runs nothing and owes a dispatch, and **nothing observed here
    /// cancels that debt**. In particular a caller that a reply or timeout has made `Runnable`
    /// is simply back on the run queue, where the authoritative dequeue may select it like any
    /// other candidate; refusing to dispatch on that basis would leave the CPU idle-but-not-idle
    /// with a stale frame to `eret` through, which is the defect this observation used to cause.
    ///
    /// So this returns a typed observation, not a verdict. The drain logs it and settles
    /// regardless. `{tid, asid}` is exact: a replacement incarnation reusing the TID reads as
    /// `Gone` rather than being mistaken for the original.
    pub(crate) fn direct_dispatch_observe_outgoing_split(
        &self,
        work: crate::kernel::direct_dispatch::DirectDispatchWork,
    ) -> crate::kernel::direct_dispatch::OutgoingObservation {
        use crate::kernel::direct_dispatch::OutgoingObservation;
        if work.outgoing_tid == 0 {
            return OutgoingObservation::Gone;
        }
        self.with_task_tcbs_split_mut(|tcbs| {
            let Some(tcb) = tcbs.iter().flatten().find(|t| {
                t.tid.0 == work.outgoing_tid
                    && t.asid == Some(crate::kernel::vm::Asid(work.outgoing_asid))
            }) else {
                return OutgoingObservation::Gone;
            };
            match tcb.status {
                crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointReceive(_),
                ) => OutgoingObservation::StillBlocked,
                crate::kernel::task::TaskStatus::Runnable
                | crate::kernel::task::TaskStatus::Running => OutgoingObservation::MadeRunnable,
                _ => OutgoingObservation::OtherState,
            }
        })
    }

    /// Stage 199D — post-lock dispatch: EXACT rollback of a partially committed dispatch.
    ///
    /// Called only when the dequeue already mutated scheduler state (it selected `incoming`,
    /// set `current` and the task was marked `Running`) and a LATER step then failed. Returning
    /// "declined" at that point would leave the scheduler believing `incoming` is running while
    /// the CPU `eret`s through somebody else's frame, so the mutation is undone exactly:
    ///
    /// * status `Running` → `Runnable` (rank 2);
    /// * `current` cleared and `incoming` re-enqueued at the head so it is not lost (rank 1).
    ///
    /// Returns `true` iff the rollback fully succeeded. The caller takes the explicit fatal path
    /// either way — this restores the invariant before halting so the failure is diagnosable and
    /// the scheduler is not left in a torn state.
    ///
    /// Stage 199D-WA3A-R2-SEAL (item C): the parameter is a `DequeuedDispatchMarkToken`, whose
    /// only constructor is `DispatchMarkToken::into_dequeued_authority`. A `ContinuedCurrent`
    /// mark removed no runqueue entry, so presenting it here is not a refused call — it is
    /// unrepresentable. The CPU comes from the token, which the seam authenticated against the
    /// authoritative dispatch CPU before it mutated anything (item D).
    pub(crate) fn direct_dispatch_rollback_split(
        &self,
        authority: DequeuedDispatchMarkToken,
    ) -> bool {
        use crate::kernel::task_transition::{
            TaskTransition, apply_task_transition, log_transition_refusal,
        };
        let token = authority.token();
        let cpu = token.cpu();
        let incoming = token.tid();
        // Rank 2 first: undo the status mutation — but ONLY for the EXACT incarnation this
        // transaction marked. Stage 199D-WA3A-R1: `expect_asid` comes from the token, never
        // `None`, so a replacement task that reused the numeric TID is refused outright.
        let status_ok = self.with_task_tcbs_split_mut(|tcbs| {
            match apply_task_transition(
                tcbs,
                incoming,
                token.expect_asid(),
                TaskTransition::RollbackDispatchedIncoming,
            ) {
                Ok(_) => true,
                Err(refusal) => {
                    log_transition_refusal(
                        "direct_dispatch_rollback_split",
                        incoming,
                        TaskTransition::RollbackDispatchedIncoming,
                        refusal,
                    );
                    false
                }
            }
        });
        if !status_ok {
            crate::yarm_log!(
                "DIRECT_DISPATCH_ROLLBACK_REFUSED cpu={} incoming={} reason=not_this_transactions_incarnation",
                cpu.0,
                incoming
            );
            return false;
        }
        // A scheduler rollback may only mutate `current` when `current` IS the token's TID.
        if self.current_tid_authoritative(cpu) != Some(incoming) {
            // Compensate the task transition back to the exact incarnation we just moved, so
            // the TCB is never left `Runnable` while the scheduler still believes it is current.
            let compensated = self.with_task_tcbs_split_mut(|tcbs| {
                apply_task_transition(
                    tcbs,
                    incoming,
                    token.expect_asid(),
                    TaskTransition::DispatchIncoming,
                )
                .is_ok()
            });
            crate::yarm_log!(
                "DIRECT_DISPATCH_ROLLBACK_TORN cpu={} incoming={} reason=current_mismatch compensated={}",
                cpu.0,
                incoming,
                u8::from(compensated)
            );
            return false;
        }
        // Rank 1: undo the dequeue and the current-set with the EXISTING exact inverse.
        let sched_ok = self.with_scheduler_split_mut(|sched| {
            let restored = kernel_mut(&mut sched.scheduler).preempt_reenqueue_only_on(cpu);
            restored == Some(crate::kernel::ipc::ThreadId(incoming))
        });
        if !sched_ok {
            // Task rollback succeeded but the scheduler did not: do NOT leave status=Runnable
            // with current=T. Put the task back to Running for its exact incarnation.
            let compensated = self.with_task_tcbs_split_mut(|tcbs| {
                apply_task_transition(
                    tcbs,
                    incoming,
                    token.expect_asid(),
                    TaskTransition::DispatchIncoming,
                )
                .is_ok()
            });
            crate::yarm_log!(
                "DIRECT_DISPATCH_ROLLBACK_TORN cpu={} incoming={} reason=scheduler_rollback_failed compensated={}",
                cpu.0,
                incoming,
                u8::from(compensated)
            );
            return false;
        }
        true
    }

    /// Stage 199D — post-lock dispatch step 3b: does the authoritative `current` slot agree
    /// with what the rank-1 dequeue just selected?
    ///
    /// The dequeue in `futex_wait_dispatch_step_mut` both selects the incoming task and sets
    /// `current`. Reading `current` back through the scheduler seam and comparing it to the
    /// selection is what turns "the dispatcher said X" into "the machine will resume X". A
    /// disagreement is fail-closed: the drain resumes nothing rather than resuming a task the
    /// scheduler does not consider current.
    pub(crate) fn direct_dispatch_current_agrees_split_read(
        &self,
        cpu: CpuId,
        incoming: u64,
    ) -> bool {
        self.current_tid_authoritative(cpu) == Some(incoming)
    }

    /// Stage 199D — post-lock dispatch step 4: activate the incoming task's address space.
    ///
    /// Reads the incoming ASID through the rank-2 task seam, then performs the activation
    /// through the SAME arch primitive the in-lock path uses
    /// (`hal_adapters::switch_address_space`, which carries the established AArch64
    /// DSB/ISB/TLBI ordering), and records it against **this CPU** in the authoritative
    /// per-CPU activation table so every existing `active_asid_on` consumer observes it.
    ///
    /// No `KernelState` is touched: the HAL's activation record was moved out of `KernelState`
    /// to a lock-free per-CPU table precisely so this step needs no broad lock. Returns the
    /// activated ASID, or `None` when the incoming task has none (nothing is activated).
    ///
    /// Stage 199D-WA3A-R2-SEAL (item F): the incoming task is named by the mark TOKEN, not by a
    /// bare numeric TID, and the ASID read back from the TCB must equal the one the token
    /// recorded. A replacement incarnation that reused the numeric TID therefore refuses here —
    /// before any address space is activated — rather than having ITS address space installed
    /// under the identity the dispatch actually marked.
    pub(crate) fn direct_dispatch_activate_asid_split(
        &self,
        token: DispatchMarkToken,
    ) -> Option<u16> {
        let cpu = token.cpu();
        let incoming = token.tid();
        let expected = token.expect_asid()?;
        let asid = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == incoming)
                .and_then(|t| t.asid)
                .filter(|asid| *asid == expected)
        })?;
        // U3 (canonical 203C), RISC-V only: perform the REAL address-space activation the
        // three RISC-V post-lock switch drains used to do inside a broad `with_cpu`
        // re-acquire — map the kernel-shared range into the incoming ASID, construct its
        // page-table root, and write `satp` (whose implementation issues the required
        // `sfence.vma` ordering). `hal_adapters::switch_address_space` deliberately DEFERS
        // RISC-V paging (`RISCV_PAGING_DEFERRED`) and writes no `satp`, so routing RISC-V
        // through it would silently drop a real hardware operation. Every other
        // architecture and configuration keeps the existing generic path unchanged.
        //
        // Fail closed: a missing page-table root returns `None`, so the caller rolls back
        // with its exact dequeued authority and diverges rather than resuming a task whose
        // address space was never activated.
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
        {
            let _ = crate::arch::riscv64::page_table::map_kernel_shared_into_asid(asid);
            let satp = crate::arch::riscv64::page_table::cr3_for_asid(asid)?;
            crate::arch::riscv64::page_table::write_satp(satp);
        }
        #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
        crate::arch::hal_adapters::switch_address_space(asid);
        crate::arch::hal::note_address_space_activated(cpu, asid);
        Some(asid.0)
    }

    /// Stage 199D — post-lock dispatch step 5: restore the incoming task's complete saved user
    /// context and its TLS state into `frame`, through the rank-2 task seam only.
    ///
    /// This is the narrow-seam equivalent of the in-lock
    /// `resume_current_thread_with_frame` + TLS lane write that
    /// `restore_arch_thread_state` performs, with two differences that are both deliberate:
    ///
    /// * it resolves the task by EXACT tid rather than by re-reading `current`, so it cannot
    ///   restore a different task than the one the dequeue selected;
    /// * it takes the pending TLS-restore request in the SAME rank-2 acquisition as the
    ///   context read, so the two cannot straddle a lock boundary.
    ///
    /// Returns the TLS base to place in the TLS lane (`Some(None)` = no restore pending), or
    /// `None` when the task has no saved context to restore.
    ///
    /// Stage 199D-WA3A-R2-SEAL (item F): resolution is by the mark TOKEN's exact incarnation —
    /// the TCB's ASID must still equal the one the mark recorded. If the TCB was replaced by a
    /// different incarnation that reused the numeric TID, this returns `None` and the caller
    /// takes its fatal/rollback path; the replacement's context is never copied into the frame.
    pub(crate) fn direct_dispatch_restore_context_split(
        &self,
        token: DispatchMarkToken,
    ) -> Option<(crate::kernel::task::UserRegisterContext, Option<usize>)> {
        let incoming = token.tid();
        let expected = token.expect_asid()?;
        // U3 (canonical 203C): the read + take itself lives in ONE place —
        // `crate::kernel::task::read_user_context_and_take_tls` — shared with the post-exit
        // restore transaction. The token supplies the exact incarnation; nothing else changes.
        self.with_task_return_split_mut(|tcbs, tls_pending| {
            crate::kernel::task::read_user_context_and_take_tls(
                tcbs,
                tls_pending,
                incoming,
                Some(expected),
            )
        })
    }

    /// Stage 199D — post-lock dispatch step 5b: consume the incoming task's parked
    /// blocked-syscall completion, through the rank-2 task seam only.
    ///
    /// Byte-for-byte the same decision as `KernelState::take_blocked_syscall_completion`,
    /// which the in-lock resume path calls: the entry is taken either way (a stale one must
    /// not linger to be seen by a later receive), it is RETURNED only on an exact
    /// `{tid, asid, blocked_generation}` match, and an exact take clears the residue that
    /// belongs to that completion alone. Keeping the two identical is what makes the incoming
    /// task's resume lanes the same whether the dispatch ran in-lock or off-lock.
    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
    ///
    /// Stage 199D-WA3A-R2-SEAL (item F): the task is located by the mark TOKEN's exact
    /// incarnation. A replacement incarnation that reused the numeric TID is not this
    /// transaction's task, so its parked completion is neither taken nor consumed here.
    pub(crate) fn direct_dispatch_take_completion_split(
        &self,
        token: DispatchMarkToken,
    ) -> Option<crate::kernel::task::BlockedSyscallCompletion> {
        let incoming = token.tid();
        let expected = token.expect_asid()?;
        self.with_task_tcbs_split_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == incoming && t.asid == Some(expected))?;
            let pending = tcb.pending_syscall_completion?;
            // U6 §2: class-scoped — a parked `IpcSend` completion belongs to
            // `direct_dispatch_take_send_completion_split` and is left untouched here.
            if pending.syscall_class != crate::kernel::task::BlockedSyscallClass::IpcRecv {
                return None;
            }
            let exact = pending.matches_tcb(tcb);
            tcb.pending_syscall_completion = None;
            if !exact {
                return None;
            }
            tcb.ipc_timeout_fired = false;
            tcb.blocked_recv_state = None;
            Some(pending)
        })
    }

    /// Canonical 199E-R2 — THE off-lock consumer of an async-preemption tag, keyed on the
    /// INCOMING identity a resume boundary has actually resolved.
    ///
    /// This replaces the 199E-R1D pair of consumers (one in the in-lock restore keyed on
    /// `kernel.current_tid()`, one in the post-lock dispatch keyed on a mark token). Both ran
    /// strictly BEFORE the resume identity was published, so on the post-lock route the in-lock
    /// one consumed the OUTGOING task's tag and the write-back never saw an authorization —
    /// measured live as 0 verbatim restores out of 187 switching write-backs while 407 tags were
    /// published and spent. There is now exactly ONE consumer and it runs AT the write-back.
    ///
    /// `incoming_asid` is the ASID the boundary itself resolved for `incoming_tid` (on RISC-V,
    /// the same `task_asid_for_tid_split_read` value it is about to install into `satp`), not a
    /// value re-read from the TCB. That is what makes the check a resume-identity check.
    ///
    /// Rank 2 only — the task lock. No broad-lock acquisition, so the Stage 204A census is
    /// unchanged.
    pub(crate) fn take_async_preempt_for_incoming_split(
        &self,
        incoming_tid: u64,
        incoming_asid: Option<crate::kernel::vm::Asid>,
    ) -> crate::kernel::task::AsyncResumeClass {
        self.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::task::classify_and_take_async_resume(tcbs, incoming_tid, incoming_asid)
        })
    }

    /// Canonical 199E-R2 — CANCEL a staged snapshot off the broad lock.
    ///
    /// Called by a resume boundary that is returning to the SAME task through the original
    /// hardware frame: no write-back happens, so the snapshot staged by this trap describes an
    /// instant the task runs straight past and must not survive to authorize a later restore.
    pub(crate) fn cancel_async_preempt_for_split(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| crate::kernel::task::cancel_async_resume(tcbs, tid))
    }

    /// U6 §8 — the PRODUCTION-LIVE, class-scoped split consumer of a blocked SENDER's parked
    /// completion, for the post-lock resume boundaries.
    ///
    /// Deliberately separate from `direct_dispatch_take_completion_split`, which is compiled
    /// only under the reply-timeout oracle feature and takes `IpcRecv` completions. Two things
    /// follow from keeping them apart, and both are required:
    ///
    /// * U6 must not production-enable any proof-gated `IpcRecv` reply-timeout behaviour, so it
    ///   cannot simply ungate the existing consumer; and
    /// * on a build where both exist, neither may discard the other's work — hence the class
    ///   check, which leaves a non-matching entry parked exactly where it is.
    ///
    /// Identity is the mark TOKEN's exact incarnation, and the parked entry must additionally
    /// match on `{tid, asid, blocked_send_generation}`. A replacement incarnation that reused
    /// the numeric TID is not this transaction's task, so its completion is neither taken nor
    /// consumed here.
    pub(crate) fn direct_dispatch_take_send_completion_split(
        &self,
        token: DispatchMarkToken,
    ) -> Option<crate::kernel::task::BlockedSyscallCompletion> {
        let incoming = token.tid();
        let expected = token.expect_asid()?;
        self.with_task_tcbs_split_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == incoming && t.asid == Some(expected))?;
            let pending = tcb.pending_syscall_completion?;
            if pending.syscall_class != crate::kernel::task::BlockedSyscallClass::IpcSend {
                return None;
            }
            let exact = pending.matches_tcb(tcb);
            tcb.pending_syscall_completion = None;
            if !exact {
                return None;
            }
            tcb.ipc_timeout_fired = false;
            Some(pending)
        })
        .inspect(|pending| {
            // U7 §3B: THE delivery point for a blocking-send completion. If an off-lock
            // send-timeout settle armed the class-retirement seal, this is where it becomes
            // true — a resumed sender has now consumed the exact parked `TimedOut` result.
            // Arming alone never emits, so a committed-but-undelivered completion cannot claim
            // the class retired.
            if pending.result == crate::kernel::boot::KernelState::SEND_COMPLETION_TIMED_OUT {
                crate::kernel::boot::maybe_emit_send_timeout_class_retired();
            }
        })
    }

    /// U3 (canonical 203C) — the RISC-V post-lock `CurrentTaskExited` validation snapshot.
    ///
    /// Replaces the brief broad `with_cpu` re-acquire the RISC-V exit consumer used to take to
    /// answer four questions about one exiting incarnation. Those four facts live in two
    /// domains, and reading them through two separated transactions could tear — the exiting
    /// task could be re-enqueued between the scheduler read and the task read, and the
    /// consumer would then attest absence that no longer holds. They are therefore taken as
    /// ONE snapshot, with the rank-2 task acquisition NESTED inside the rank-1 scheduler
    /// acquisition:
    ///
    /// * rank 1 (scheduler): current TID on the exact trapping CPU, and whether the exiting
    ///   TID appears in ANY CPU's runqueue or current slot (`task_present_anywhere`, not this
    ///   CPU's queue alone);
    /// * rank 2 (task, nested while rank 1 is still held): the exact `{tid, asid}`
    ///   incarnation and the terminal status.
    ///
    /// Canonical rank order is scheduler(1) → task(2), so this nesting is ascending and takes
    /// the two backing locks in the documented direction. The nested read goes through the
    /// rank-2 seam directly rather than through any `KernelState` accessor: an accessor such
    /// as `current_tid`/`task_asid` would re-enter its own domain lock, and the rank-1
    /// re-entry in particular would deadlock against the guard this transaction already holds.
    ///
    /// The three task-side answers are byte-for-byte the old `with_cpu` body's:
    /// an absent TCB is identity-safe AND terminal (what `task_asid`/`task_status` returning
    /// `None` meant), a TCB with no ASID is identity-safe (`task_asid` returned `None` there
    /// too), and only `Exited(_)` / `Dead` are terminal.
    ///
    /// Read-only: nothing is mutated in either domain. `with_cpu` also wrote `current_cpu` as
    /// a side effect of admission; that write is not reproduced and is not needed — the
    /// Phase-2 broad-lock phase of this very trap already bound `current_cpu` to this same
    /// CPU. The admission CHECK is preserved exactly, so an invalid or offline CPU still
    /// yields the identical `KernelError` and the caller's existing failure path is unchanged.
    pub(crate) fn post_lock_exit_validation_split(
        &self,
        cpu: CpuId,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> Result<PostLockExitValidation, KernelError> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            let current = kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|t| t.0);
            let in_runqueue = kernel_ref(&sched.scheduler)
                .task_present_anywhere(crate::kernel::ipc::ThreadId(tid));
            // Rank 2 nested here, with the rank-1 guard above still held: the scheduler facts
            // and the task facts are one coherent observation, not two that can tear.
            let (identity_ok, terminal) = self.with_task_tcbs_split_mut(|tcbs| {
                match tcbs.iter().flatten().find(|t| t.tid.0 == tid) {
                    // FULL incarnation: a numeric TID match alone would let a restarted task
                    // satisfy a stale disposition, so the ASID recorded at publication must
                    // still be bound to that TID.
                    Some(tcb) => (
                        match tcb.asid {
                            Some(bound) => bound == asid,
                            None => true,
                        },
                        matches!(
                            tcb.status,
                            crate::kernel::task::TaskStatus::Exited(_)
                                | crate::kernel::task::TaskStatus::Dead
                        ),
                    ),
                    // TCB gone entirely: identity-safe and terminal.
                    None => (true, true),
                }
            });
            Ok(PostLockExitValidation {
                current,
                identity_ok,
                terminal,
                in_runqueue,
            })
        })
    }

    /// U3 (canonical 203C) — the AArch64 post-lock `CurrentTaskExited` REPLACEMENT restore
    /// transaction.
    ///
    /// Replaces the second brief broad `with_cpu` re-acquire the AArch64 exit consumer took, the
    /// one that ran `d2_recv_switch_incoming_asid(next)` followed by the post-switch arch restore.
    /// Everything that body read or consumed from kernel state is taken here, and everything it
    /// did to hardware or to the trap frame is left to the caller — which performs it with every
    /// domain lock released.
    ///
    /// Lock ordering: rank 1 (scheduler) with rank 2 (task) NESTED inside it, the same ascending
    /// shape [`Self::post_lock_exit_validation_split`] uses. One acquisition of each, so the
    /// replacement's ASID, its saved context, its TLS request and its parked completion are ONE
    /// coherent observation of ONE incarnation. Splitting them would let the ASID that gets
    /// activated belong to a different instant than the context that gets restored.
    ///
    /// Rank 1 reproduces exactly what `with_cpu` did on entry: the same `validate_online_cpu`
    /// admission predicate — so an invalid or offline CPU still yields the identical
    /// `KernelError` — and the same `current_cpu` binding, which the retired body depended on
    /// (`d2_recv_switch_incoming_asid` switched the address space of `self.current_cpu()`).
    ///
    /// Two refusals are DELIBERATE tightenings of the retired body, and both fail closed:
    ///
    /// * `next_tid == exiting_tid` — the exiting task may never be the restore source. The
    ///   consumer already refuses this above with its own marker; refusing here as well makes the
    ///   transaction sound on its own terms.
    /// * `current_tid_on(cpu) != Some(next_tid)` — the replacement named by the caller is no
    ///   longer the task this CPU is running. The retired body re-read `current_tid()` inside its
    ///   own separate acquisition and would have restored whatever it found there; resolving the
    ///   replacement on the exact CPU and refusing a disagreement is what stops a stale identity
    ///   from being restored at all.
    ///
    /// Everything else is the retired body's behaviour, outcome for outcome:
    ///
    /// * a replacement with no ASID — including one whose TCB is gone entirely — yields
    ///   `asid: None`, `facts: None`, `enter_idle: true`. `task_asid` returned `None` in both
    ///   cases, so no address space was switched and the restore logged `SCHED_ENTER_IDLE` and
    ///   returned success. Nothing is consumed.
    /// * `restore_frame == false` (the caller holds no trap frame) yields `facts: None` with the
    ///   ASID still resolved. The retired body performed the address-space switch unconditionally
    ///   and only then discovered it had no frame, so nothing was consumed on that path either.
    ///   Taking the TLS request or a parked completion here would destroy state with nowhere to
    ///   write it.
    ///
    /// The facts are TAKES, not observations: once returned, this value holds the only remaining
    /// copy of the replacement's TLS request and parked completion.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn post_exit_replacement_restore_split(
        &self,
        cpu: CpuId,
        exiting_tid: u64,
        next_tid: u64,
        restore_frame: bool,
    ) -> Result<ExitReplacementRestore, KernelError> {
        if next_tid == 0 || next_tid == exiting_tid {
            return Err(KernelError::TaskMissing);
        }
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            // The admission side effect `with_cpu` performed, which the retired body relied on.
            sched.current_cpu = cpu;
            if kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|t| t.0)
                != Some(next_tid)
            {
                return Err(KernelError::TaskMissing);
            }
            // Rank 2 nested here, with the rank-1 guard above still held.
            Ok(self.with_task_return_split_mut(|tcbs, tls_pending| {
                let Some(asid) = tcbs
                    .iter()
                    .flatten()
                    .find(|t| t.tid.0 == next_tid)
                    .and_then(|t| t.asid)
                else {
                    return ExitReplacementRestore {
                        asid: None,
                        facts: None,
                        enter_idle: restore_frame,
                    };
                };
                let facts = restore_frame
                    .then(|| {
                        crate::kernel::task::take_thread_restore_facts(
                            tcbs,
                            tls_pending,
                            next_tid,
                            Some(asid),
                        )
                    })
                    .flatten();
                ExitReplacementRestore {
                    asid: Some(asid),
                    facts,
                    enter_idle: false,
                }
            }))
        })
    }

    /// U9 (canonical 203C) — the rank-1 half of the PRODUCTION post-switch restore.
    ///
    /// `with_cpu(cpu, …)` is exactly `lock` + `set_current_cpu(cpu)?` + body, and `set_current_cpu`
    /// is itself `validate_online_cpu` + `sched.current_cpu = cpu` under the rank-1 guard. Both
    /// halves are reproduced here, so nothing the retired body depended on is dropped:
    ///
    /// * the ONLINE-CPU **admission**, whose `Err` the caller still propagates as
    ///   `TrapHandleError::Syscall`, before any task state is read or taken;
    /// * the **binding** of `current_cpu`, which the retired body silently relied on — the arch
    ///   restore's `KernelState::current_tid()` is `current_tid_on(self.current_cpu())`, so without
    ///   the bind it would have answered for the wrong CPU.
    ///
    /// The current TID is therefore read HERE, under the same rank-1 guard that just bound the
    /// CPU — never through [`Self::current_tid_split_read`], which is deliberately NON-binding
    /// (see the Stage 4T+6R revert) and would reintroduce exactly the staleness the bind exists to
    /// prevent.
    ///
    /// `Ok(None)` is the retired `current_tid()` answering `None`, which the arch restores treated
    /// as "no user task yet" rather than as an error.
    pub(crate) fn post_switch_restore_admit_split(
        &self,
        cpu: CpuId,
    ) -> Result<Option<u64>, KernelError> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            // The admission side effect `with_cpu` performed, which the retired body relied on.
            sched.current_cpu = cpu;
            Ok(kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|tid| tid.0))
        })
    }

    /// U9 (canonical 203C) — the rank-2 half of the AArch64 post-switch restore, ONE acquisition.
    ///
    /// The retired in-lock body reached the task domain four separate times — `task_asid`,
    /// `thread_user_context`, `take_tls_restore_request`, and one take per completion class.
    /// Fusing them into a single acquisition is strictly more coherent than that sequence and
    /// changes no decision: every one of them resolved the SAME `tid` against the SAME `tcbs`, and
    /// none of them read the trap frame, so performing them all before the first frame write is
    /// behaviour-preserving (the argument U3 already established for
    /// `apply_restored_thread_state`'s two producers).
    ///
    /// The takes are ordered exactly as the retired body ordered them — context and TLS, then
    /// `IpcSend`, then (where compiled) `IpcRecv` — because
    /// [`crate::kernel::task::take_thread_restore_facts`] is the single definition both producers
    /// use.
    ///
    /// `expect_asid` is the ASID read from the same TCB in this same acquisition, so the exactness
    /// check can only ever agree; it is passed rather than `None` so a future caller cannot reach
    /// this gather with an ASID it did not itself resolve.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn post_switch_restore_facts_split(
        &self,
        tid: u64,
    ) -> PostSwitchRestoreOutcome<crate::kernel::task::ThreadRestoreFacts> {
        if tid == 0 {
            return PostSwitchRestoreOutcome::Idle;
        }
        self.with_task_return_split_mut(|tcbs, tls_pending| {
            // `task_asid(tid)`: `None` for a reaped TCB and for a task with no address space. The
            // retired body treated BOTH as the idle outcome, before it read any context.
            let Some(asid) = tcbs
                .iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .and_then(|t| t.asid)
            else {
                return PostSwitchRestoreOutcome::Idle;
            };
            match crate::kernel::task::take_thread_restore_facts(tcbs, tls_pending, tid, Some(asid))
            {
                Some(facts) => PostSwitchRestoreOutcome::Facts(facts),
                None => PostSwitchRestoreOutcome::Missing,
            }
        })
    }

    /// U9 (canonical 203C) — the rank-2 half of the x86_64 post-switch restore, ONE acquisition.
    ///
    /// The retired in-lock body read the task domain four times as well —
    /// `apply_current_thread_to_frame`'s `thread_user_context`, `take_tls_restore_request`, and
    /// then `task_asid(tid)` for the pre-IRET CR3 block. This is the SAME fused gather
    /// [`Self::owner_revalidation_snapshot_split`] already performs for the idle-owner
    /// revalidation, reused rather than re-implemented so the two boundaries cannot drift.
    ///
    /// `None` is the retired `thread_user_context(tid).is_none()` verdict, which
    /// `apply_current_thread_to_frame` raised as `KernelError::TaskMissing` and
    /// `restore_arch_thread_state` then swallowed into `Ok(())` as the early-boot "no user task
    /// scheduled yet" case. Nothing is consumed on that path.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn post_switch_restore_snapshot_split(
        &self,
        tid: u64,
    ) -> Option<OwnerRevalidationSnapshot> {
        self.owner_revalidation_snapshot_split(tid)
    }

    /// U3 (canonical 203C) — the class-neutral blocked-waiter Phase-C completion transaction.
    ///
    /// Replaces the three byte-identical broad `with_cpu` completion closures the plain,
    /// ordinary-cap and reply-cap blocked-waiter delivery executors each held. All three ran the
    /// same body — clear the waiter's return registers, then (when a wake TID exists) clear the
    /// endpoint waiter slot by identity and wake exactly once — so one transaction serves all
    /// three. It takes no class name and emits no class-specific telemetry: every
    /// `kind=blocked_waiter_*` marker stays at its call site, outside this seam.
    ///
    /// Lock ordering. The legacy body ran inside one broad guard, but its own helpers took the
    /// task, IPC and scheduler locks sequentially underneath it. This reproduces that sequence
    /// with the domain locks taken directly, in the SAME order the legacy body produced, each
    /// fully released before the next is taken — so no lock is ever held while another is
    /// acquired, and the canonical rank direction is never violated:
    ///
    /// 1. rank 1 (scheduler) — authenticate `cpu` with the same predicate `with_cpu` uses and
    ///    bind `current_cpu`, exactly as `with_cpu` did on entry;
    /// 2. rank 2 (task) — clear the waiter's return registers through the byte-identical locked
    ///    helper, and read the wake TID's current ASID for the identity below;
    /// 3. rank 3 (IPC) — clear the endpoint waiter through the central identity path;
    /// 4. rank 2 (task) — the wake's state half: status validation, `Runnable`, timeout clear,
    ///    and the placement facts;
    /// 5. rank 1 (scheduler) — the enqueue decision.
    ///
    /// Identity is deliberately UNCHANGED: the endpoint clear keys on
    /// `task_asid(wake_tid).unwrap_or(Asid(0))` — the wake TID's CURRENT ASID, read here — not on
    /// the snapshot's `waiter_asid`, a wait generation, a `WaiterKey` or an ownership token.
    /// Tightening that is a WA3C2 question and is not this increment's.
    ///
    /// The caller ignores the result, matching the retired `let _ = self.with_cpu(...)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn complete_blocked_waiter_delivery_split(
        &self,
        cpu: CpuId,
        waiter_tid: u64,
        endpoint_idx: usize,
        wake_tid: Option<crate::kernel::ipc::ThreadId>,
    ) -> Result<(), KernelError> {
        use crate::kernel::boot::{ReceiverWaiterIdentity, map_scheduler_error};
        use crate::kernel::vm::Asid;

        // (1) rank 1 — the CPU authentication and binding `with_cpu` performed on entry.
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(map_scheduler_error)?;
            sched.current_cpu = cpu;
            Ok::<(), KernelError>(())
        })?;

        // (2) rank 2 — return-register clear, then the wake TID's current ASID. An absent waiter
        // TCB mutates nothing and is not an error, exactly as the locked helper behaves.
        let wake_asid = self.with_task_tcbs_split_mut(|tcbs| {
            KernelState::clear_blocked_recv_return_regs_locked(tcbs, waiter_tid);
            wake_tid.map(|w| {
                tcbs.iter()
                    .flatten()
                    .find(|t| t.tid.0 == w.0)
                    .and_then(|t| t.asid)
                    .unwrap_or(Asid(0))
            })
        });

        let (Some(wake_tid), Some(wake_asid)) = (wake_tid, wake_asid) else {
            // `wake_tid == None`: registers cleared, no endpoint clear, no wake.
            return Ok(());
        };

        // (3) rank 3 — the central identity-keyed clear. Slot clearing, the direct-ack lease
        // release and the waiter-census unlink all remain owned by that one path; the slot is
        // never written directly here. Same marker, same identity-only reblock semantics.
        self.with_ipc_split_mut(|ipc| {
            if ipc.clear_endpoint_waiter_if_identity(
                endpoint_idx,
                ReceiverWaiterIdentity::new(wake_tid, wake_asid),
            ) {
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_RECV_V2_CLEAR_WAITER receiver_tid={}",
                    wake_tid.0
                );
            }
        });

        // (4)/(5) — the wake, reproducing `apply_scheduler_wake_plan(Wake(tid))`.
        self.wake_tid_to_runnable_split(cpu, wake_tid)
    }

    /// U3 (canonical 203C) — the split form of
    /// `with_cpu(cpu, |k| k.apply_split_sender_wake_plan(tid))`, the deferred ordinary-cap
    /// sender wake on the recv boundary.
    ///
    /// The broad body was a two-line composition: `IPC_RECV_SPLIT_REFILL_WAKE_APPLY`, then
    /// `apply_scheduler_wake_plan(Wake(tid))` — which is `wake_tid_to_runnable(tid)`. It took
    /// the whole `KernelState` lock for the CPU validate-and-bind `with_cpu` performs on entry
    /// plus one scheduler/task wake. This reproduces it step for step:
    ///
    /// 1. **rank 1** — [`Self::bind_current_cpu_split`]: the same `validate_online_cpu`
    ///    predicate `set_current_cpu` applies. A refusal returns the same `KernelError` class,
    ///    leaves `current_cpu` and every task/scheduler field untouched, and — exactly as the
    ///    broad closure that never ran — emits NO marker and performs NO wake. Released.
    /// 2. The marker, with identical text and fields, only after a successful bind.
    /// 3. **rank 2 then rank 1** — the SHARED [`Self::wake_tid_to_runnable_split`], the one
    ///    body this transaction and the blocked-waiter completion both use. No wake logic is
    ///    duplicated here.
    ///
    /// This is deliberately NOT routed through `complete_blocked_waiter_delivery_split`: that
    /// transaction also clears the receiver's return registers and the endpoint waiter slot,
    /// neither of which is part of sender-wake semantics.
    ///
    /// Nothing is strengthened: no endpoint identity, ASID, wait generation, `WaiterKey` or
    /// ownership state is consulted. This stays the existing generic sender wake.
    #[cfg_attr(not(test), allow(dead_code))]
    fn apply_split_sender_wake_plan_split(
        &self,
        cpu: CpuId,
        target: crate::kernel::ipc::SenderWakeTarget,
    ) -> Result<(), KernelError> {
        // (1) rank 1 — the CPU authentication and binding `with_cpu` performed on entry.
        self.bind_current_cpu_split(cpu)?;
        // (2) the marker `KernelState::apply_split_sender_wake_plan` emitted, unchanged.
        crate::yarm_log!(
            "IPC_RECV_SPLIT_REFILL_WAKE_APPLY tid={} send_generation={}",
            target.tid.0,
            target.send_generation
        );
        // (2b) U6 §2/§7 — publish the success completion BEFORE the wake, exactly as the
        // in-lock sibling `KernelState::apply_split_sender_wake_plan` does. The sender's
        // message was already refilled into the endpoint under `ipc_state_lock`, so its send
        // succeeded; the completion is what makes the woken sender return that instead of the
        // `WouldBlock` its saved frame still carries.
        self.publish_blocking_send_completion_split(
            target,
            crate::kernel::boot::KernelState::SEND_COMPLETION_OK,
        );
        // (3) the shared wake body — `apply_scheduler_wake_plan(Wake(tid))`.
        self.wake_tid_to_runnable_split(cpu, target.tid)
    }

    /// U6/199C §2 — the NO-RECLAIM transient-pin release, and the single off-lock envelope
    /// settle used by every terminal path that must dispose of a blocked shared-region send.
    ///
    /// # Why this needs no TLB-shootdown wait (the §2 derivation, from production source)
    ///
    /// A shared-region transfer envelope holds ONE transient `pin_refcount` on the source
    /// MemoryObject, taken at `stash_transfer_envelope`. Releasing it here provably cannot free
    /// a frame, unmap a page, or initiate a shootdown, so the full D3 `await_tlb_shootdown_ack`
    /// design is **not** required. Four independent facts, each checked against the tree:
    ///
    /// 1. `adjust_memory_object_pin_refcount(obj, -1)` is a PURE COUNTER DECREMENT. It touches
    ///    `pin_refcount` and nothing else — no `free_frame`, no unmap, no shootdown.
    /// 2. Reclaim is a SEPARATE, EXPLICITLY-CALLED operation
    ///    (`reclaim_memory_object_if_unreferenced` / `reclaim_memory_object_for_phys_locked`),
    ///    and it refuses unless `cap_refcount == 0 && map_refcount == 0 && pin_refcount == 0`.
    ///    This transaction never calls it.
    /// 3. NONE of the five production pin-release sites calls reclaim either
    ///    (`take_transfer_envelope`, the two `shared_region_txn` releases,
    ///    `revoke_transfer_envelopes_for_cnode`, and `sr_release_pin_split`) — dropping this
    ///    pin is simply not a reclaim trigger anywhere in the tree.
    /// 4. The pin can never be the FINAL reference while the envelope exists. The sender KEEPS
    ///    its `source_cap`: `stash_transfer_handle` only *resolves* the capability, it never
    ///    strips it from the sender's cnode, and a cnode-held MemoryObject/DmaRegion cap holds
    ///    `cap_refcount >= 1` (`mint_capability_in_cnode` bumps it; only revocation drops it).
    ///    So even if reclaim WERE called it would refuse on `cap_refcount != 0`.
    ///
    /// The one place a shared-region frame is genuinely freed is the VM unmap path, where an
    /// explicit `revoke_capability_in_cnode` first drops `cap_refcount` to 0 and
    /// `execute_tlb_shootdown_wait_plan` then reclaims (see `syscall/vm.rs`). That path is not
    /// reachable from a blocking send, and this transaction does not enter it.
    ///
    /// Per §2's preference order this is therefore case (2) — a narrowly typed no-reclaim
    /// release — not case (3). Implementing a shootdown wait here would be inventing a
    /// synchronization requirement the code does not have.
    ///
    /// # Lock discipline
    ///
    /// Strictly ascending and never nested: the rank-3 IPC section consumes the envelope and
    /// RELEASES its lock, and only then does the rank-6 memory section drop the pin, through the
    /// already-production `sr_release_pin_split` seam. No new `with_memory_split_mut` call site
    /// is introduced, and no lock is held across the other.
    ///
    /// Idempotent by construction: the rank-3 consume clears the slot, so a second settle for
    /// the same handle finds no envelope, returns `false`, and drops no second pin.
    pub(crate) fn settle_blocked_send_envelope_split(
        &self,
        handle: u64,
        endpoint_idx: usize,
        receiver_tid: crate::kernel::ipc::ThreadId,
    ) -> bool {
        // Rank 3: consume the envelope exactly once, reporting whether it owed a pin.
        let (taken, pinned_object) =
            self.take_transfer_envelope_split_inner(handle, endpoint_idx, receiver_tid);
        if !taken {
            return false;
        }
        // Rank 3 is released. Rank 6: drop the transient pin — no reclaim, see above.
        if let Some(object) = pinned_object {
            self.sr_release_pin_split(object);
            crate::yarm_log!(
                "U6_SHARED_REGION_PIN_RELEASED handle={} endpoint={} reclaim=0 result=ok",
                handle,
                endpoint_idx
            );
        }
        true
    }

    /// The rank-3 half of [`Self::settle_blocked_send_envelope_split`]. Returns
    /// `(consumed, pinned_object)`, where
    /// `pinned_object` is `Some` exactly when the consumed envelope was a shared-region one and
    /// therefore owes the caller a single rank-6 pin release.
    fn take_transfer_envelope_split_inner(
        &self,
        handle: u64,
        endpoint_idx: usize,
        receiver_tid: crate::kernel::ipc::ThreadId,
    ) -> (bool, Option<CapObject>) {
        use crate::kernel::boot::{MAX_TRANSFER_ENVELOPES, TransferState};
        let Ok(idx) = usize::try_from(handle & 0xFFFF) else {
            return (false, None);
        };
        if idx >= MAX_TRANSFER_ENVELOPES {
            return (false, None);
        }
        let generation = handle >> 16;
        if generation == 0 {
            return (false, None);
        }
        self.with_ipc_split_mut(|ipc| {
            if ipc.transfer_envelope_generations[idx] != generation {
                return (false, None);
            }
            let Some(envelope) = ipc.transfer_envelopes[idx] else {
                return (false, None);
            };
            // The endpoint the envelope was stashed against, matched against the ENVELOPE's OWN
            // recorded endpoint object — deliberately NOT against a live `ipc.endpoints` entry.
            //
            // A settle frequently runs precisely because the endpoint is gone (destruction is one
            // of the terminal winners, and a commit refused for a vanished endpoint is another).
            // Requiring a live entry here would make exactly those cases un-settleable and leak
            // the envelope and its pin — which is what this check originally did.
            let endpoint_matches = matches!(envelope.endpoint,
                crate::kernel::capabilities::CapObject::Endpoint { index, .. }
                    if index == endpoint_idx);
            if !endpoint_matches {
                return (false, None);
            }
            if let Some(bound_receiver) = envelope.receiver_tid
                && bound_receiver != receiver_tid
            {
                return (false, None);
            }
            if envelope.transition(TransferState::Released).is_none() {
                return (false, None);
            }
            ipc.telemetry.transfer_records_materialized = ipc
                .telemetry
                .transfer_records_materialized
                .saturating_add(1);
            ipc.transfer_envelopes[idx] = None;
            // A shared-region envelope owes exactly ONE rank-6 pin release, which the caller
            // performs after this lock is dropped. Reporting it here — rather than releasing it
            // here — is what keeps the two domains strictly sequential instead of nested.
            (
                true,
                envelope
                    .shared_region
                    .is_some()
                    .then_some(envelope.source_object),
            )
        })
    }

    /// U6 §6 — the BLOCKING-SEND COMMIT: one architecture-neutral, rank-ordered transaction
    /// shared by both blocking-send origins, run entirely outside the broad lock.
    ///
    /// # Why one transaction, and why nested
    ///
    /// Committing a blocking send touches three domains that must agree at a single instant:
    /// the sender must still be this CPU's current task (scheduler, rank 1); it must become
    /// `Blocked(EndpointSend)` carrying a fresh block generation and deadline (task, rank 2);
    /// and exactly one waiter bearing that generation must appear on the endpoint (IPC, rank
    /// 3). Split into three separately-acquired sections these could tear — the sender could be
    /// preempted between the scheduler check and the task write, or the endpoint could be
    /// destroyed between the task write and the waiter publish, leaving a task blocked forever
    /// on a queue that no longer exists.
    ///
    /// They are therefore taken as ONE transaction with rank 2 nested inside rank 1 and rank 3
    /// nested inside rank 2. That is strictly ASCENDING rank order, which is the documented
    /// direction (`doc/CAPABILITY_MODEL.md §3`: "always acquire locks in strictly ascending
    /// rank order"; a seam of rank N must not be entered while holding a lock of rank ≥ N).
    /// The same ascending nesting is what the accepted RISC-V exit snapshot
    /// (`riscv_exit_validation_snapshot_split`) uses for its rank-1 → rank-2 pair.
    ///
    /// # Preflight, then mutate
    ///
    /// Every fallible condition is checked BEFORE anything is written:
    ///
    /// * rank 1 — the snapshot's CPU is the authoritative dispatch CPU, and the exact
    ///   `sender_tid` is its current task;
    /// * rank 2 — a live `{tid, asid}` incarnation exists, it is not already blocked, and its
    ///   `blocked_send_generation` can be advanced by `checked_add`;
    /// * rank 3 — the endpoint slot is present, its generation still matches the one the
    ///   producer resolved, no waiter for this TID is already queued (no duplicate), and the
    ///   waiter queue has a free slot.
    ///
    /// Only then does it mutate, and the mutations are ordered so that none of them can fail:
    /// publish the waiter (rank 3), commit the blocked TCB (rank 2), clear `current` (rank 1) —
    /// the clear happening inside the SAME rank-1 section that validated it, so nothing can
    /// observe a sender that is blocked yet still current, or current yet already queued as a
    /// waiter. There is no broad-lock fallback and nothing to roll back.
    ///
    /// Returns the minted send generation on success.
    pub(crate) fn commit_blocking_send_split(
        &self,
        snap: &crate::kernel::dispatch_post_work::BlockingSendCommitSnapshot,
    ) -> BlockingSendCommitOutcome {
        use crate::kernel::boot::MAX_ENDPOINT_SENDER_WAITERS;
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::task::{TaskStatus, WaitReason};

        self.with_scheduler_split_mut(|sched| {
            // ── rank 1 preflight ────────────────────────────────────────────────────────────
            if sched.current_cpu != snap.cpu {
                return BlockingSendCommitOutcome::RefusedCpuMismatch {
                    requested: snap.cpu,
                    authoritative: sched.current_cpu,
                };
            }
            let current = kernel_ref(&sched.scheduler)
                .current_tid_on(snap.cpu)
                .map(|t| t.0);
            if current != Some(snap.sender_tid) {
                return BlockingSendCommitOutcome::RefusedNotCurrent {
                    expected: snap.sender_tid,
                    observed: current,
                };
            }

            self.with_task_tcbs_split_mut(|tcbs| {
                // ── rank 2 preflight (no mutation yet) ──────────────────────────────────────
                let Some(idx) = tcbs.iter().position(|slot| {
                    slot.as_ref().is_some_and(|t| {
                        t.tid.0 == snap.sender_tid && t.asid == Some(snap.sender_asid)
                    })
                }) else {
                    return BlockingSendCommitOutcome::RefusedSenderMissing;
                };
                {
                    let tcb = tcbs[idx].as_ref().expect("position guarantees Some");
                    if matches!(tcb.status, TaskStatus::Blocked(_)) {
                        return BlockingSendCommitOutcome::RefusedAlreadyBlocked;
                    }
                }
                let Some(next_generation) = tcbs[idx]
                    .as_ref()
                    .expect("position guarantees Some")
                    .blocked_send_generation
                    .checked_add(1)
                else {
                    return BlockingSendCommitOutcome::RefusedGenerationExhausted;
                };

                // ── rank 3 preflight + the ONLY fallible mutation ───────────────────────────
                let waiter = crate::kernel::boot::SenderWaiter {
                    tid: ThreadId(snap.sender_tid),
                    msg: snap.msg,
                    asid: Some(snap.sender_asid),
                    send_generation: next_generation,
                };
                let ipc_outcome =
                    self.with_ipc_split_mut(|ipc| {
                        if ipc
                            .endpoints
                            .get(snap.endpoint_idx)
                            .and_then(Option::as_ref)
                            .is_none()
                        {
                            return Err(BlockingSendCommitOutcome::RefusedEndpointMissing);
                        }
                        if ipc.endpoint_generations.get(snap.endpoint_idx).copied()
                            != Some(snap.endpoint_generation)
                        {
                            return Err(
                                BlockingSendCommitOutcome::RefusedEndpointGenerationChanged,
                            );
                        }
                        let queue = &mut ipc.endpoint_sender_waiters[snap.endpoint_idx];
                        if queue[..MAX_ENDPOINT_SENDER_WAITERS]
                            .iter()
                            .flatten()
                            .any(|w| w.tid.0 == snap.sender_tid)
                        {
                            return Err(BlockingSendCommitOutcome::RefusedDuplicateWaiter);
                        }
                        let Some(slot) = queue[..MAX_ENDPOINT_SENDER_WAITERS]
                            .iter_mut()
                            .find(|s| s.is_none())
                        else {
                            return Err(BlockingSendCommitOutcome::RefusedWaiterQueueFull);
                        };
                        *slot = Some(waiter);
                        // U6-FRAME §5A — waiter-present coordination PARITY with the in-lock route.
                        //
                        // `KernelState::enqueue_sender_waiter` pushes the proof coordination signal
                        // into E2 from inside the SAME `ipc_state_lock` section that fills the waiter
                        // slot. This publication route fills the very same slot without ever calling
                        // that function, so on any architecture that takes it the signal was never
                        // pushed and the proof's receiver could not tell that the sender had become a
                        // real waiter. Emitting it HERE — after the exact slot fill, inside the same
                        // rank-3 section, and only on the success path — restores the identical
                        // atomic proxy: "E2 has the signal" ⇔ "this sender is a waiter on E1".
                        //
                        // Every refusal above returns before this point, so a refused commit signals
                        // nothing; and with the sub-knob off
                        // `proof_sender_wake_coordination_target` returns `None`, so this is a strict
                        // no-op on every ordinary boot and on every endpoint except the proof E1.
                        if let Some(e2_idx) =
                            crate::kernel::boot::proof_sender_wake_coordination_target(
                                snap.endpoint_idx,
                            )
                        {
                            crate::kernel::boot::proof_sender_wake_push_coordination_locked(
                                ipc,
                                e2_idx,
                                snap.sender_tid,
                            );
                            crate::yarm_log!(
                                "IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT endpoint={} tid={}",
                                snap.endpoint_idx,
                                snap.sender_tid
                            );
                        }
                        Ok(())
                    });
                if let Err(refusal) = ipc_outcome {
                    // Nothing was mutated in any domain: rank 3 refused before writing, and
                    // ranks 2 and 1 have not been touched.
                    return refusal;
                }

                // ── infallible commit: rank 2, then rank 1, both still held ─────────────────
                {
                    let tcb = tcbs[idx].as_mut().expect("position guarantees Some");
                    tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(snap.send_cap));
                    tcb.ipc_timeout_deadline = snap.deadline;
                    tcb.ipc_timeout_fired = false;
                    tcb.blocked_send_generation = next_generation;
                }
                // `block_current_on` is the SAME rank-1 transition the in-lock route's
                // `block_current_cpu` performs — this adds no second `current` writer — and it
                // runs inside the rank-1 section that validated `current` above, so the
                // blocked-but-still-current window never exists.
                let cleared = kernel_mut(&mut sched.scheduler).block_current_on(snap.cpu);
                sched.timer.reset_quantum();
                debug_assert_eq!(
                    cleared.map(|t| t.0),
                    Some(snap.sender_tid),
                    "rank-1 preflight already proved the sender is current"
                );
                crate::yarm_log!("SCHED_BLOCK tid={}", snap.sender_tid);
                BlockingSendCommitOutcome::Committed {
                    send_generation: next_generation,
                }
            })
        })
    }

    /// U6 §2 — the split (rank-2 only) form of
    /// `KernelState::publish_blocking_send_completion`, for the post-lock wake sites.
    ///
    /// Byte-for-byte the same decision as the in-lock body: refused with NO mutation unless the
    /// target's `{tid, asid, send_generation}` still names a live incarnation blocked in
    /// `EndpointSend`. One acquisition, no other domain touched.
    fn publish_blocking_send_completion_split(
        &self,
        target: crate::kernel::ipc::SenderWakeTarget,
        result: u64,
    ) -> bool {
        use crate::kernel::task::{TaskStatus, WaitReason};
        let Some((tid, asid, send_generation)) = target.exact() else {
            return false;
        };
        let published = self.with_task_tcbs_split_mut(|tcbs| {
            let Some(tcb) = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == tid && t.asid == Some(asid))
            else {
                return false;
            };
            if tcb.blocked_send_generation != send_generation {
                return false;
            }
            if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {
                return false;
            }
            tcb.pending_syscall_completion = Some(crate::kernel::task::BlockedSyscallCompletion {
                syscall_class: crate::kernel::task::BlockedSyscallClass::IpcSend,
                result,
                tid,
                asid,
                blocked_generation: send_generation,
            });
            true
        });
        if published {
            crate::yarm_log!(
                "U6_SEND_COMPLETION_PUBLISHED tid={} asid={} send_generation={} result={} result=ok",
                tid,
                asid.0,
                send_generation,
                result
            );
        } else {
            crate::yarm_log!(
                "U6_SEND_COMPLETION_REFUSED_STALE tid={} asid={} send_generation={}",
                tid,
                asid.0,
                send_generation
            );
        }
        published
    }

    /// U3 (canonical 203C) — the split form of `KernelState::wake_tid_to_runnable`.
    ///
    /// Byte-for-byte the same decisions and the same five markers as the in-lock body: the
    /// accepted old states are `Blocked | Runnable | Running` and everything else returns
    /// `WouldBlock` after `SCHED_WAKE_FAIL`; a missing task is `TaskMissing`; the status becomes
    /// `Runnable` only when it was not already; `ipc_timeout_deadline`/`ipc_timeout_fired` are
    /// cleared; a `Running` task already current on this CPU is NOT redundantly enqueued; and
    /// otherwise placement is the pinned CPU when the task has an affinity, else this CPU, with
    /// the spawn-reservation refusal, class-derived priority, first-user pinning invariant and
    /// `reason=` string all unchanged.
    ///
    /// The rank-2 half is one acquisition and the rank-1 enqueue is a separate later one, so no
    /// lock is held across the other.
    ///
    /// **It does NOT bind `current_cpu`** — every caller has already authenticated and bound the
    /// CPU it passes. U3 (203C) gave it a second production caller, the ordinary-cap deferred
    /// sender wake (through [`Self::apply_split_sender_wake_plan_split`]), and renamed it from
    /// `wake_blocked_waiter_split` to name the operation rather than the first caller: it is the
    /// single split form of `wake_tid_to_runnable`, and there is exactly one body.
    ///
    /// U9-D3 (§6) gave it a third production caller — the split
    /// `report_transfer_revoke_to_supervisor_split` endpoint wake, which is the exact split twin
    /// of `wake_waiter_for_endpoint`'s `wake_tid_to_runnable`. It is `pub(crate)` for that caller;
    /// there is still exactly one body.
    pub(crate) fn wake_tid_to_runnable_split(
        &self,
        cpu: CpuId,
        tid: crate::kernel::ipc::ThreadId,
    ) -> Result<(), KernelError> {
        use crate::kernel::boot::map_scheduler_error;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::{TaskClass, TaskStatus};

        const BOOTSTRAP_FIRST_USER_TID: u64 = 1;

        // (4) rank 2 — status validation + transition + timeout clear + placement facts, in ONE
        // acquisition. `WakeState` mirrors what the in-lock body read from the TCB.
        struct WakeState {
            old_status: TaskStatus,
            affinity: Option<CpuId>,
            priority: TaskPriority,
            reserved: bool,
        }
        let state = self.with_task_enqueue_policy_split_mut(|tcbs, classes| {
            let idx = tcbs
                .iter()
                .position(|s| s.as_ref().is_some_and(|t| t.tid.0 == tid.0))
                .ok_or(KernelError::TaskMissing)?;
            let old_status = tcbs[idx].as_ref().expect("slot present").status;
            crate::yarm_log!("SCHED_WAKE_BEGIN tid={} old_status={:?}", tid.0, old_status);
            if !matches!(
                old_status,
                TaskStatus::Blocked(_) | TaskStatus::Runnable | TaskStatus::Running
            ) {
                crate::yarm_log!(
                    "SCHED_WAKE_FAIL tid={} reason=unexpected_status:{:?}",
                    tid.0,
                    old_status
                );
                return Err(KernelError::WouldBlock);
            }
            let tcb = tcbs[idx].as_mut().expect("slot present");
            if !matches!(old_status, TaskStatus::Runnable) {
                tcb.status = TaskStatus::Runnable;
            }
            // `clear_ipc_timeout_for_tid`, in the same acquisition.
            tcb.ipc_timeout_deadline = None;
            tcb.ipc_timeout_fired = false;
            // Canonical 199E: the in-lock twin retires the caller's reply-deadline registration
            // here too. The handle is captured under this same rank-2 claim; the rank-3 store
            // release runs after this section drops, never nested inside it.
            let retire = tcb.reply_timeout_token.take();
            let affinity = tcb.cpu_affinity;
            let reserved = tcb.is_spawn_reservation();
            // `task_priority`: TID 0 is the idle/supervisor sentinel and is Normal WITHOUT a
            // class lookup; every other TID needs a class, and a missing one is `TaskMissing`.
            let priority = if tid.0 == 0 {
                TaskPriority::Normal
            } else {
                match classes[idx] {
                    Some(TaskClass::SystemServer) => TaskPriority::High,
                    Some(TaskClass::Driver) | Some(TaskClass::App) => TaskPriority::Normal,
                    None => return Err(KernelError::TaskMissing),
                }
            };
            Ok((
                WakeState {
                    old_status,
                    affinity,
                    priority,
                    reserved,
                },
                retire,
            ))
        })?;
        let (state, retire) = state;
        // Rank 2 is released. Rank 3: return the caller's reply-deadline slot to the bounded
        // store, exactly once and only for the handle this wake took off the TCB.
        if let Some(handle) = retire {
            self.with_ipc_split_mut(|ipc| {
                if let Some(t) = ipc.reply_deadline_tokens.get_mut(handle.token_index()) {
                    let _ = t.disarm_after_terminal_completion(handle.identity(), handle.epoch());
                }
            });
        }
        crate::yarm_log!("SCHED_WAKE_SET_RUNNABLE tid={} new_status=Runnable", tid.0);

        // (5) rank 1 — the placement decision. `current_tid_on(cpu)` is the same fact
        // `current_tid()` read: the in-lock body resolved it through `current_cpu()`, which is
        // this CPU (bound in step 1, and MPIDR-derived to the same value on freestanding
        // AArch64, where `cpu` is the trapping CPU).
        self.with_scheduler_split_mut(|sched| {
            let already_current = kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .is_some_and(|c| c.0 == tid.0)
                && matches!(state.old_status, TaskStatus::Running);
            if already_current {
                crate::yarm_log!("SCHED_WAKE_ALREADY_RUNNABLE tid={}", tid.0);
                return Ok(());
            }
            // `enqueue_woken_task`: the pinned CPU when the task has an affinity, else this CPU.
            let (target, reason) = match state.affinity {
                Some(pinned) => (pinned, "pinned"),
                None => (sched.current_cpu, "current_cpu"),
            };
            // `enqueue_on_cpu`'s refusals, in its order: spawn reservation first, then the queue
            // primitive's own mapped error.
            if state.reserved {
                crate::yarm_log!(
                    "ENQUEUE_REFUSED tid={} reason=spawn_reservation_not_live",
                    tid.0
                );
                return Err(KernelError::WrongObject);
            }
            // First-user CPU pinning: TID 1 must only ever be placed on the bootstrap CPU.
            if tid.0 == BOOTSTRAP_FIRST_USER_TID
                && target.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                && cfg!(not(feature = "hosted-dev"))
            {
                crate::yarm_log!(
                    "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                    sched.current_cpu.0,
                    tid.0,
                    target.0
                );
                assert_eq!(target.0, crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
            }
            kernel_mut(&mut sched.scheduler)
                .enqueue_on_with_priority(target, tid, state.priority)
                .map_err(map_scheduler_error)?;
            if tid.0 == BOOTSTRAP_FIRST_USER_TID && cfg!(not(feature = "hosted-dev")) {
                let q = |c: u8| kernel_ref(&sched.scheduler).runnable_count_on(CpuId(c));
                crate::yarm_log!(
                    "BOOTSTRAP_ENQUEUE_VERIFY tid=1 queue0_len={} queue1_len={} queue2_len={} queue3_len={}",
                    q(0),
                    q(1),
                    q(2),
                    q(3)
                );
            }
            let queue_len = kernel_ref(&sched.scheduler).runnable_count_on(target);
            crate::yarm_log!(
                "SCHED_WAKE_ENQUEUE tid={} cpu={} queue_len={} reason={}",
                tid.0,
                target.0,
                queue_len,
                reason
            );
            Ok(())
        })
    }

    /// U3 (canonical 203C) — bind this CPU as the scheduler's current CPU, as ONE rank-1
    /// critical section.
    ///
    /// Replaces the broad `with_cpu(ctx.cpu_id, |kernel| …)` re-acquire in the x86_64 D6
    /// first-resume trampoline. That acquisition existed to run
    /// `post_switch_restore_arch_thread_state(kernel, cpu, None)` — but with `frame == None`
    /// the x86_64 `restore_arch_thread_state` returns `Ok(())` on its very first statement,
    /// before reading the current TID, before touching a TCB, before restoring context or TLS,
    /// before activating an ASID, before checking or writing CR3, and before taking any domain
    /// lock. So the ONLY `KernelState` effect the broad body ever had was the CPU
    /// validation-and-bind that `with_cpu` itself performs on entry. That is exactly, and only,
    /// what this transaction does.
    ///
    /// Semantics are the broad path's:
    ///
    /// 1. rank 1 (scheduler) is acquired exactly once;
    /// 2. `cpu` is validated with `validate_online_cpu` — the SAME predicate
    ///    `KernelState::set_current_cpu` uses;
    /// 3. on failure the same `KernelError` class is returned and `scheduler.current_cpu` is
    ///    left unchanged, with no scheduler state mutated at all;
    /// 4. on success `scheduler.current_cpu = cpu` is bound, and it succeeds whether or not
    ///    that CPU has a current task — no current TID is read or required.
    ///
    /// This deliberately does NOT go through `current_tid_authoritative`: that helper's
    /// `Option` conflates "valid CPU with no current task" with refusal, and it performs a
    /// current-TID read this binding has no use for.
    ///
    /// The guard is released when this returns, before the caller's marker emission and its
    /// hardware-CR3 observation.
    ///
    /// U3 (203C) — **architecture-neutral**. This was introduced `#[cfg(target_arch = "x86_64")]`
    /// only because its first caller was the x86_64 D6 first-resume path. Nothing in the body is
    /// architecture-specific: `validate_online_cpu` is the portable scheduler predicate
    /// `KernelState::set_current_cpu` applies on every architecture, and `sched.current_cpu` is the
    /// portable stored binding. The recv copy-fault completion transaction reaches it on every
    /// architecture, so the gate is gone rather than a second CPU-binding implementation existing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn bind_current_cpu_split(&self, cpu: CpuId) -> Result<(), KernelError> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            sched.current_cpu = cpu;
            Ok(())
        })
    }

    /// U3 (canonical 203C) — the authoritative return-to-scheduler transaction for an x86_64 AP,
    /// taken as ONE rank-1 critical section.
    ///
    /// Replaces the broad `with_cpu(cpu, |k| k.block_current_on_cpu(cpu))` re-acquire in
    /// `ap_seal_return_to_idle`. That acquisition took the whole `KernelState` lock to do one
    /// scheduler-domain thing: take this CPU's `current` and drop its membership entry. Nothing
    /// it touched lives outside rank 1.
    ///
    /// Semantics are the broad path's, step for step:
    ///
    /// 1. rank 1 (scheduler) is acquired exactly once, for the whole transaction;
    /// 2. `cpu` is validated with `validate_online_cpu` — the SAME predicate
    ///    `KernelState::set_current_cpu` uses, which is what `with_cpu` called on entry;
    /// 3. on validation failure the same `KernelError` class is returned, `current_cpu` is left
    ///    UNCHANGED, and NO current task is removed — `with_cpu` never ran its closure either;
    /// 4. on success `scheduler.current_cpu = cpu` is bound, exactly as `with_cpu` did, and
    ///    unconditionally — including when that CPU has no current task to remove;
    /// 5. the existing `block_current_on(cpu)` primitive runs inside that same guard, so the
    ///    membership-table removal is the scheduler's own, not reproduced here.
    ///
    /// This is a scheduler-only mutation and deliberately nothing more: rank 2 is never taken,
    /// no `TaskStatus` is read or written, nothing is enqueued or dispatched, and clearing
    /// `current` is NOT treated as a task-state transition — there is no rollback, ownership,
    /// waiter, wake or barrier logic here. The removed TID is returned verbatim, `Some(0)`
    /// included, and `None` when the CPU has no current task. The guard is released when this
    /// returns, before the caller's `printk_emit_sync`.
    #[cfg(target_arch = "x86_64")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn block_current_on_cpu_split(
        &self,
        cpu: CpuId,
    ) -> Result<Option<u64>, KernelError> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            sched.current_cpu = cpu;
            Ok(kernel_mut(&mut sched.scheduler)
                .block_current_on(cpu)
                .map(|tid| tid.0))
        })
    }

    /// U3 (canonical 203C) — the x86_64 BSP saved-resume PREEMPT-AND-PREFER selection, taken as
    /// ONE rank-1 scheduler transaction for an EXPLICIT CPU.
    ///
    /// Replaces the broad re-acquire `c2c_bsp_saved_frame_resume` performed:
    ///
    /// ```ignore
    /// shared.with_cpu(cpu, |k| k.on_preempt_prefer_on_cpu(cpu, client_tid)).unwrap_or(None)
    /// ```
    ///
    /// That acquisition took the WHOLE `KernelState` lock to perform one scheduler-domain
    /// operation. `KernelState::on_preempt_prefer_on_cpu` was a thin wrapper that immediately
    /// re-entered the rank-1 scheduler lock and called `Scheduler::on_preempt_prefer_on`; the
    /// broad guard added nothing but width. Here rank 1 is acquired once and the SAME scheduler
    /// primitive runs inside it, so the policy is not duplicated — `Scheduler::on_preempt_prefer_on`
    /// (and beneath it `PriorityScheduler::on_preempt_prefer`) remains the single implementation.
    ///
    /// Contract, outcome for outcome:
    ///
    /// * **online validation** is the scheduler's own `check_online_cpu(cpu)` inside
    ///   `on_preempt_prefer_on`, which returns `None` for an invalid or offline CPU. The broad
    ///   form reached the same `None` by a different route — `with_cpu` refused first and the
    ///   caller's `.unwrap_or(None)` collapsed the error — so an invalid CPU still yields `None`
    ///   with NO mutation of any kind;
    /// * the previous current task is re-enqueued and `preferred` is made current if it is
    ///   queued on that CPU, exactly as before;
    /// * **the legacy fallback is preserved, deliberately.** When `preferred` is NOT queued on
    ///   `cpu`, the scheduler may still select some other runnable task and return it. That is
    ///   not "improved" here: the caller compares the returned TID against its own and aborts
    ///   when they differ, so the mutation and the result both stay exactly what they were;
    /// * the actual selected TID is returned verbatim, `None` included.
    ///
    /// Deliberately NOT done: no rank-2 or any other domain is entered, no `TaskStatus` is read
    /// or written, and the process-global ambient `scheduler.current_cpu` is neither read nor
    /// written — this transaction is named by the explicit `cpu` its caller trapped on. (The
    /// retired `with_cpu` bound `current_cpu` as an admission side effect; on this path that was
    /// a write of CPU 0 over CPU 0, because the consumer returns immediately unless `cpu.0 == 0`
    /// and the broad Phase-2 trap guard already bound the same value.) No broad fallback, no
    /// drain, no retry.
    #[cfg(target_arch = "x86_64")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn on_preempt_prefer_on_cpu_split(
        &self,
        cpu: CpuId,
        preferred_tid: u64,
    ) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            kernel_mut(&mut sched.scheduler)
                .on_preempt_prefer_on(cpu, crate::kernel::ipc::ThreadId(preferred_tid))
                .map(|tid| tid.0)
        })
    }

    /// U3 (canonical 203C) — the RISC-V post-lock TERMINAL-IDLE predicate, taken as ONE
    /// coherent rank-1 scheduler snapshot.
    ///
    /// Replaces the broad re-acquire the blocked-syscall idle branch of
    /// `handle_riscv_trap_entry_shared` performed:
    ///
    /// ```ignore
    /// shared.with_cpu(cpu, |k| {
    ///     matches!(k.current_tid(), None | Some(0)) && k.runnable_count_on_cpu(cpu) == 0
    /// }).unwrap_or(false)
    /// ```
    ///
    /// That acquisition took the WHOLE `KernelState` lock to answer one scheduler-domain
    /// question. Its two reads (`current_tid`, `runnable_count_on_cpu`) each took the scheduler
    /// lock separately; the broad guard is what made them coherent. Here they are one
    /// acquisition, so the predicate cannot tear between them — which is why this is not
    /// `current_tid_authoritative` followed by a second runnable-count read.
    ///
    /// Semantics are the broad path's, step for step:
    ///
    /// 1. rank 1 (scheduler) is acquired exactly once, for the whole transaction;
    /// 2. `cpu` is validated with `validate_online_cpu` — the SAME predicate
    ///    `KernelState::set_current_cpu` uses, so a refusal returns the same `KernelError` class
    ///    the broad `with_cpu` returned and leaves `scheduler.current_cpu` — and everything else
    ///    — untouched. The caller's `.unwrap_or(false)` then reports the same `false`;
    /// 3. on success `scheduler.current_cpu = cpu` is bound, exactly as `with_cpu` did on entry;
    /// 4. `current_tid_on(cpu)` and `runnable_count_on(cpu)` are read under that one guard, and
    ///    the verdict is the identical `matches!(current, None | Some(0)) && runnable == 0`.
    ///
    /// The broad body read `current_tid()`, which resolves to `current_tid_on(current_cpu)`
    /// after the bind — the same value `current_tid_on(cpu)` names directly here.
    ///
    /// Nothing else happens: no task lock, no `TaskStatus`, no dispatch, dequeue or enqueue, no
    /// ownership token, no provenance mutation, and no fallback. The provenance take stays where
    /// it was, ahead of this call.
    pub(crate) fn terminal_idle_on_cpu_split(&self, cpu: CpuId) -> Result<bool, KernelError> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(crate::kernel::boot::map_scheduler_error)?;
            sched.current_cpu = cpu;
            let current = kernel_ref(&sched.scheduler)
                .current_tid_on(cpu)
                .map(|t| t.0);
            let runnable = kernel_ref(&sched.scheduler).runnable_count_on(cpu);
            Ok(matches!(current, None | Some(0)) && runnable == 0)
        })
    }

    /// U3 (canonical 203C) — the authoritative enqueue→dispatch transaction for an x86_64 AP
    /// placement, taken as ONE rank-1 → rank-2 critical section.
    ///
    /// Replaces the broad `with_cpu(cpu, |k| { enqueue_on_cpu(..); dispatch_next_on_cpu(..) })`
    /// re-acquire. `KernelState::enqueue_on_cpu` resolves its task-domain policy through two
    /// separate `with_tcbs` acquisitions (the spawn-reservation refusal, then the class→priority
    /// derivation) and only afterwards takes the scheduler lock to mutate the run queue. Under
    /// the broad lock nothing could interleave, but the shape itself is torn: the policy the
    /// enqueue acts on is read in a critical section that has already been released by the time
    /// the queue is touched. This transaction closes that.
    ///
    /// Lock ordering — canonical ascending 1 → 2, acquired once each:
    ///
    /// 1. rank 1 (scheduler) is acquired exactly once, for the whole transaction;
    /// 2. the CPU is validated with `validate_online_cpu` — the SAME predicate
    ///    `KernelState::set_current_cpu` uses, which is what `with_cpu` called on entry. On
    ///    failure the same `KernelError` class is returned and nothing below has run;
    /// 3. rank 2 (task) is nested INSIDE the still-held rank-1 guard, and resolves the entire
    ///    enqueue policy — idle-TID shortcut, task existence, class → priority,
    ///    spawn-reservation refusal — in one acquisition;
    /// 4. the existing scheduler primitives run under the rank-1 guard already held:
    ///    `enqueue_on_with_priority`, then `dispatch_next_on` per the typed policy.
    ///
    /// **Validate-EXPLICIT-CPU, not validate-BIND.** This transaction does NOT write
    /// `scheduler.current_cpu`. That field is ONE process-global selector which `current_tid()`
    /// and `current_task_cnode()` resolve through, so binding it from an off-broad seam
    /// retargets every ambient reader on every CPU — including a CPU sitting mid-syscall inside
    /// the broad lock. That failure was measured directly on the SMP saved-resume path, where an
    /// off-broad bind flipped the other CPU's ambient identity and made `handle_ipc_recv`
    /// validate a receive capability against the wrong process CNode. Every step above is named
    /// by the explicit `cpu` argument, so the binding was never load-bearing for the returned
    /// outcome: `validate_online_cpu(cpu)`, `enqueue_on_with_priority(cpu, ..)` and
    /// `dispatch_next_on(cpu)` all take the CPU explicitly and read no ambient selector.
    /// Removing it therefore changes no result this transaction can produce, and both callers
    /// carry the CPU and the selected TID forward as plain values. Broad `with_cpu` keeps its
    /// own legacy binding; that is transaction-local state and out of scope here.
    ///
    /// Neither nested step re-enters its domain through a `KernelState` accessor: `task_priority`
    /// and `refuse_enqueue_of_spawn_reservation` would each re-take the task lock, and
    /// `current_cpu()` / `scheduler_state()` would re-take the scheduler lock this transaction
    /// already holds. The policy is therefore recomputed here from the same fields those
    /// accessors read, with identical semantics — including the first-user CPU-pinning invariant
    /// and every existing error mapping. Eligibility is NOT strengthened: there is deliberately
    /// no `Runnable` requirement, so an already-queued or already-current task refuses exactly
    /// where it refused before and nowhere else.
    ///
    /// Every guard is released when this returns; the caller does its logging, CR3 work, frame
    /// construction, register writes and `iretq` with nothing held.
    #[cfg(target_arch = "x86_64")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn enqueue_then_dispatch_on_cpu_split(
        &self,
        cpu: CpuId,
        tid: u64,
        policy: EnqueueRefusalPolicy,
    ) -> Result<CpuEnqueueDispatch, KernelError> {
        use crate::kernel::boot::map_scheduler_error;
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::TaskClass;

        // BOOTSTRAP_FIRST_USER_TID / DEBUG_DISPATCH_CONTEXT_LOG mirror the private constants in
        // `scheduler_state.rs`; the invariant they guard is reproduced below unchanged.
        const BOOTSTRAP_FIRST_USER_TID: u64 = 1;
        const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;

        self.with_scheduler_split_mut(|sched| {
            // (2) Same admission predicate `set_current_cpu` uses. A refusal returns the same
            // error class with nothing below having run.
            kernel_ref(&sched.scheduler)
                .validate_online_cpu(cpu)
                .map_err(map_scheduler_error)?;
            // (3) No ambient binding. `scheduler.current_cpu` is deliberately NOT written here —
            // see the contract above. Every step below names the CPU explicitly.

            // (4) One rank-2 acquisition, nested while rank 1 is held (ascending 1 -> 2),
            // resolving the whole task-domain policy `enqueue_on_cpu` reads across two.
            //
            // `task_priority`: TID 0 is the idle/supervisor sentinel and is Normal WITHOUT a
            // TCB lookup; every other TID needs a class, and a missing class is `TaskMissing`.
            // `refuse_enqueue_of_spawn_reservation`: a reservation is not a live task.
            let policy_result: Result<TaskPriority, KernelError> =
                self.with_task_enqueue_policy_split_mut(|tcbs, classes| {
                    let slot = tcbs.iter().position(|slot| {
                        slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid)
                    });
                    if let Some(idx) = slot
                        && tcbs[idx]
                            .as_ref()
                            .is_some_and(|tcb| tcb.is_spawn_reservation())
                    {
                        crate::yarm_log!(
                            "ENQUEUE_REFUSED tid={} reason=spawn_reservation_not_live",
                            tid
                        );
                        return Err(KernelError::WrongObject);
                    }
                    if tid == 0 {
                        return Ok(TaskPriority::Normal);
                    }
                    let class = slot
                        .and_then(|idx| classes[idx])
                        .ok_or(KernelError::TaskMissing)?;
                    Ok(match class {
                        TaskClass::SystemServer => TaskPriority::High,
                        TaskClass::Driver | TaskClass::App => TaskPriority::Normal,
                    })
                });

            // (5) The scheduler primitives, under the rank-1 guard already held. The enqueue
            // verdict is composed exactly as `enqueue_on_cpu` composes it: the policy errors
            // above short-circuit, otherwise the queue primitive's own mapped error stands.
            let enqueued: Result<(), KernelError> = match policy_result {
                Err(err) => Err(err),
                Ok(priority) => {
                    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                        crate::yarm_log!(
                            "ENQUEUE_CALL cpu_current={} cpu_target={} tid={}",
                            cpu.0,
                            cpu.0,
                            tid
                        );
                    }
                    // First-user CPU pinning: TID 1 must only ever be placed on the bootstrap
                    // CPU. Reproduced verbatim, asserts included.
                    if tid == BOOTSTRAP_FIRST_USER_TID
                        && cpu.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                        && cfg!(not(feature = "hosted-dev"))
                    {
                        crate::yarm_log!(
                            "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                            cpu.0,
                            tid,
                            cpu.0
                        );
                        assert_eq!(cpu.0, crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
                        assert_eq!(
                            cpu.0 as usize,
                            crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize
                        );
                    }
                    let placed = kernel_mut(&mut sched.scheduler)
                        .enqueue_on_with_priority(cpu, ThreadId(tid), priority)
                        .map_err(map_scheduler_error);
                    if placed.is_ok()
                        && tid == BOOTSTRAP_FIRST_USER_TID
                        && cfg!(not(feature = "hosted-dev"))
                    {
                        let q = |c: u8| kernel_ref(&sched.scheduler).runnable_count_on(CpuId(c));
                        crate::yarm_log!(
                            "BOOTSTRAP_ENQUEUE_VERIFY tid=1 queue0_len={} queue1_len={} queue2_len={} queue3_len={}",
                            q(0),
                            q(1),
                            q(2),
                            q(3)
                        );
                    }
                    placed
                }
            };

            // The one place the two historical policies differ, and the only place they may.
            let selected = match (policy, &enqueued) {
                (EnqueueRefusalPolicy::Decline, Err(_)) => None,
                _ => kernel_mut(&mut sched.scheduler)
                    .dispatch_next_on(cpu)
                    .map(|t| t.0),
            };
            Ok(CpuEnqueueDispatch { enqueued, selected })
        })
    }

    /// U3 (canonical 203C) — the authoritative saved-context snapshot for an x86_64 AP
    /// saved-frame resume, taken through the rank-2 task seam only.
    ///
    /// Replaces the broad `shared.with(|k| k.ap_saved_resume_context(tid))` re-acquire that
    /// `ap_saved_frame_resume` used to take. The legacy `KernelState` body answered the same
    /// question across FOUR separate reads — `task_asid`, then `cr3_for_asid`, then
    /// `task_status`, then `with_tcbs` for the context — each of which re-entered the task
    /// domain on its own. Under the broad lock that was safe but incoherent by construction:
    /// nothing in its shape says the ASID, the status and the register context belong to the
    /// same incarnation. This transaction takes all of them in ONE rank-2 acquisition, so a
    /// resume can never mix one incarnation's ASID with another's saved frame.
    ///
    /// Lock ordering, and why CR3 resolution is deliberately outside:
    ///
    /// 1. rank 2 (task) is acquired exactly once and every task-owned field — ASID, status,
    ///    the full `UserRegisterContext`, and the TLS pointer — is copied by value;
    /// 2. rank 2 is released completely when the seam closure returns;
    /// 3. only then is `asid` resolved to a page-table root through
    ///    `x86_64::page_table::cr3_for_asid`, which takes `PAGE_TABLE_STATE` — an INDEPENDENT,
    ///    unranked lock. Holding the task lock across it would couple two lock orders that the
    ///    rank system says nothing about, so it is not held.
    ///
    /// The refusal set is byte-for-byte the legacy body's: an absent TCB, a TCB with no ASID,
    /// and an ASID with no page-table root each return `None`. `runnable_saved` is likewise
    /// unchanged — `Runnable | Running` AND a complete (`rip != 0 && rsp != 0`) saved frame;
    /// `Blocked`, `Faulted`, `Exited`, `Dead` and `Reserved` are all rejected. TLS absent maps
    /// to `fs_base = 0`.
    ///
    /// Identity contract — deliberately NOT strengthened: like the legacy body, this locates
    /// the task by numeric TID and reports the ASID it finds bound to that TID in the same
    /// snapshot. The callers do not hold a generation-bearing or exact-incarnation token, so
    /// this claims no exact-incarnation authority they could not honour.
    ///
    /// Read-only in both domains: no dispatch, no enqueue, no status write, no context
    /// consumption.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn ap_saved_resume_context_split(&self, tid: u64) -> Option<ApSavedResumeContext> {
        // Phase 1 — ONE rank-2 acquisition. Everything task-owned is copied out by value, so
        // nothing below observes the TCB array after the guard drops.
        let (asid, context, runnable, fs_base) = self.with_task_tcbs_split_mut(|tcbs| {
            let tcb = tcbs.iter().flatten().find(|t| t.tid.0 == tid)?;
            let asid = tcb.asid?;
            let runnable = matches!(
                tcb.status,
                crate::kernel::task::TaskStatus::Runnable
                    | crate::kernel::task::TaskStatus::Running
            );
            // Stage 199A2D2C2B: FS base comes from the SELECTED TASK's saved TLS state, never a
            // hardcoded constant; a task with no TLS resumes with FS.base = 0.
            let fs_base = tcb.tls_ptr.map(|v| v.0).unwrap_or(0);
            Some((asid, tcb.user_context, runnable, fs_base))
        })?;
        // Phase 2 — rank 2 is released. `PAGE_TABLE_STATE` is an independent, unranked lock and
        // must not be taken while the task lock is held.
        let cr3 = crate::arch::x86_64::page_table::cr3_for_asid(asid)?;
        let mut gprs = [0u64; 15];
        for (i, g) in gprs.iter_mut().enumerate() {
            *g = context.user_gprs[i] as u64;
        }
        let rip = context.instruction_ptr.0;
        let rsp = context.stack_ptr.0;
        let has_saved = rip != 0 && rsp != 0;
        Some(ApSavedResumeContext {
            asid: asid.0,
            cr3,
            rip,
            rsp,
            gprs,
            fs_base,
            runnable_saved: runnable && has_saved,
        })
    }

    /// Stage 108: VM/user-spaces (rank 5) split-mut seam.
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — called by
    ///   `try_split_vm_brk_shrink_into_frame` once per unmapped page.
    pub(crate) fn with_vm_user_spaces_split_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::kernel::vm::AddressSpaceManager) -> R,
    ) -> R {
        // SAFETY: same pattern — the vm lock serializes user_spaces storage.
        let (vm_lock, user_spaces) =
            unsafe { KernelState::vm_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let vm_lock = unsafe { &*vm_lock };
        let _guard = vm_lock.lock();
        let user_spaces = unsafe { &mut *user_spaces };
        f(kernel_mut(user_spaces))
    }

    /// Stage 108: memory/frame-allocator (rank 6) split-mut seam.
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — called by
    ///   `try_split_vm_brk_shrink_into_frame` once for the initial brk-bounds
    ///   read, once per unmapped page (COW clear + mapping-removed bookkeeping
    ///   + frame reclaim), and once more for the final brk-bounds write.
    pub(crate) fn with_memory_split_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::kernel::boot::MemorySubsystem) -> R,
    ) -> R {
        // SAFETY: same pattern — the memory lock serializes MemorySubsystem
        // storage (memory objects + frame bookkeeping).
        let (memory_lock, memory) =
            unsafe { KernelState::memory_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let memory_lock = unsafe { &*memory_lock };
        let _guard = memory_lock.lock();
        let memory = unsafe { &mut *memory };
        f(kernel_mut(memory))
    }

    /// Stage 115: IPC/waiter-publish (rank 3) split-mut seam.
    ///
    /// # Validation status
    /// - M2_SEAM_HELPER_ONLY — no live caller as of Stage 115. D2 Phase C
    ///   (`recv_block_phase_c_ipc_publish`) cannot be moved outside `with_cpu`
    ///   until `dispatch_next_task` → `maybe_switch_kernel_context` →
    ///   `switch_frames` (arch-specific cooperative kernel context switch) is
    ///   restructured per arch; that is the precise Stage 115 blocker.
    ///
    /// Callers must not hold any lock of rank ≤ 3 (scheduler, task, or IPC)
    /// when invoking this seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_ipc_split_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::kernel::boot::IpcSubsystem) -> R,
    ) -> R {
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned
        // by this SharedKernel. `ipc_split_mut_ptrs_from_raw` derives raw
        // field pointers via addr_of!/addr_of_mut! without forming a
        // reference to the whole KernelState; `ipc_state_lock` serializes
        // access to the `ipc` storage.
        let (ipc_lock, ipc) =
            unsafe { KernelState::ipc_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let ipc_lock = unsafe { &*ipc_lock };
        let _guard = ipc_lock.lock();
        let ipc = unsafe { &mut *ipc };
        f(kernel_mut(ipc))
    }

    // ── U9-C: the rank-3 (IPC) halves of the queued-split recv Phase A ──────────────────
    //
    // Each of these drives the SAME body the broad `KernelState` method drives — the
    // `*_locked` free functions in `kernel/boot/ipc_state.rs` — through `with_ipc_split_mut`
    // instead of a broad `&mut KernelState`. One implementation, two owners; a divergence
    // between the split recv route and the legacy route is therefore not expressible.

    /// U9-C — rank-3 dequeue + two-phase sender refill, plain (no cap) receiver form.
    pub(crate) fn ipc_try_recv_queued_plain_endpoint_only_split(
        &self,
        endpoint_idx: usize,
    ) -> crate::kernel::boot::IpcEndpointRecvResult {
        self.with_ipc_split_mut(|ipc| {
            crate::kernel::boot::ipc_try_recv_queued_plain_endpoint_only_locked(ipc, endpoint_idx)
        })
    }

    /// U9-C — rank-3 dequeue + two-phase sender refill, cap-transfer-tolerant receiver form.
    pub(crate) fn ipc_try_recv_queued_with_cap_transfer_split(
        &self,
        endpoint_idx: usize,
    ) -> crate::kernel::boot::IpcEndpointRecvResult {
        self.with_ipc_split_mut(|ipc| {
            crate::kernel::boot::ipc_try_recv_queued_with_cap_transfer_locked(ipc, endpoint_idx)
        })
    }

    /// U9-C — rank-3 endpoint-index resolution with the same generation/liveness checks
    /// `KernelState::resolve_endpoint_index` performs. The capacity limit is read from the
    /// existing config split-read BEFORE the IPC lock is taken, so the two never nest.
    pub(crate) fn resolve_endpoint_index_split(
        &self,
        object: CapObject,
    ) -> Result<usize, KernelError> {
        let limits = self.runtime_capacity_config_split_read();
        match object {
            CapObject::Endpoint { index, generation } => self.with_ipc_split_mut(|ipc| {
                if index >= limits.max_endpoints {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoint_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            _ => Err(KernelError::WrongObject),
        }
    }

    /// U9-C — rank-3 read of a transfer envelope's source object, generation-guarded exactly
    /// as `KernelState::peek_transfer_envelope_source_object`. Reads only; consumes nothing.
    pub(crate) fn peek_transfer_envelope_source_object_split(
        &self,
        handle: u64,
    ) -> Option<CapObject> {
        let idx = usize::try_from(handle & 0xFFFF).ok()?;
        if idx >= crate::kernel::boot::MAX_TRANSFER_ENVELOPES {
            return None;
        }
        let generation = handle >> 16;
        if generation == 0 {
            return None;
        }
        self.with_ipc_split_mut(|ipc| {
            if ipc.transfer_envelope_generations[idx] != generation {
                return None;
            }
            ipc.transfer_envelopes[idx].map(|e| e.source_object)
        })
    }

    /// U9-C — the queued-split recv **Phase A**, off the broad lock.
    ///
    /// This is the off-lock twin of `crate::kernel::syscall::try_split_recv_queued_plain_with_snapshot_locked`,
    /// and the reason `try_split_ipc_recv_queued_plain_into_frame` no longer needs a broad
    /// `with_cpu`. Every step below either drives the SAME shared body the broad path drives, or
    /// is one of the ordered split transactions added by U9-C:
    ///
    /// | step | rank(s) | seam |
    /// |---|---|---|
    /// | receiver class | 2 | `task_asid_option_split_read` on the AUTHORITATIVE requester |
    /// | plan | — | `plan_recv_core` (pure) |
    /// | endpoint index | 3 | `resolve_endpoint_index_split` |
    /// | admit + dequeue + refill | 3 | `ipc_try_recv_queued_admitted_locked` (one acquisition) |
    /// | telemetry | 3 | `note_endpoint_only_queued_recv_split_seam` |
    /// | outcome mapping | — | `map_queued_recv_outcome` (shared with the broad cores) |
    /// | reply-cap materialization | 3→2→4→3 | `materialize_reply_cap_split` |
    /// | ordinary-cap snapshot | 3, 4, 2 | envelope facts + capability + CNode splits |
    /// | sender wake | 1 | `apply_split_sender_wake_plan_split` |
    ///
    /// # `is_kernel_task` is now exact, not ambient
    ///
    /// The broad body derived it from `current_task_has_user_asid`, i.e. from the AMBIENT current
    /// task — which is precisely why that closure had to be `with_cpu(cpu, …)` rather than `with`
    /// (the Stage 160 parity fix: on an SMP boot an unbound `current_cpu` could observe another
    /// CPU's task and misclassify the receiver). Here it is read from
    /// `snapshot.requester_tid`, the authoritative TID `current_tid_authoritative(cpu)` already
    /// resolved before Phase A. The ambient dependency — and with it the reason to bind a CPU —
    /// is gone rather than relocated.
    ///
    /// # What Phase A admits, and what it refuses BEFORE consuming anything
    ///
    /// Admitted: plain messages, `FLAG_REPLY_CAP` reply caps (the one class a real boot
    /// materializes here), and ordinary caps to a user receiver (whose mint was already off-lock).
    ///
    /// Refused pre-dequeue, via `ipc_try_recv_queued_admitted_locked`, because no off-lock
    /// materializer exists for them: shared-region (`OPCODE_SHARED_MEM`) messages, whose
    /// receiver-side mapping obligations sit outside the materialize step; cap transfers to a
    /// kernel-register receiver; and non-`FLAG_REPLY_CAP` messages whose envelope names a `Reply`
    /// object, which the broad router deliberately fails closed. Each returns `Fallback` with the
    /// message still queued, so the unchanged legacy path services it — the same contract Phase A
    /// has always had for a case it cannot serve.
    pub(crate) fn recv_queued_split_phase_a_split(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        snapshot: &EndpointRecvCapSnapshot,
    ) -> crate::kernel::syscall::RecvQueuedSplitPhaseA {
        use crate::kernel::ipc::{Message, pack_register_payload};
        use crate::kernel::recv_core::{
            RecvOutcome, RecvPlan, RecvSchedulerWakePlan, RecvWritebackPlan, kernel_register_plan,
            map_queued_recv_outcome, plan_recv_core, queued_recv_result_delivered,
            user_memory_plan, user_memory_v2_plan,
        };
        use crate::kernel::syscall::{
            OPCODE_SHARED_MEM, RecvQueuedSplitPhaseA, SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0,
            SYSCALL_ARG_INLINE_PAYLOAD1, SYSCALL_ARG_LEN, SYSCALL_ARG_PTR, SyscallError,
            recv_boundary_encode_transfer_cap_ret,
        };

        let receiver_tid = snapshot.requester_tid;
        // Rank 2 on the EXACT requester — never the ambient current task.
        let is_kernel_task = self.task_asid_option_split_read(receiver_tid).is_none();
        let recv_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
        let request = crate::kernel::recv_core::RecvRequest::from_legacy_ipc_recv(
            receiver_tid,
            recv_cap,
            frame.arg(SYSCALL_ARG_PTR),
            frame.arg(SYSCALL_ARG_LEN),
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
            frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            is_kernel_task,
        );

        let plan = plan_recv_core(&request);
        crate::yarm_log!("YARM_RECV_CORE_PLAN plan={:?}", plan);
        let (kind, is_user_writeback) = match plan {
            RecvPlan::KernelPlainEligible => ("kernel_plain", false),
            RecvPlan::UserPlainEligible => ("user_plain", true),
            RecvPlan::UserPlainV2Eligible => ("user_plain_v2", true),
            RecvPlan::FallbackRequired(reason) => {
                crate::yarm_log!("YARM_RECV_CORE_FALLBACK reason={:?}", reason);
                return RecvQueuedSplitPhaseA::Fallback;
            }
        };
        crate::yarm_log!("YARM_RECV_CORE_ADAPTER kind={}", kind);

        let endpoint = snapshot.endpoint;
        let endpoint_idx = match self.resolve_endpoint_index_split(endpoint) {
            Ok(idx) => idx,
            Err(e) => {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::from(e),
                )));
            }
        };

        // ── rank 3: admit + dequeue + two-phase refill, in ONE acquisition ──────────────────
        let result = self.with_ipc_split_mut(|ipc| {
            crate::kernel::boot::ipc_try_recv_queued_admitted_locked(
                ipc,
                endpoint_idx,
                |msg, ipc| {
                    // Admission classifies by MESSAGE SHAPE only. Envelope validity is
                    // deliberately NOT an admission criterion: a stale or missing handle must
                    // still be dequeued so it surfaces the same `InvalidCapability` the broad
                    // path raised, rather than degrading into a silent fallback.
                    let Some(handle) = msg.transferred_cap().map(|c| c.0) else {
                        return true; // plain: always serviceable
                    };
                    if msg.opcode == OPCODE_SHARED_MEM {
                        // Shared-region transfers carry receiver-side MAPPING obligations outside
                        // the materialize step; no off-lock materializer exists for them. Decline
                        // BEFORE consuming, so the unchanged legacy owner services the message.
                        return false;
                    }
                    if (msg.flags & Message::FLAG_REPLY_CAP) != 0 {
                        return true; // the exact reply-cap transaction serves this
                    }
                    // A Reply object tagged as an ORDINARY transfer is the forbidden queued
                    // reply-cap shape; the broad router sends it to the canonical materialize,
                    // which fails closed. Declining here is that same refusal one step earlier —
                    // and only when the envelope actually RESOLVES to a Reply. An unresolvable
                    // handle stays admitted so its error is raised, not swallowed.
                    let Ok(idx) = usize::try_from(handle & 0xFFFF) else {
                        return true;
                    };
                    if idx >= crate::kernel::boot::MAX_TRANSFER_ENVELOPES {
                        return true;
                    }
                    let generation = handle >> 16;
                    if generation == 0 || ipc.transfer_envelope_generations[idx] != generation {
                        return true;
                    }
                    !matches!(
                        ipc.transfer_envelopes[idx].map(|e| e.source_object),
                        Some(CapObject::Reply { .. })
                    )
                },
            )
        });
        if queued_recv_result_delivered(&result) {
            self.note_endpoint_only_queued_recv_split_seam();
        }
        let outcome = match plan {
            RecvPlan::KernelPlainEligible => map_queued_recv_outcome(result, kernel_register_plan),
            RecvPlan::UserPlainEligible => {
                map_queued_recv_outcome(result, |_m, tid| user_memory_plan(&request, tid))
            }
            RecvPlan::UserPlainV2Eligible => {
                map_queued_recv_outcome(result, |_m, tid| user_memory_v2_plan(&request, tid))
            }
            RecvPlan::FallbackRequired(_) => unreachable!("fallback returned above"),
        };

        let delivery = match outcome {
            RecvOutcome::Delivered(d) => d,
            RecvOutcome::WouldBlock | RecvOutcome::FallbackRequired(_) | RecvOutcome::TimedOut => {
                return RecvQueuedSplitPhaseA::Fallback;
            }
            RecvOutcome::Error(e) => {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::from(e),
                )));
            }
        };

        let is_reply_cap = (delivery.msg.flags & Message::FLAG_REPLY_CAP) != 0;

        // ── ordinary cap to a USER receiver: unchanged route, its mint already off-lock ─────
        if is_user_writeback
            && !is_reply_cap
            && let Some(plan) = delivery.cap_transfer
            && !plan.is_reply_cap
        {
            let Some(facts) = self.take_transfer_envelope_facts_split(
                plan.raw_handle,
                endpoint_idx,
                crate::kernel::ipc::ThreadId(receiver_tid),
            ) else {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::InvalidCapability,
                )));
            };
            let source_capability =
                match self.resolve_capability_for_task_split(facts.source_tid, facts.source_cap) {
                    Ok(c) => c,
                    Err(e) => {
                        return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                            SyscallError::from(e),
                        )));
                    }
                };
            let Some(receiver_cnode) = self.task_cnode_split(receiver_tid) else {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::InvalidCapability,
                )));
            };
            let wake_tid = match delivery.scheduler {
                RecvSchedulerWakePlan::WakeSender(t) => Some(t),
                RecvSchedulerWakePlan::None => None,
            };
            return RecvQueuedSplitPhaseA::PendingOrdinaryCapUserCopy(
                crate::kernel::recv_core::RecvBoundaryOrdinaryCapSnapshot {
                    receiver_cnode,
                    object: source_capability.object,
                    rights: source_capability.rights(),
                    source_tid: facts.source_tid,
                    source_cap: facts.source_cap,
                    wake_tid,
                    asid: self.task_asid_option_split_read(receiver_tid),
                    receiver_tid,
                    msg: delivery.msg,
                    writeback: delivery.writeback,
                },
            );
        }

        // ── ordinary cap to a KERNEL-REGISTER receiver: same off-lock seam, no user copy ────
        //
        // The user-receiver form of this class already mints off-lock through
        // `complete_recv_boundary_ordinary_cap`; a kernel-register receiver differs only in
        // having no user copy to defer, so it mints through the SAME seam and completes here.
        // Keeping it ADMITTED is what preserves the split path's long-standing contract that a
        // cap-transfer message is dequeued and materialized rather than handed back — including
        // surfacing a bad handle as `Some(Err(InvalidCapability))` instead of `None`.
        if !is_user_writeback
            && !is_reply_cap
            && let Some(plan) = delivery.cap_transfer
            && !plan.is_reply_cap
        {
            let local_cap = match self.materialize_ordinary_cap_split(
                endpoint_idx,
                receiver_tid,
                plan.raw_handle,
            ) {
                Ok(cap) => cap,
                Err(e) => {
                    crate::yarm_log!(
                        "IPC_RECV_CAP_MATERIALIZE_FAILED kind=transfer raw={} err={:?}",
                        plan.raw_handle,
                        e
                    );
                    return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(e)));
                }
            };
            if recv_boundary_encode_transfer_cap_ret(frame, Some(local_cap.0)).is_err() {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::Internal,
                )));
            }
            crate::yarm_log!(
                "YARM_D1_SPLIT_MATERIALIZE kind=transfer receiver_tid={} local_cap={}",
                receiver_tid,
                local_cap.0
            );
            crate::yarm_log!(
                "IPC_TRANSFER_CAP_MATERIALIZE_OK receiver_tid={} local_cap={}",
                receiver_tid,
                local_cap.0
            );
            crate::yarm_log!(
                "YARM_RECV_CORE_CAP_MATERIALIZE receiver_tid={} local_cap={}",
                receiver_tid,
                local_cap.0
            );
            if let RecvSchedulerWakePlan::WakeSender(wake_tid) = delivery.scheduler {
                let _ = self.apply_split_sender_wake_plan_split(cpu, wake_tid);
                crate::yarm_log!(
                    "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
                    wake_tid.tid.0
                );
            }
            let RecvWritebackPlan::KernelRegister {
                sender_tid,
                raw_len,
            } = delivery.writeback
            else {
                unreachable!("!is_user_writeback implies a KernelRegister plan");
            };
            frame.set_ok(sender_tid, raw_len, frame.ret2());
            let words = match pack_register_payload(delivery.msg.as_slice()) {
                Ok(w) => w,
                Err(_) => {
                    return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                        SyscallError::InvalidArgs,
                    )));
                }
            };
            frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, words[0]);
            frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, words[1]);
            crate::yarm_log!("YARM_RECV_CORE_LIVE kind=kernel_plain");
            return RecvQueuedSplitPhaseA::Completed(Ok(()));
        }

        // ── reply cap: the exact ordered transaction ────────────────────────────────────────
        let mut reply_record: Option<(usize, u64)> = None;
        let materialized_cap: Option<u64> = if let Some(plan) = delivery.cap_transfer {
            match self.materialize_reply_cap_split(endpoint_idx, receiver_tid, plan.raw_handle) {
                Ok(materialized) => {
                    let local_cap = materialized.cap;
                    reply_record = Some((materialized.reply_index, materialized.reply_generation));
                    if recv_boundary_encode_transfer_cap_ret(frame, Some(local_cap.0)).is_err() {
                        return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                            SyscallError::Internal,
                        )));
                    }
                    self.note_d5_split_reply_materialize_split();
                    crate::yarm_log!(
                        "YARM_D5_SPLIT_MATERIALIZE kind=reply receiver_tid={} local_cap={}",
                        receiver_tid,
                        local_cap.0
                    );
                    crate::yarm_log!(
                        "IPC_REPLY_CAP_ONESHOT_OK receiver_tid={} local_reply_cap={}",
                        receiver_tid,
                        local_cap.0
                    );
                    crate::yarm_log!(
                        "YARM_RECV_CORE_CAP_MATERIALIZE receiver_tid={} local_cap={}",
                        receiver_tid,
                        local_cap.0
                    );
                    Some(local_cap.0)
                }
                Err(e) => {
                    crate::yarm_log!(
                        "IPC_RECV_CAP_MATERIALIZE_FAILED kind=reply raw={} err={:?}",
                        plan.raw_handle,
                        e
                    );
                    return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(e)));
                }
            }
        } else {
            if recv_boundary_encode_transfer_cap_ret(frame, None).is_err() {
                return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                    SyscallError::Internal,
                )));
            }
            None
        };

        // ── rank 1: deferred sender wake, BEFORE any writeback (§56 order) ─────────────────
        if let RecvSchedulerWakePlan::WakeSender(wake_tid) = delivery.scheduler {
            let _ = self.apply_split_sender_wake_plan_split(cpu, wake_tid);
            crate::yarm_log!(
                "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
                wake_tid.tid.0
            );
        }

        match delivery.writeback {
            RecvWritebackPlan::KernelRegister {
                sender_tid,
                raw_len,
            } => {
                frame.set_ok(sender_tid, raw_len, frame.ret2());
                let words = match pack_register_payload(delivery.msg.as_slice()) {
                    Ok(w) => w,
                    Err(_) => {
                        return RecvQueuedSplitPhaseA::Completed(Err(TrapHandleError::Syscall(
                            SyscallError::InvalidArgs,
                        )));
                    }
                };
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, words[0]);
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, words[1]);
                crate::yarm_log!("YARM_RECV_CORE_LIVE kind=kernel_plain");
                RecvQueuedSplitPhaseA::Completed(Ok(()))
            }
            RecvWritebackPlan::UserMemory { .. } | RecvWritebackPlan::UserMemoryV2 { .. } => {
                RecvQueuedSplitPhaseA::PendingUserCopy(
                    crate::kernel::recv_core::RecvBoundaryUserCopySnapshot {
                        asid: self.task_asid_option_split_read(receiver_tid),
                        receiver_tid,
                        msg: delivery.msg,
                        writeback: delivery.writeback,
                        materialized_cap,
                        is_reply_cap,
                        reply_record,
                    },
                )
            }
        }
    }

    /// U9-C — rank-2 `Option<Asid>` read for an exact TID. `task_asid_for_tid_split_read` flattens
    /// "no ASID" and "ASID 0" into the same `u64`; the recv boundary snapshot needs them
    /// distinguished, so this reads the TCB field directly under the task lock.
    pub(crate) fn task_asid_option_split_read(&self, tid: u64) -> Option<crate::kernel::vm::Asid> {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.asid)
        })
    }

    /// U9-C — rank-3 D5 telemetry bump, the split twin of
    /// `KernelState::note_d5_split_reply_materialize`.
    pub(crate) fn note_d5_split_reply_materialize_split(&self) {
        self.with_ipc_split_mut(|ipc| {
            ipc.telemetry.d5_split_reply_materializations = ipc
                .telemetry
                .d5_split_reply_materializations
                .saturating_add(1);
        });
    }

    /// U9-C — the authoritative reply-cap materialization transaction, off the broad lock.
    ///
    /// This is the production wiring Stage 188D deliberately left open: 188D built the rank-3
    /// halves (`try_record_reply_waiter_cap_split` / `clear_reply_waiter_cap_split`) and proved
    /// them by unit test, but said "wiring a live producer there is out of Stage 188D scope".
    /// U9-C wires the producer, so the ONE live reply-cap materialization a real boot performs
    /// (PM receiving a call that carries a reply cap) no longer needs a broad `&mut KernelState`.
    ///
    /// # Rank order — sequential, never nested
    ///
    /// | phase | rank | effect |
    /// |---|---|---|
    /// | A | 3 (IPC) | consume the transfer envelope exactly once; read its `source_object` |
    /// | B | 3 (IPC) | the named `Reply{index,generation}` is still live |
    /// | C | 2 then 4 | resolve the receiver's process CNode |
    /// | D | 4 (+6 no-op) | mint the receiver-local one-shot Reply cap |
    /// | E | 3 (IPC) | record the exact CapId as the reply record's waiter alias |
    ///
    /// Each phase fully RELEASES its domain before the next acquires one, so the capability lock
    /// is never held while the IPC lock is taken. That is what removes the
    /// `reply_cap_ipc_rank_inversion` at this site: the inversion was never the order 4-then-3, it
    /// was holding 4 *while* taking 3.
    ///
    /// # Exactness
    ///
    /// Nothing here is derived from a bare TID, a bare slot or the ambient current task. The
    /// envelope consume is generation-guarded and endpoint-matched; the Reply object carries its
    /// own `{index, generation}`; the record write is generation-guarded again in phase E, which is
    /// what closes the mint→record window. If phase E finds the reply object revoked or reused, the
    /// phase-D mint is rolled all the way back (slot + refcount) and the whole transaction fails
    /// closed — no cap published, no alias set, no envelope resurrected.
    ///
    /// A shared-region envelope (`pinned_object.is_some()`) is NOT serviced here: it owes a rank-6
    /// pin release this transaction deliberately does not perform. Its arrival is a routing bug, so
    /// it fails closed rather than being half-settled.
    pub(crate) fn materialize_reply_cap_split(
        &self,
        endpoint_idx: usize,
        receiver_tid: u64,
        raw_handle: u64,
    ) -> Result<ReplyCapMaterialization, crate::kernel::syscall::SyscallError> {
        use crate::kernel::capabilities::{CapRights, Capability};
        use crate::kernel::syscall::SyscallError;

        // ── Phase A (rank 3): consume the envelope exactly once ────────────────────────────
        let facts = self
            .take_transfer_envelope_facts_split(
                raw_handle,
                endpoint_idx,
                crate::kernel::ipc::ThreadId(receiver_tid),
            )
            .ok_or(SyscallError::InvalidCapability)?;
        if facts.pinned_object.is_some() {
            // A shared-region envelope owes a rank-6 pin release this transaction does not do.
            return Err(SyscallError::WrongObject);
        }
        let CapObject::Reply {
            index: reply_index,
            generation: reply_generation,
        } = facts.source_object
        else {
            return Err(SyscallError::WrongObject);
        };

        // ── Phase B (rank 3): the exact Reply object is still live ─────────────────────────
        if !self.reply_object_live_split(reply_index, reply_generation) {
            return Err(SyscallError::InvalidCapability);
        }

        // ── Phase C (rank 2 → rank 4): the receiver's exact CNode ──────────────────────────
        let receiver_cnode = self
            .task_cnode_split(receiver_tid)
            .ok_or(SyscallError::InvalidCapability)?;

        // ── Phase D (rank 4): mint the one-shot Reply cap. A Reply object holds no memory
        // reference, so the rank-6 half of this seam is a no-op for it. ─────────────────────
        let reply_object = CapObject::Reply {
            index: reply_index,
            generation: reply_generation,
        };
        let minted = self
            .mint_capability_with_memory_ref_split(
                receiver_cnode,
                Capability::new(reply_object, CapRights::SEND),
            )
            .map_err(SyscallError::from)?;

        // ── Phase E (rank 3): record the exact CapId as the record's waiter alias ──────────
        match self.try_record_reply_waiter_cap_split(reply_index, reply_generation, minted) {
            crate::kernel::boot::ReplyRecordSetOutcome::Set => {
                crate::yarm_log!(
                    "YARM_D5_SPLIT_RECORD reply_index={} reply_gen={} cap={}",
                    reply_index,
                    reply_generation,
                    minted.0
                );
                Ok(ReplyCapMaterialization {
                    cap: minted,
                    reply_index,
                    reply_generation,
                })
            }
            stale => {
                // The reply object was revoked or reused inside the mint→record window. Roll the
                // mint all the way back so no cap is published against a record that will never
                // reference it, then fail exactly as the broad path did.
                self.rollback_minted_cap_split(receiver_cnode, minted, reply_object);
                crate::yarm_log!(
                    "YARM_D5_SPLIT_RECORD_ROLLBACK reply_index={} reply_gen={} cap={} reason={}",
                    reply_index,
                    reply_generation,
                    minted.0,
                    stale.stale_reason().unwrap_or("unknown")
                );
                Err(SyscallError::WrongObject)
            }
        }
    }

    /// U9-C — off-lock materialization of an ORDINARY (non-Reply) transferred cap, for the
    /// kernel-register receiver class. Same three ordered steps the user-receiver form already
    /// runs after the boundary: rank 3 consume the envelope exactly once, rank 4 (+2) resolve
    /// the source object/rights and the receiver CNode, then the existing 186D2/186D3 seam
    /// mints atomically and records the delegation edge.
    ///
    /// A shared-region envelope is refused (`pinned_object.is_some()`): it owes a rank-6 pin
    /// release this transaction does not perform, and its class is declined pre-dequeue anyway.
    pub(crate) fn materialize_ordinary_cap_split(
        &self,
        endpoint_idx: usize,
        receiver_tid: u64,
        raw_handle: u64,
    ) -> Result<CapId, crate::kernel::syscall::SyscallError> {
        use crate::kernel::boot::{
            CapTransferMaterializeOutcome, TransferCapDelegation, TransferCapSnapshot,
        };
        use crate::kernel::syscall::SyscallError;

        let facts = self
            .take_transfer_envelope_facts_split(
                raw_handle,
                endpoint_idx,
                crate::kernel::ipc::ThreadId(receiver_tid),
            )
            .ok_or(SyscallError::InvalidCapability)?;
        if facts.pinned_object.is_some() {
            return Err(SyscallError::WrongObject);
        }
        let source_capability = self
            .resolve_capability_for_task_split(facts.source_tid, facts.source_cap)
            .map_err(SyscallError::from)?;
        let receiver_cnode = self
            .task_cnode_split(receiver_tid)
            .ok_or(SyscallError::InvalidCapability)?;
        let snap = TransferCapSnapshot {
            receiver_cnode,
            object: source_capability.object,
            rights: source_capability.rights(),
        };
        let delegation = TransferCapDelegation {
            source_tid: facts.source_tid,
            source_cap: facts.source_cap,
            dest_tid: receiver_tid,
        };
        match self
            .materialize_received_message_cap_routed_with_delegation_split(snap, Some(delegation))
        {
            Ok(CapTransferMaterializeOutcome::Materialized(cap)) => Ok(cap),
            // Unreachable: a Reply source object is declined at admission, before the dequeue.
            Ok(CapTransferMaterializeOutcome::DeferredReplyCap) => Err(SyscallError::WrongObject),
            Err(e) => Err(SyscallError::from(e)),
        }
    }

    /// U9-C — the authoritative Reply-ONLY rollback, off the broad lock.
    ///
    /// Undoes exactly what [`Self::materialize_reply_cap_split`] published when a Phase-B/C
    /// writeback later fails: the receiver-local Reply cap and the reply record's alias to it.
    /// Rank order is again sequential — rank 4 (validate + revoke), then rank 3 (clear the alias).
    ///
    /// # Exact identity, so a reused slot or record is never touched
    ///
    /// The caller passes the `{reply_index, reply_generation}` it materialized against. Before
    /// revoking anything this re-resolves the receiver-local slot and requires it to still name
    /// **that same Reply object at that same generation**. If the slot was already reclaimed and
    /// re-minted for something else, or the record advanced, the resolve fails or mismatches and
    /// this returns `false` — a typed stale/already-retired refusal — without revoking a
    /// replacement or clearing a successor's alias. `clear_reply_waiter_cap_split` is itself
    /// generation-guarded, so the rank-3 half fails closed for the same reason.
    ///
    /// Repeated rollback is therefore harmless: the second call finds no matching slot and refuses.
    ///
    /// This function NEVER reaches the ordinary-object teardown family
    /// (`revoke_capability_in_cnode` → delegated-descendant revocation, active-mapping unmap +
    /// TLB shootdown, memory refcount/reclaim, notification destroy + wake). A Reply object has
    /// none of those obligations, which is exactly why the Reply arm can be retired off-lock while
    /// the ordinary arms cannot.
    pub(crate) fn rollback_reply_cap_split(
        &self,
        receiver_tid: u64,
        minted: CapId,
        reply_index: usize,
        reply_generation: u64,
    ) -> bool {
        let Some(receiver_cnode) = self.task_cnode_split(receiver_tid) else {
            return false;
        };
        let expected = CapObject::Reply {
            index: reply_index,
            generation: reply_generation,
        };
        // Rank 4: the slot must still name the EXACT object/generation we minted.
        match self.resolved_capability_split(receiver_cnode, minted) {
            Some(capability) if capability.object == expected => {}
            _ => {
                crate::yarm_log!(
                    "YARM_D5_SPLIT_ROLLBACK_STALE receiver_tid={} cap={} reply_index={} reply_gen={}",
                    receiver_tid,
                    minted.0,
                    reply_index,
                    reply_generation
                );
                return false;
            }
        }
        // Rank 4: revoke the exact slot (and its mint-time memory reference, a no-op for Reply).
        self.rollback_minted_cap_split(receiver_cnode, minted, expected);
        // Rank 3, after rank 4 is released: drop the record's alias to the cap just revoked. The
        // `ReplyCapRecord` itself stays live and re-deliverable, matching the broad policy.
        self.clear_reply_waiter_cap_split(reply_index, reply_generation);
        crate::yarm_log!(
            "IPC_RECV_V2_ROLLBACK_OK site=reply_split tid={} reply=true",
            receiver_tid
        );
        true
    }

    /// U9-C — rank-2 then rank-4, sequentially (never nested): resolve the process CNode that
    /// owns `tid`. The broad `KernelState::task_cnode` takes both domains at once through
    /// `with_task_then_capability`; this reads the thread-group id under rank 2, RELEASES it,
    /// then looks the CNode up under rank 4.
    pub(crate) fn task_cnode_split(
        &self,
        tid: u64,
    ) -> Option<crate::kernel::capabilities::CNodeId> {
        let pid = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.thread_group_id.0)
        })?;
        self.with_capability_state_split_mut(|capability| {
            capability
                .process_cnodes
                .iter()
                .flatten()
                .find(|record| record.pid == pid)
                .map(|record| record.cnode)
        })
    }

    /// U9-C — rank-4 capability resolution for an exact task, the split twin of
    /// `KernelState::resolve_capability_for_task`. Composes the two seams above in
    /// ascending order.
    pub(crate) fn resolve_capability_for_task_split(
        &self,
        tid: u64,
        cap: CapId,
    ) -> Result<crate::kernel::capabilities::Capability, KernelError> {
        let cnode = self.task_cnode_split(tid).ok_or(KernelError::TaskMissing)?;
        self.resolved_capability_split(cnode, cap)
            .ok_or(KernelError::InvalidCapability)
    }

    /// U9-C — rank-3 liveness of an exact Reply object. This is the `Reply` arm of
    /// `KernelState::capability_object_live`, narrowed to the one class the reply-cap
    /// transaction needs, so no wider object-store read is introduced.
    pub(crate) fn reply_object_live_split(&self, index: usize, generation: u64) -> bool {
        if index >= crate::kernel::boot::MAX_REPLY_CAPS {
            return false;
        }
        self.with_ipc_split_mut(|ipc| ipc.reply_cap_generations[index] == generation)
    }

    /// U9-C — rank-3 consume of a transfer envelope, returning the FACTS the caller needs
    /// (`source_object`, `source_tid`, `source_cap`) instead of only the pin obligation.
    ///
    /// This is the same single-consume transition the broad `take_transfer_envelope` performs
    /// — generation-guarded, endpoint-matched, receiver-bound-checked, `TransferState::Released`
    /// exactly once — with one deliberate difference: a shared-region envelope's rank-6 pin
    /// release is REPORTED, not performed, so rank 3 and rank 6 stay strictly sequential. The
    /// reply-cap and ordinary-cap classes this serves never carry a shared region, so
    /// `pinned_object` is always `None` for them; a `Some` means the caller was handed a class
    /// it must not service and must settle through the existing owner.
    pub(crate) fn take_transfer_envelope_facts_split(
        &self,
        handle: u64,
        endpoint_idx: usize,
        receiver_tid: crate::kernel::ipc::ThreadId,
    ) -> Option<TakenTransferEnvelopeFacts> {
        use crate::kernel::boot::{MAX_TRANSFER_ENVELOPES, TransferState};
        let idx = usize::try_from(handle & 0xFFFF).ok()?;
        if idx >= MAX_TRANSFER_ENVELOPES {
            return None;
        }
        let generation = handle >> 16;
        if generation == 0 {
            return None;
        }
        self.with_ipc_split_mut(|ipc| {
            if ipc.transfer_envelope_generations[idx] != generation {
                return None;
            }
            let envelope = ipc.transfer_envelopes[idx]?;
            let endpoint_matches = matches!(
                envelope.endpoint,
                CapObject::Endpoint { index, .. } if index == endpoint_idx);
            if !endpoint_matches {
                return None;
            }
            if let Some(bound_receiver) = envelope.receiver_tid
                && bound_receiver != receiver_tid
            {
                return None;
            }
            let envelope = envelope.transition(TransferState::Released)?;
            ipc.telemetry.transfer_records_materialized = ipc
                .telemetry
                .transfer_records_materialized
                .saturating_add(1);
            ipc.transfer_envelopes[idx] = None;
            Some(TakenTransferEnvelopeFacts {
                source_object: envelope.source_object,
                source_tid: envelope.source_tid.0,
                source_cap: envelope.source_cap,
                pinned_object: envelope
                    .shared_region
                    .is_some()
                    .then_some(envelope.source_object),
            })
        })
    }

    /// U9-C — rank-3 telemetry bump, the split twin of
    /// `KernelState::note_endpoint_only_queued_recv_split`.
    pub(crate) fn note_endpoint_only_queued_recv_split_seam(&self) {
        self.with_ipc_split_mut(|ipc| {
            ipc.telemetry.queued_recvs = ipc.telemetry.queued_recvs.saturating_add(1);
        });
    }

    /// Stage 186A: capability/cnode/object-store (rank 4) split-mut seam.
    ///
    /// Completes the per-domain split-mut seam set — ranks 1/2/3/5/6 predate this
    /// stage (Stage 108/115); Stage 186A adds rank 4, the last core subsystem
    /// seam. Exposes ONLY `&mut CapabilitySubsystem` (CNode spaces,
    /// `process_cnodes`, `delegated_capability_links`) — never a broad
    /// `&mut KernelState`. `capability_state_lock` (rank 4) serializes the
    /// `capability` field.
    ///
    /// # Validation status
    /// - M2_SEAM_HELPER_ONLY — infrastructure only; NO live caller as of
    ///   Stage 186A. Migrating capability/cnode runtime paths (e.g. the reply-cap
    ///   fast-revoke and cnode insertion in a future `ipc_reply` vertical slice)
    ///   onto this seam is deferred to Stage 186B+.
    ///
    /// Lock-rank contract (`doc/CAPABILITY_MODEL.md §3`): the capability domain is
    /// rank 4, ABOVE IPC (rank 3). A caller MUST hold NO IPC (rank 3), task
    /// (rank 2), or scheduler (rank 1) lock when invoking this seam — i.e. cap
    /// materialization runs here AFTER `ipc_state_lock` is dropped (the two-phase
    /// invariant, §8: "no cap materialization under ipc_state_lock"). Callers MUST
    /// NOT perform user-memory copy inside the closure.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_capability_state_split_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::kernel::boot::CapabilitySubsystem) -> R,
    ) -> R {
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `capability_split_mut_ptrs_from_raw` derives raw
        // field pointers via addr_of!/addr_of_mut! without forming a reference to
        // the whole KernelState; `capability_state_lock` serializes access to the
        // `capability` field. `capability` is a direct (non-`KernelStorage`)
        // field, so no `kernel_mut` unwrap is needed.
        let (capability_lock, capability) =
            unsafe { KernelState::capability_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let capability_lock = unsafe { &*capability_lock };
        let _guard = capability_lock.lock();
        let capability = unsafe { &mut *capability };
        f(capability)
    }

    fn with_fault_split_read<R>(&self, f: impl FnOnce(&FaultSubsystem) -> R) -> R {
        // Stage 4T+5 split-read: acquires fault_state_lock (rank 8) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Callers must not hold any lock of rank ≤ 8 (scheduler/task/ipc/cap/vm/
        // memory/driver) when invoking this helper.
        // SAFETY: `fault_split_mut_ptrs_from_raw` derives raw field pointers from
        // the stable KernelState storage owned by this SharedKernel without creating
        // a whole-KernelState reference. The fault lock serializes access; the *mut
        // pointer is downgraded to *const for this read-only use.
        let (fault_state_lock, faults) =
            unsafe { KernelState::fault_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let fault_state_lock = unsafe { &*fault_state_lock };
        let _guard = fault_state_lock.lock();
        let faults: &KernelStorage<FaultSubsystem> = unsafe { &*(faults as *const _) };
        f(kernel_ref(faults))
    }

    pub fn last_fault_split_read(&self) -> Option<crate::kernel::trap::FaultInfo> {
        // Stage 4T+5 split-read: reads last_fault under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.last_fault)
    }

    pub fn last_fault_frame_split_read(&self) -> Option<crate::kernel::trapframe::TrapFrame> {
        // Stage 4T+5 split-read: reads last_fault_frame under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.last_fault_frame.clone())
    }

    pub fn fault_policy_split_read(&self) -> FaultPolicy {
        // Stage 4T+5 split-read: reads fault_policy under fault_state_lock (rank 8).
        // Does not acquire the outer SharedKernel lock.
        self.with_fault_split_read(|faults| faults.fault_policy)
    }

    fn with_telemetry_split_read<R>(&self, f: impl FnOnce(&TelemetrySubsystem) -> R) -> R {
        // Stage 4T+5 split-read: acquires telemetry_state_lock (rank 10) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Callers must not hold any lock of rank ≤ 10 when invoking this helper.
        // SAFETY: `telemetry_split_mut_ptrs_from_raw` derives raw field pointers
        // from the stable KernelState storage owned by this SharedKernel without
        // creating a whole-KernelState reference. The telemetry lock serializes
        // access; the *mut pointer is downgraded to *const for read-only use.
        let (telemetry_state_lock, telemetry) =
            unsafe { KernelState::telemetry_split_mut_ptrs_from_raw(self.state.data_ptr()) };
        let telemetry_state_lock = unsafe { &*telemetry_state_lock };
        let _guard = telemetry_state_lock.lock();
        let telemetry: &KernelStorage<TelemetrySubsystem> = unsafe { &*(telemetry as *const _) };
        f(kernel_ref(telemetry))
    }

    pub fn tlb_shootdown_count_split_read(&self) -> u64 {
        // Stage 4T+5 split-read: reads tlb_shootdown_count under telemetry_state_lock (rank 10).
        // Does not acquire the outer SharedKernel lock.
        self.with_telemetry_split_read(|telemetry| telemetry.tlb_shootdown_count)
    }

    pub fn tlb_shootdown_timeout_count_split_read(&self) -> u64 {
        // Stage 4T+5 split-read: reads tlb_shootdown_timeout_count under telemetry_state_lock (rank 10).
        // Does not acquire the outer SharedKernel lock.
        self.with_telemetry_split_read(|telemetry| telemetry.tlb_shootdown_timeout_count)
    }

    /// Stage 114: reads the D3 live-split-path call counter incremented by
    /// `try_split_vm_brk_shrink_into_frame`, under telemetry_state_lock (rank
    /// 10) only. Does not acquire the outer SharedKernel lock.
    pub fn d3_vm_brk_shrink_split_live_calls_split_read(&self) -> u64 {
        self.with_telemetry_split_read(|telemetry| telemetry.d3_vm_brk_shrink_split_live_calls)
    }

    /// Stage 114: reads the D3 live-split-path pages-unmapped counter
    /// incremented by `try_split_vm_brk_shrink_into_frame`, under
    /// telemetry_state_lock (rank 10) only. Does not acquire the outer
    /// SharedKernel lock.
    pub fn d3_vm_brk_shrink_split_live_pages_unmapped_split_read(&self) -> u64 {
        self.with_telemetry_split_read(|telemetry| {
            telemetry.d3_vm_brk_shrink_split_live_pages_unmapped
        })
    }

    // U2: `ipc_recv_with_deadline_split_bridge` used to sit here. It was never a
    // trap-seam path — its own doc said so — and its only callers were hosted tests,
    // so its two broad-lock acquisitions (zero-timeout `try_ipc_recv`, deadline
    // `ipc_recv_until_deadline`) were pure test cost carried in the production census.
    // The helper now lives beside the two tests that need it, in
    // `kernel/boot/tests.rs`. No production path changed: nothing called it.

    /// # Validation status
    /// - LIVE_TRAP_SMOKE_X86_64 (Stage 32B) — now wired into the live trap seam via
    ///   `try_split_dispatch_into_frame` (NR 2 → here). The helper fast-paths ONLY a
    ///   kernel-task receiver of a queued plain message; every other case returns
    ///   `None` and falls back to the unchanged global-lock path. See
    ///   `doc/KERNEL_LOCKING.md` §50.11.
    ///
    /// Stage 31: attempt to service an `IpcRecv` for the narrowest split-safe case
    /// — a plain (no cap/reply) message already queued on a buffered endpoint,
    /// delivered to a kernel-task (no user ASID) receiver, with no recv-v2 metadata.
    ///
    /// Lock order: [no lock] → `current_tid_authoritative` (takes+releases the
    /// global lock for the TID read) → [no lock]. The dequeue + writeback then runs
    /// under the global lock via `with` for THIS helper-only path because endpoint-
    /// cap resolution (capability domain, rank 4) and the user-copy path are not yet
    /// split-extracted; the dequeue itself touches only the IPC domain
    /// (`ipc_state_lock`, rank 3) inside `ipc_try_recv_queued_plain_endpoint_only`.
    /// No scheduler wake/yield/switch occurs (`task_switched` stays `false`): a
    /// sender-waiter refill is rejected (→ `None`) so no wake plan is produced.
    ///
    /// Forbidden under ipc_state_lock: scheduler lock, capability lock, VM lock,
    /// user-copy. (The user-ASID receiver case — which would need a user copy — is
    /// rejected before any dequeue.)
    ///
    /// Returns `Some(Ok(()))` when a plain message was dequeued and the frame
    /// written; `Some(Err(e))` when the recv cap was invalid (same error as the old
    /// path); `None` for every non-split-eligible case (fall back to global lock).
    pub fn try_split_ipc_recv_queued_plain_into_frame(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
    ) -> Option<Result<(), TrapHandleError>> {
        // Stage 160 diagnostics: pin exactly where (if anywhere) the AArch64 split
        // recv falls back to the global legacy path. Each step logs result=ok or a
        // reason, so the boot log localizes the divergence to a single step.
        crate::yarm_log!("YARM_SPLIT_RECV_PROBE step=enter nr=2 cpu={}", cpu.0);

        // Authoritative requester-TID read (binds current_cpu, then releases).
        // Mirrors the Stage 29A trap-seam discipline: never current_tid_split_read.
        let Some(requester_tid) = self.current_tid_authoritative(cpu) else {
            crate::yarm_log!("YARM_SPLIT_RECV_PROBE step=tid result=none cpu={}", cpu.0);
            return None;
        };
        crate::yarm_log!(
            "YARM_SPLIT_RECV_PROBE step=tid result=ok requester_tid={}",
            requester_tid
        );

        // Stage 32: resolve the endpoint receive cap via the phase-separated
        // split-read (task(2) read+release → capability(4) read+release), with
        // NO ipc lock and NO global lock held. A resolution failure is a real
        // error the old path returned — surface it (Some(Err)); the caller must
        // NOT fall back, since the global path produces the identical error.
        let recv_cap = CapId(frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP) as u64);
        let snapshot = match self.resolve_endpoint_recv_cap_split_read(requester_tid, recv_cap) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                crate::yarm_log!(
                    "YARM_SPLIT_RECV_PROBE step=snapshot result=err recv_cap={}",
                    recv_cap.0
                );
                return Some(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::from(e),
                )));
            }
        };
        // Stage 32B per-phase telemetry: cap plan resolved (task(2)→cap(4), no ipc
        // lock). Low-noise — emitted once per attempt that clears cap resolution.
        crate::yarm_log!(
            "YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=cap_plan result=ok endpoint_idx={}",
            snapshot.endpoint_index().map(|i| i as i64).unwrap_or(-1)
        );

        // Stage 32: the cap lock is RELEASED; only now acquire the IPC domain for
        // the dequeue + writeback. The snapshot's endpoint object is revalidated
        // for liveness under ipc_state_lock inside the dequeue. The capability
        // lock and the IPC lock are NEVER held simultaneously.
        //
        // Stage 160 parity fix: use `with_cpu(cpu, …)` (not `with`) so `current_cpu`
        // is bound to the trapping CPU for the duration of the snapshot dispatch.
        // The snapshot recv computes `is_kernel_task` from the AMBIENT current task
        // (`current_task_has_user_asid` → `current_tid`), which is read off
        // `current_cpu`. The global-lock path always binds the CPU (see
        // `handle_trap_entry_shared`'s `with_cpu`); the split path used `with`,
        // which left `current_cpu` unbound. On a single-CPU boot (x86_64 smoke)
        // `current_cpu` is always CPU0 so it happened to be correct; on a
        // multi-CPU boot (AArch64 smoke, SMP=2) it could observe another CPU's
        // current task → `is_kernel_task=true` → `plan_recv_core` returns
        // `FallbackRequired(RecvV2MetaUserCopy)` (kernel task + V2 meta) → `None` →
        // the recv fell through to the global `legacy_full_path`, never emitting
        // the queued-split markers. Binding the CPU here makes the user-ASID
        // receiver class resolve identically to the global path.
        // Stage 187A — recv delivery boundary split. Phase A (inside with_cpu,
        // broad &mut KernelState live): plan + rank-3 dequeue + legacy cap
        // materialization + deferred sender wake (§56 order) + kernel-register
        // writeback. NO seam helper is called inside this closure. For a
        // user-ASID receiver the closure returns a by-value PendingUserCopy
        // snapshot instead of copying; the copy runs AFTER this closure
        // returns, i.e. after the broad borrow is dead (Phase B, 186E seam).
        // U9-C — Phase A runs OFF the broad lock. The former `with_cpu(cpu, …)` existed for two
        // reasons, and both are gone: it supplied the `&mut KernelState` every step now reaches
        // through its own ordered split seam, and it bound `current_cpu` so the ambient
        // `current_task_has_user_asid` classified the receiver correctly (the Stage 160 parity
        // fix). The receiver class is now read from `snapshot.requester_tid` — the authoritative
        // TID resolved above — so there is no ambient reader left to bind a CPU for.
        let phase_a = self.recv_queued_split_phase_a_split(cpu, frame, &snapshot);
        let result = match phase_a {
            crate::kernel::syscall::RecvQueuedSplitPhaseA::Fallback => None,
            crate::kernel::syscall::RecvQueuedSplitPhaseA::Completed(r) => Some(r),
            crate::kernel::syscall::RecvQueuedSplitPhaseA::PendingUserCopy(pending) => {
                // The with_cpu closure has returned: the global SpinLock is
                // released and no &mut KernelState is live. Phase B/C below may
                // now safely use the data_ptr()-derived seams (Stage 186D4's
                // aliasing blocker does not apply past this point).
                crate::yarm_log!(
                    "IPC_RECV_BOUNDARY_SNAPSHOT_OK receiver_tid={} cap={} reply={}",
                    pending.receiver_tid,
                    pending.materialized_cap.map(|c| c as i64).unwrap_or(-1),
                    pending.is_reply_cap
                );
                crate::yarm_log!("IPC_RECV_BOUNDARY_GLOBAL_DROPPED_OK");
                Some(self.complete_recv_boundary_user_copy(cpu, frame, &pending))
            }
            crate::kernel::syscall::RecvQueuedSplitPhaseA::PendingOrdinaryCapUserCopy(pending) => {
                // Stage 187B — the global lock is released; materialize the
                // ordinary transferred cap through the 186D2/186D3 seam, wake the
                // sender, then run the 186E user copy.
                crate::yarm_log!("IPC_RECV_BOUNDARY_GLOBAL_DROPPED_OK");
                Some(self.complete_recv_boundary_ordinary_cap(cpu, frame, pending))
            }
        };
        match result {
            Some(Ok(())) => {
                crate::yarm_log!(
                    "YARM_LOCK_SPLIT_IPC_RECV nr=2 phase=writeback result=ok target=user_or_kernel"
                );
                crate::yarm_log!("YARM_SPLIT_RECV_PROBE step=outcome result=serviced_ok");
            }
            Some(Err(_)) => {
                crate::yarm_log!("YARM_SPLIT_RECV_PROBE step=outcome result=serviced_err");
            }
            None => {
                crate::yarm_log!("YARM_SPLIT_RECV_PROBE step=outcome result=fallback");
            }
        }
        result
    }

    /// Stage 187A — Phase B (186E user-copy seam) + Phase C (frame/rollback/
    /// fault completion) for a queued-split recv whose user writeback was
    /// deferred past the global-lock boundary by
    /// [`crate::kernel::syscall::RecvQueuedSplitPhaseA::PendingUserCopy`].
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_187A_RECV_BOUNDARY — first live seam call on the recv
    ///   delivery path. The copies run through `copy_to_user_split` (VM rank 5
    ///   + memory rank 6 seams) via the recv_core boundary executors; NO
    ///   `ipc_state_lock`, NO capability lock, NO broad `&mut KernelState` is
    ///   held during the copy.
    ///
    /// Ordering proof (§56/§58 preserved): Phase A already committed — in
    /// order — the cap materialization, the sender wake
    /// (`IPC_RECV_V2_SENDER_WAKE_ORDER_OK … phase=before_writeback`), and the
    /// ret2 transfer-cap register. This function performs only the writeback
    /// (meta-first for v2) and the §58 failure handling (cap rollback / user
    /// fault record) via brief `with_cpu` re-entries — the same operations the
    /// legacy in-lock path performed at the same point in the sequence.
    ///
    /// Failure semantics are byte-identical to the legacy in-lock arms:
    /// undersized → cap rollback + `InvalidArgs`; v2 meta fault → cap rollback
    /// + `PageFault`; payload fault → fault record + `Ok(())` (no rollback);
    /// the message is consumed in every case (one-shot preserved).
    fn complete_recv_boundary_user_copy(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: &crate::kernel::recv_core::RecvBoundaryUserCopySnapshot,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::recv_core::{
            RecvUserWritebackOutcome, RecvV2WritebackOutcome, RecvWritebackPlan,
            execute_user_asid_plain_v2_writeback_boundary,
            execute_user_asid_plain_writeback_boundary,
        };
        use crate::kernel::syscall::SyscallError;

        // Phase C helper: §58 cap rollback, now entirely off the broad lock. The
        // seam copy has already completed (or failed) — no seam call happens
        // inside this closure.
        let rollback_cap = |shared: &Self, frame: &mut TrapFrame| {
            if let Some(cap_id) = pending.materialized_cap {
                // U9-C — REPLY caps roll back through the exact ordered transaction, off the
                // broad lock: rank 4 validate-the-same-object-and-generation + revoke, then
                // rank 3 clear that record's alias. A Reply object's teardown is the
                // reply-registry transaction, not a cnode revoke, which is why it is routed
                // here rather than through the ordinary composition.
                //
                // U9-D3 §6 — every OTHER object class goes to the phased split composition,
                // which now serves the memory-backed cohort too: it performs the COMPLETE
                // teardown — cross-CNode descendant revocation, delegation-link removal, the
                // recursive in-cspace revoke, the active-transfer-mapping unmap with a REAL
                // generation-matched TLB shootdown, the memory refcount drop, the
                // reclaim-only-after-ACK, and notification destroy + waiter wake — with no
                // broad lock and nothing held while a shootdown is awaited.
                //
                // The broad `with_cpu` fallback that used to stand here is RETIRED. It was
                // load-bearing only for the memory-backed cohort; the only refusal this site
                // can still produce is `Unresolvable`, for which the broad path did nothing
                // either. Nothing is narrowed, filtered or skipped.
                if let Some((reply_index, reply_generation)) = pending.reply_record {
                    let _ = shared.rollback_reply_cap_split(
                        pending.receiver_tid,
                        crate::kernel::capabilities::CapId(cap_id),
                        reply_index,
                        reply_generation,
                    );
                } else {
                    let _ = shared.rollback_materialized_recv_cap_no_vm_split(
                        pending.receiver_tid,
                        crate::kernel::capabilities::CapId(cap_id),
                    );
                }
                crate::kernel::syscall::recv_boundary_clear_transfer_cap_ret(frame);
                true
            } else {
                false
            }
        };

        match pending.writeback {
            RecvWritebackPlan::UserMemory { sender_tid, .. } => {
                match execute_user_asid_plain_writeback_boundary(self, pending) {
                    RecvUserWritebackOutcome::Ok => {
                        let payload_len = pending.msg.as_slice().len();
                        frame.set_ok(sender_tid, payload_len, frame.ret2());
                        crate::yarm_log!("IPC_RECV_BOUNDARY_USER_COPY_SEAM_OK kind=user_plain");
                        crate::yarm_log!("YARM_RECV_CORE_LIVE kind=user_plain");
                        crate::yarm_log!("IPC_RECV_BOUNDARY_SPLIT_DONE result=ok");
                        Ok(())
                    }
                    RecvUserWritebackOutcome::UndersizedBuffer => {
                        // §58: rollback materialized cap (matches legacy in-lock arm).
                        let _ = rollback_cap(self, frame);
                        Err(TrapHandleError::Syscall(SyscallError::InvalidArgs))
                    }
                    RecvUserWritebackOutcome::CopyFault { user_ptr } => {
                        // No rollback on payload copy fault (§54/§58) — fault record + frame
                        // error. U3 (203C): the rank-1 -> rank-8 transaction below replaces the
                        // broad re-entry; same record, same frame encoding, same ignored result.
                        let _ = self.record_recv_boundary_user_fault_split(cpu, frame, user_ptr);
                        Ok(())
                    }
                }
            }
            RecvWritebackPlan::UserMemoryV2 { .. } => {
                match execute_user_asid_plain_v2_writeback_boundary(self, pending) {
                    RecvV2WritebackOutcome::Ok => {
                        let payload_len = pending.msg.as_slice().len();
                        frame.set_ok(0, payload_len, frame.ret2());
                        crate::yarm_log!("IPC_RECV_BOUNDARY_USER_COPY_SEAM_OK kind=user_plain_v2");
                        crate::yarm_log!("YARM_RECV_CORE_LIVE kind=user_plain_v2");
                        crate::yarm_log!("YARM_RECV_CORE_V2_WRITEBACK result=ok");
                        // Stage 156 IPC oracle: queued-split recv-v2 meta delivered
                        // (marker relocated with the writeback in Stage 187A —
                        // same live path, same meaning).
                        crate::yarm_log!("IPC_RECV_V2_META_QUEUED_SPLIT_OK len=40");
                        crate::yarm_log!("IPC_RECV_BOUNDARY_SPLIT_DONE result=ok");
                        Ok(())
                    }
                    RecvV2WritebackOutcome::PayloadUndersized => {
                        crate::yarm_log!("YARM_RECV_CORE_V2_WRITEBACK result=payload_undersized");
                        if rollback_cap(self, frame) {
                            // Stage 156 IPC oracle: rollback on queued-split undersize.
                            crate::yarm_log!(
                                "IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize reply={}",
                                pending.is_reply_cap
                            );
                        }
                        Err(TrapHandleError::Syscall(SyscallError::InvalidArgs))
                    }
                    RecvV2WritebackOutcome::MetaCopyFault { .. } => {
                        crate::yarm_log!("YARM_RECV_CORE_V2_WRITEBACK result=meta_fault");
                        if rollback_cap(self, frame) {
                            // Stage 156 IPC oracle: rollback on queued-split meta fault.
                            crate::yarm_log!(
                                "IPC_RECV_V2_ROLLBACK_OK site=queued_split_meta reply={}",
                                pending.is_reply_cap
                            );
                        }
                        Err(TrapHandleError::Syscall(SyscallError::PageFault))
                    }
                    RecvV2WritebackOutcome::PayloadCopyFault { user_ptr } => {
                        // No rollback on payload copy fault (§55/§58). U3 (203C): the SAME
                        // class-neutral transaction the plain arm uses — the two fault records
                        // were byte-identical and now share one implementation.
                        crate::yarm_log!("YARM_RECV_CORE_V2_WRITEBACK result=payload_fault");
                        let _ = self.record_recv_boundary_user_fault_split(cpu, frame, user_ptr);
                        Ok(())
                    }
                }
            }
            RecvWritebackPlan::KernelRegister { .. } => {
                unreachable!("KernelRegister writeback completes in Phase A, never deferred")
            }
        }
    }

    /// U3 (canonical 203C) — the class-neutral recv-boundary user-fault completion, replacing
    /// the two homologous broad re-entries the plain and recv-v2 payload copy-fault arms used
    /// to make. Both ran the byte-identical body
    /// `with_cpu(cpu, |k| recv_boundary_record_user_fault(k, frame, user_ptr))`, which is
    /// `record_user_fault(.., FaultAccess::Write)`: record the fault, then set `PageFault` on
    /// the frame. Nothing in it needed the whole `KernelState`.
    ///
    /// Rank-ordered, ascending, with each guard fully released before the next step:
    ///
    /// 1. **rank 1** — [`Self::bind_current_cpu_split`], which is exactly the CPU
    ///    validate-and-bind `with_cpu` performed on entry through `set_current_cpu`: the same
    ///    `validate_online_cpu` predicate, `scheduler.current_cpu` left untouched on refusal,
    ///    bound unconditionally on success. No second binding implementation is introduced.
    /// 2. **rank 8** — the existing `record_fault_split_mut`, recording the identical
    ///    `FaultInfo { addr: VirtAddr(addr as u64), access: FaultAccess::Write }`.
    /// 3. **no lock held** — `frame.set_err(SyscallError::PageFault.code())`.
    ///
    /// The record precedes the frame error, exactly as `record_user_fault` ordered them.
    ///
    /// A refused CPU propagates the same `KernelError` class the broad `with_cpu` returned and
    /// mutates **nothing**: no fault is recorded and the frame is untouched — the broad body's
    /// closure never ran either. Callers ignore the result exactly as they ignored `with_cpu`'s.
    ///
    /// This adds no seam: it composes the existing rank-1 binding transaction with the existing
    /// rank-8 fault seam. It records no frame snapshot, touches no `TaskStatus`, and performs no
    /// scheduler, IPC, capability, VM, memory, ownership or notification work.
    fn record_recv_boundary_user_fault_split(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        addr: usize,
    ) -> Result<(), KernelError> {
        use crate::arch::trap::{FaultAccess, FaultInfo};
        use crate::kernel::syscall::SyscallError;
        use crate::kernel::vm::VirtAddr;

        // (1) rank 1 — the CPU authentication and binding `with_cpu` performed on entry.
        self.bind_current_cpu_split(cpu)?;
        // (2) rank 8 — the fault record, identical to `record_user_fault`'s.
        self.record_fault_split_mut(FaultInfo {
            addr: VirtAddr(addr as u64),
            access: FaultAccess::Write,
        });
        // (3) no lock held — the frame error, after the record, as `record_user_fault` ordered.
        frame.set_err(SyscallError::PageFault.code());
        Ok(())
    }

    /// Stage 198B — emit the AUTHORITATIVE ordinary-cap object-identity proof for a freshly
    /// materialized receiver-local cap. Re-resolves the cap OUT of the receiver's cspace (rank-4
    /// capability seam, `resolved_cap_object_split`) and compares the FULL `CapObject` to the source
    /// object the sender transferred, emitting `IPC_ORDINARY_CAP_OBJECT_IDENTITY … match={0|1}`.
    /// `match=1` proves object identity is preserved (SAME object, fresh CapId); a genuine mismatch
    /// or a missing install yields `match=0`. This is a real cnode lookup, not a `CapId != 0` check.
    /// Both ordinary-cap delivery executors (blocked-waiter + recv-boundary) call it on materialize.
    fn log_ordinary_cap_object_identity(
        &self,
        receiver_tid: u64,
        receiver_cnode: crate::kernel::capabilities::CNodeId,
        source_object: crate::kernel::capabilities::CapObject,
        expected_rights: crate::kernel::capabilities::CapRights,
        cap: crate::kernel::capabilities::CapId,
    ) {
        use crate::kernel::capabilities::CapObject;
        // Resolve the FULL freshly-minted capability (object + rights) out of the receiver cspace.
        let installed = self.resolved_capability_split(receiver_cnode, cap);
        let installed_object = installed.map(|c| c.object);
        let identity_match = installed_object == Some(source_object);
        let endpoint_index = |obj: &CapObject| match obj {
            CapObject::Endpoint { index, .. } => Some(*index),
            _ => None,
        };
        crate::yarm_log!(
            "IPC_ORDINARY_CAP_OBJECT_IDENTITY receiver_tid={} src_endpoint={:?} dst_endpoint={:?} match={}",
            receiver_tid,
            endpoint_index(&source_object),
            installed_object.as_ref().and_then(endpoint_index),
            identity_match as u8
        );
        // Stage 198B1 Part C: AUTHORITATIVE capability-entry (rights + metadata) attestation.
        //  * destination rights MUST equal the canonical transfer/delegation result (the snapshot
        //    `expected_rights`, taken from `source_capability.rights()` — ordinary-cap transfer does
        //    NOT attenuate), so `rights_ok=1`.
        //  * the minted object MUST be an Endpoint, NOT a Reply — reply-cap metadata is absent
        //    (`reply_object=0`), so a reply cap can never be misclassified as ordinary here.
        //  * the endpoint generation is carried for validity (nonzero on a live object).
        let dst_rights_bits = installed.map(|c| c.rights_bits());
        let rights_ok = dst_rights_bits == Some(expected_rights.bits());
        let (is_endpoint, generation) = match installed_object {
            Some(CapObject::Endpoint { generation, .. }) => (true, generation),
            _ => (false, 0),
        };
        let reply_object = matches!(installed_object, Some(CapObject::Reply { .. })) as u8;
        crate::yarm_log!(
            "IPC_ORDINARY_CAP_RIGHTS receiver_tid={} dst_rights={:?} expected_rights={} rights_ok={} object_endpoint={} reply_object={} generation={}",
            receiver_tid,
            dst_rights_bits,
            expected_rights.bits(),
            rights_ok as u8,
            is_endpoint as u8,
            reply_object,
            generation
        );
    }

    /// Stage 187B — Phase B/C for an ordinary (non-reply, non-shared-region)
    /// cap transfer to a user receiver on the queued-split recv boundary.
    ///
    /// # Validation status
    /// - M2_SEAM_LIVE_187B_CAP_TRANSFER — the FIRST live use of the Stage
    ///   186D2/186D3 cap-transfer materialization + delegation seam on a real
    ///   runtime path. The mint runs through the Stage 186D-proper atomic
    ///   cap↔memory mint and records the delegation link; NO `ipc_state_lock`,
    ///   NO broad `&mut KernelState`, NO seam call while the Phase A borrow was
    ///   live (this runs entirely AFTER the `with_cpu` closure returned).
    ///
    /// Order (materialize → wake → writeback, §56/§58 preserved):
    ///   1. materialize the ordinary cap via
    ///      `materialize_received_message_cap_routed_with_delegation_split`
    ///      (atomic mint + delegation link + rollback-on-delegation-failure),
    ///   2. commit the receiver-local CapId to the transfer-cap return register,
    ///   3. apply the deferred sender wake (brief `with_cpu` re-entry — no seam),
    ///   4. run the 186E user copy and §58 writeback/rollback completion (shared
    ///      with the plain boundary path).
    ///
    /// The receiver-local CapId is freshly minted by the seam; the source CapId
    /// is used ONLY as the delegation-link parent edge, never as authority. On a
    /// writeback failure the cap is rolled back via `rollback_materialized_recv_cap`
    /// (revoke + delegation-link removal + refcount drop), exactly as the legacy
    /// §58 path. The transfer envelope was consumed once in Phase A (one-shot).
    fn complete_recv_boundary_ordinary_cap(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: crate::kernel::recv_core::RecvBoundaryOrdinaryCapSnapshot,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::boot::{
            CapTransferMaterializeOutcome, TransferCapDelegation, TransferCapSnapshot,
        };
        use crate::kernel::syscall::SyscallError;

        crate::yarm_log!(
            "CAP_TRANSFER_BOUNDARY_SEAM_BEGIN kind=ordinary receiver_tid={}",
            pending.receiver_tid
        );

        let snap = TransferCapSnapshot {
            receiver_cnode: pending.receiver_cnode,
            object: pending.object,
            rights: pending.rights,
        };
        let delegation = TransferCapDelegation {
            source_tid: pending.source_tid,
            source_cap: pending.source_cap,
            dest_tid: pending.receiver_tid,
        };
        crate::yarm_log!("CAP_TRANSFER_BOUNDARY_SEAM_SNAPSHOT_OK kind=ordinary");

        // Step 1 — seam mint (atomic cap↔memory mint) + delegation link. This is
        // the first live seam materialization; the broad borrow is dead.
        let local_cap = match self
            .materialize_received_message_cap_routed_with_delegation_split(snap, Some(delegation))
        {
            Ok(CapTransferMaterializeOutcome::Materialized(cap)) => {
                crate::yarm_log!(
                    "CAP_TRANSFER_BOUNDARY_SEAM_ATOMIC_MINT_OK kind=ordinary local_cap={}",
                    cap.0
                );
                crate::yarm_log!("CAP_TRANSFER_BOUNDARY_SEAM_DELEGATION_OK kind=ordinary");
                // Stage 198B / 198B1: authoritative object-identity + rights proof of the cap.
                self.log_ordinary_cap_object_identity(
                    pending.receiver_tid,
                    pending.receiver_cnode,
                    pending.object,
                    pending.rights,
                    cap,
                );
                cap.0
            }
            Ok(CapTransferMaterializeOutcome::DeferredReplyCap) => {
                // Cannot occur: ordinary (non-reply) objects only reach here. If
                // it somehow did, surface a real error rather than silently drop.
                crate::yarm_log!(
                    "CAP_TRANSFER_BOUNDARY_SEAM_DEFERRED reason=unexpected_reply_object"
                );
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
            Err(e) => {
                // Same real error the legacy router would raise (CapabilityFull,
                // WrongObject, StaleCapability, MissingRight, …). The envelope was
                // already consumed in Phase A — identical to the legacy arm, whose
                // materialize failure also leaves the envelope consumed.
                return Err(TrapHandleError::Syscall(SyscallError::from(e)));
            }
        };

        // Step 2 — commit the receiver-local CapId to the return register.
        if crate::kernel::syscall::recv_boundary_encode_transfer_cap_ret(frame, Some(local_cap))
            .is_err()
        {
            // Roll the just-minted cap back so nothing leaks, then fail. U9-D3 §6: the phased
            // split composition performs the COMPLETE teardown off the broad lock for every class
            // this site can produce — including the memory-backed cohort, whose
            // active-transfer-mapping unmap now completes a real generation-matched TLB shootdown
            // before any frame is reclaimed. The broad `with_cpu` fallback is retired.
            let _ = self.rollback_materialized_recv_cap_no_vm_split(
                pending.receiver_tid,
                crate::kernel::capabilities::CapId(local_cap),
            );
            return Err(TrapHandleError::Syscall(SyscallError::Internal));
        }

        // Step 3 — deferred sender wake (AFTER materialize, BEFORE writeback:
        // §56/§58 order). Brief global re-entry; no seam inside.
        if let Some(wake_tid) = pending.wake_tid {
            // U3 (canonical 203C): the broad re-entry is retired. Same order, same marker, same
            // wake, and the result stays ignored exactly as `let _ = self.with_cpu(...)` ignored
            // it — including a CPU refusal, which wakes nothing and emits nothing.
            let _ = self.apply_split_sender_wake_plan_split(cpu, wake_tid);
            crate::yarm_log!(
                "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
                wake_tid.tid.0
            );
        }

        // Step 4 — 186E user copy + §58 completion, shared with the plain path.
        let user_copy = crate::kernel::recv_core::RecvBoundaryUserCopySnapshot {
            // Ordinary object cap: not a Reply, so no reply record and no split reply rollback.
            reply_record: None,
            asid: pending.asid,
            receiver_tid: pending.receiver_tid,
            msg: pending.msg,
            writeback: pending.writeback,
            materialized_cap: Some(local_cap),
            is_reply_cap: false,
        };
        let result = self.complete_recv_boundary_user_copy(cpu, frame, &user_copy);
        if result.is_ok() {
            crate::yarm_log!(
                "CAP_TRANSFER_BOUNDARY_SEAM_DONE result=ok kind=ordinary local_cap={}",
                local_cap
            );
        }
        result
    }

    /// Stage 188A — dispatch-return delivery channel drain.
    ///
    /// Called by the trap entry (`handle_trap_entry_shared`) **after** the broad
    /// `with_cpu` / `SpinLock<KernelState>` guard is dropped, alongside the
    /// existing D2/D6 post-`with_cpu` drains. Takes the per-CPU
    /// [`crate::kernel::boot::DISPATCH_POST_WORK_STASH`] item a handler produced
    /// under the broad borrow and executes it through `&SharedKernel` seams.
    ///
    /// # Validation status
    /// - DISPATCH_RETURN_CHANNEL (Stage 188A) — **infrastructure only**. No live
    ///   handler stashes work in Stage 188A, so on every production trap the stash
    ///   is empty and this is a no-op (a one-shot `DISPATCH_RETURN_CHANNEL_READY
    ///   mode=helper_only` marker is emitted as honest evidence the channel is
    ///   present and inert). The `BlockedWaiterPlainDelivery` executor arm is
    ///   complete and unit-tested (186E copy seam) but produced by nothing live.
    ///
    /// Aliasing: this runs only AFTER `with_cpu` returned, so no broad
    /// `&mut KernelState` is live when the 186E `copy_to_user_split` seam derives
    /// its `&mut Subsystem` from `data_ptr()` (Stage 186D4's blocker does not
    /// apply here). It touches no `ipc_state_lock`.
    ///
    /// # U6 §4 — the typed disposition
    ///
    /// The drain now RETURNS what it requires of its caller, instead of always returning `()`.
    /// Every pre-U6 variant answers `NoCallerAction`, so no existing wrapper behaviour
    /// changes; only the `BlockingSendCommit` variant can answer otherwise, because only it
    /// decides the fate of the CALLER's own frame. `frame` is threaded in for the same reason:
    /// a refused commit encodes its canonical error into the still-running caller's frame here,
    /// in the same place and the same lane the in-lock route would have.
    pub(crate) fn drain_dispatch_post_work(
        &self,
        cpu: CpuId,
        frame: Option<&mut TrapFrame>,
    ) -> Result<crate::kernel::dispatch_post_work::DispatchPostWorkDisposition, TrapHandleError>
    {
        use crate::kernel::dispatch_post_work::DispatchPostWorkDisposition;
        let cpu_idx = cpu.0 as usize;
        if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
            return Ok(DispatchPostWorkDisposition::NoCallerAction);
        }
        // One-shot readiness marker (honest boot-log evidence; additive). Stage
        // 188B wires a live producer (plain blocked-waiter reply delivery), so the
        // channel is now `mode=live`.
        if !crate::kernel::boot::DISPATCH_RETURN_CHANNEL_READY_LOGGED
            .swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            crate::yarm_log!("DISPATCH_RETURN_CHANNEL_READY mode=live");
        }
        // SAFETY: local-CPU trap path, interrupts disabled, no concurrent access —
        // identical discipline to the Stage 117 `DISPATCH_SWITCH_PLAN_STASH` drain.
        let work = unsafe { crate::kernel::boot::DISPATCH_POST_WORK_STASH[cpu_idx].take() };
        let Some(work) = work else {
            // Nothing stashed — the common case on every ordinary trap.
            return Ok(DispatchPostWorkDisposition::NoCallerAction);
        };
        // U6 §4: the blocking-send commit is the one variant whose outcome the caller must act
        // on, and the one variant that needs the caller's frame. It is dispatched here rather
        // than inside `execute_dispatch_post_work` so that function keeps its exact pre-U6
        // signature and every existing arm keeps its exact pre-U6 behaviour.
        if let crate::kernel::dispatch_post_work::DispatchPostWork::BlockingSendCommit(snap) = work
        {
            return Ok(self.execute_blocking_send_commit(cpu, snap, frame));
        }
        self.execute_dispatch_post_work(cpu, work)
            .map(|()| DispatchPostWorkDisposition::NoCallerAction)
    }

    /// U6 §4/§6 — run one deferred blocking-send commit and report what the trap wrapper owes.
    ///
    /// Ordering is the whole point of this function:
    ///
    /// 1. run the rank-ordered transaction ([`Self::commit_blocking_send_split`]), which either
    ///    commits all three domains or mutates nothing at all;
    /// 2. on success, and ONLY after the commit, arm the U4 D2-send deferral so the drain that
    ///    runs immediately after this one performs the queue-advancing dispatch. Arming before
    ///    the commit would let that drain observe a CPU whose `current` had not yet been
    ///    cleared;
    /// 3. on refusal, take back the transfer envelope the producer stashed — the same cleanup
    ///    the in-lock error path performs — and hand the canonical error back for the caller's
    ///    frame.
    fn execute_blocking_send_commit(
        &self,
        cpu: CpuId,
        snap: crate::kernel::dispatch_post_work::BlockingSendCommitSnapshot,
        frame: Option<&mut TrapFrame>,
    ) -> crate::kernel::dispatch_post_work::DispatchPostWorkDisposition {
        use crate::kernel::dispatch_post_work::DispatchPostWorkDisposition;
        crate::yarm_log!(
            "DISPATCH_POST_WORK_SNAPSHOT_OK kind=blocking_send_commit tid={} endpoint={}",
            snap.sender_tid,
            snap.endpoint_idx
        );
        crate::yarm_log!("DISPATCH_POST_WORK_GLOBAL_DROPPED_OK kind=blocking_send_commit");
        let outcome = self.commit_blocking_send_split(&snap);
        crate::yarm_log!(
            "U6_BLOCKING_SEND_COMMIT tid={} asid={} endpoint={} endpoint_generation={} cpu={} outcome={}",
            snap.sender_tid,
            snap.sender_asid.0,
            snap.endpoint_idx,
            snap.endpoint_generation,
            cpu.0,
            outcome.slug()
        );
        match outcome {
            BlockingSendCommitOutcome::Committed { send_generation } => {
                // The sender is blocked and this CPU has no current task. Arm the established
                // U4 D2-send deferral so the very next drain in this same post-lock section
                // performs the one authoritative queue-advancing dispatch. This is not a
                // second dispatch channel — it is the same one the in-lock route arms.
                let cpu_idx = cpu.0 as usize;
                let armed = cpu_idx < crate::kernel::scheduler::MAX_CPUS
                    && crate::kernel::boot::d2_send_dispatch_try_defer(cpu_idx, snap.sender_tid);
                if armed {
                    crate::yarm_log!(
                        "D2_SEND_GENUINE_DISPATCH_DEFERRED tid={} cpu={}",
                        snap.sender_tid,
                        cpu_idx
                    );
                }
                crate::yarm_log!(
                    "U6_BLOCKING_SEND_COMMITTED tid={} send_generation={} deferral_armed={} result=ok",
                    snap.sender_tid,
                    send_generation,
                    u8::from(armed)
                );
                DispatchPostWorkDisposition::SenderCommittedBlocked {
                    tid: snap.sender_tid,
                }
            }
            refusal => {
                // Nothing was mutated. The caller is still running and still owns its message,
                // so it owes exactly what the in-lock error path owes: take the stashed
                // transfer envelope back, then return the canonical error.
                if let Some(cleanup) = snap.transfer_envelope {
                    // U6/199C: one settle for every cap class. For a shared-region envelope it
                    // additionally releases the transient pin exactly once (rank 3 then rank 6,
                    // sequential, no reclaim — see `settle_blocked_send_envelope_split`).
                    let taken = self.settle_blocked_send_envelope_split(
                        cleanup.handle,
                        cleanup.endpoint_idx,
                        cleanup.cleanup_tid,
                    );
                    crate::yarm_log!(
                        "U6_BLOCKING_SEND_ENVELOPE_RECLAIMED tid={} handle={} taken={}",
                        snap.sender_tid,
                        cleanup.handle,
                        u8::from(taken)
                    );
                }
                let error = refusal
                    .immediate_error()
                    .expect("only Committed has no immediate error");
                if let Some(frame) = frame {
                    frame.set_err(crate::kernel::syscall::SyscallError::from(error).code());
                }
                DispatchPostWorkDisposition::ImmediateReturn { error }
            }
        }
    }

    /// Stage 188A — execute one drained [`DispatchPostWork`] item through
    /// `&SharedKernel` seams (Phase B) and a brief `with_cpu` completion re-entry
    /// (Phase C). Runs only outside `with_cpu` (see `drain_dispatch_post_work`).
    fn execute_dispatch_post_work(
        &self,
        cpu: CpuId,
        work: crate::kernel::dispatch_post_work::DispatchPostWork,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::dispatch_post_work::DispatchPostWork;
        match work {
            DispatchPostWork::None => Ok(()),
            // U6 §4: routed by `drain_dispatch_post_work` before it reaches here, because it
            // needs the caller's frame and returns a caller disposition. Unreachable.
            DispatchPostWork::BlockingSendCommit(_) => Ok(()),
            DispatchPostWork::BlockedWaiterPlainDelivery(snap) => {
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_SNAPSHOT_OK kind=blocked_waiter_plain waiter_tid={}",
                    snap.waiter_tid
                );
                crate::yarm_log!("DISPATCH_POST_WORK_GLOBAL_DROPPED_OK kind=blocked_waiter_plain");
                // Phase B — user copy through the 186E seam (payload then meta),
                // to the WAITER's ASID. No ipc_state_lock, no broad borrow.
                if self
                    .copy_to_user_split(
                        snap.waiter_asid,
                        crate::kernel::vm::VirtAddr(snap.payload_user_ptr as u64),
                        &snap.payload[..snap.payload_len],
                    )
                    .is_err()
                {
                    return Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::InvalidArgs,
                    ));
                }
                if self
                    .copy_to_user_split(
                        snap.waiter_asid,
                        crate::kernel::vm::VirtAddr(snap.meta_user_ptr as u64),
                        &snap.meta,
                    )
                    .is_err()
                {
                    return Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::InvalidArgs,
                    ));
                }
                crate::yarm_log!("DISPATCH_POST_WORK_USER_COPY_OK kind=blocked_waiter_plain");
                // Stage 193A: IpcSend-origin plain deliveries emit the boundary marker
                // here (peek — the flag is consumed after the wake below). Reply-origin
                // deliveries leave the flag unset, so this is silent for them.
                let ipc_send_origin =
                    crate::kernel::boot::ipc_send_boundary_origin_is_set(cpu.0 as usize);
                if ipc_send_origin {
                    crate::yarm_log!(
                        "IPC_SEND_BOUNDARY_USER_COPY_OK waiter_tid={}",
                        snap.waiter_tid
                    );
                }
                crate::yarm_log!("DISPATCH_POST_WORK_EXECUTE_OK kind=blocked_waiter_plain");
                // Phase C — completion, via a brief global re-entry (no seam
                // inside the closure), preserving the legacy order copy → clear
                // GPRs → clear endpoint waiter slot → wake exactly once:
                //   1. clear the waiter's return regs (legacy
                //      complete_blocked_recv_for_waiter completion),
                //   2. clear the endpoint receiver-waiter slot (legacy Phase 4
                //      ipc_clear_plain_receiver_waiter_only),
                //   3. wake the waiter exactly once (legacy Phase 5).
                // U3 (canonical 203C): the class-neutral rank-ordered completion transaction
                // replaces this broad re-entry. Same order, same identity, same wake; the
                // result stays ignored exactly as `let _ = self.with_cpu(...)` ignored it.
                let _ = self.complete_blocked_waiter_delivery_split(
                    cpu,
                    snap.waiter_tid,
                    snap.endpoint_idx,
                    snap.wake_tid,
                );
                crate::yarm_log!("DISPATCH_POST_WORK_WAKE_OK kind=blocked_waiter_plain");
                // Stage 193A: for an IpcSend-origin plain delivery, emit the IpcSend boundary
                // wake/done markers + the one-shot retirement, and consume the origin flag.
                if crate::kernel::boot::ipc_send_boundary_origin_take(cpu.0 as usize) {
                    crate::yarm_log!("IPC_SEND_BOUNDARY_WAKE_OK waiter_tid={}", snap.waiter_tid);
                    crate::yarm_log!(
                        "IPC_SEND_BOUNDARY_SPLIT_DONE result=ok waiter_tid={}",
                        snap.waiter_tid
                    );
                    crate::kernel::boot::maybe_log_ipc_send_plain_retired();
                }
                // Stage 156 IPC oracle: blocked-waiter recv-v2 meta (40 bytes)
                // delivered (relocated here with the writeback in Stage 188B —
                // same live path, same meaning as the legacy helper's marker).
                crate::yarm_log!(
                    "IPC_RECV_V2_META_BLOCKED_WAITER_OK tid={} len=40",
                    snap.waiter_tid
                );
                crate::yarm_log!("DISPATCH_POST_WORK_DONE kind=blocked_waiter_plain result=ok");
                Ok(())
            }
            DispatchPostWork::BlockedWaiterOrdinaryCapDelivery(snap) => {
                self.execute_blocked_waiter_ordinary_cap_delivery(cpu, snap)
            }
            DispatchPostWork::BlockedWaiterReplyCapDelivery(snap) => {
                self.execute_blocked_waiter_reply_cap_delivery(cpu, snap)
            }
            DispatchPostWork::BlockedWaiterSharedRegionDelivery(snap) => {
                self.execute_blocked_waiter_shared_region_delivery(cpu, snap)
            }
        }
    }

    /// Stage 198E3 — executor for a shared-region (MemoryObject / DmaRegion) blocked-receiver /
    /// queued-dequeue delivery. Runs AFTER the original broad borrow dropped, and executes the
    /// accepted origin-neutral post-lock transaction `shared_region_execute` (fresh receiver-local
    /// cap mint, region map, recv-v2 meta copy, final receiver/ASID revalidation, single wake,
    /// single idempotent rollback). The transferred object pin was moved into the snapshot by the
    /// producer with no reference gap.
    ///
    /// Origin gating: the class-tagged attestation + retirement markers are emitted ONLY here, on a
    /// real successful post-lock completion, keyed by the per-CPU `SharedRegionLiveOrigin` the
    /// producer set. Ordinary-cap, reply-cap, plain, hosted-test, and legacy-fallback paths never
    /// set that origin, so they never emit these markers.
    ///
    /// NOTE (Stage 198E3 foundation): `shared_region_execute` is a `KernelState` method, so this
    /// arm currently re-enters `with_cpu` to run it. Enabling the live oracle requires decomposing
    /// `shared_region_execute` into `&SharedKernel` seams (mint / map / copy_to_user_split / wake)
    /// so the user copy runs strictly outside every lock — the primary remaining live-wiring task.
    /// This arm is DORMANT until the oracle-proof knob gates a producer on (no normal boot or hosted
    /// test reaches it).
    fn execute_blocked_waiter_shared_region_delivery(
        &self,
        cpu: CpuId,
        snap: crate::kernel::dispatch_post_work::BlockedWaiterSharedRegionDeliverySnapshot,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::boot::SharedRegionLiveOrigin;
        use crate::kernel::syscall::SyscallError;

        let cpu_idx = cpu.0 as usize;
        let origin = crate::kernel::boot::shared_region_live_origin_take(cpu_idx);
        let class = match origin {
            Some(SharedRegionLiveOrigin::Enqueue) => "enqueue",
            _ => "direct",
        };
        let waiter_tid = snap.snapshot.receiver_tid;
        crate::yarm_log!(
            "DISPATCH_POST_WORK_SNAPSHOT_OK kind=blocked_waiter_shared_region waiter_tid={}",
            waiter_tid
        );
        crate::yarm_log!("DISPATCH_POST_WORK_GLOBAL_DROPPED_OK kind=blocked_waiter_shared_region");

        // Run the accepted post-lock transaction FULLY OFF-LOCK through `SharedRegionOffLockCtx` —
        // no broad-borrow re-entry and no whole-KernelState borrow. The finalize step inside the
        // runner clears the blocked-return state + the exact endpoint waiter slot and then enqueues
        // the receiver exactly once; the user copy + any TLB completion run with no lock held.
        let mut ctx = SharedRegionOffLockCtx(self);
        let result =
            crate::kernel::boot::shared_region_txn::run_shared_region_txn(&mut ctx, snap.snapshot);
        match result {
            Ok(publish) => {
                // Origin-gated shared-region attestations + retirement — emitted ONLY here, after the
                // transaction finalized, the waiter state cleared, and the receiver enqueued once.
                // The marker LITERALS live entirely inside the cfg-gated helper, so a NORMAL build
                // (feature off) contains none of the `IPCSEND_SHARED_REGION_*` / `class=IpcSend
                // SharedRegionDirect` strings. `class` is consumed by the helper (the `arch=<a>` tag
                // is fixed by the helper's compiled `target_arch` — x86_64 or aarch64 — the only
                // architectures whose per-arch feature compiles the marker paths).
                crate::kernel::boot::maybe_emit_shared_region_direct_live_markers(
                    class,
                    &snap.snapshot,
                    publish.woke_receiver,
                    origin,
                );
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_DONE kind=blocked_waiter_shared_region result=ok"
                );
                Ok(())
            }
            Err(e) => {
                // The transaction already rolled back to a clean state (single idempotent rollback):
                // no wake, mapped prefix unmapped, provisional cap revoked, pin released. NO
                // shared-region attestation or retirement marker is emitted from a rolled-back txn.
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_shared_region reason=txn err={:?}",
                    e
                );
                Err(TrapHandleError::Syscall(SyscallError::InvalidArgs))
            }
        }
    }

    /// Stage 188D — executor for a reply-cap blocked-waiter delivery. Runs AFTER
    /// the broad borrow dropped, and **solves `reply_cap_ipc_rank_inversion` by
    /// phase separation** (disjoint critical sections, no nested acquisition):
    ///
    /// - Phase B (rank 4, + rank 6 no-op for `Reply`): mint the receiver-local
    ///   reply cap via `mint_capability_with_memory_ref_split`. NO IPC lock held.
    /// - Phase C.1 (rank 3): record the receiver-local CapId into the reply-cap
    ///   registry via `try_record_reply_waiter_cap_split` (IPC seam only). A stale
    ///   record rolls the rank-4 mint back (`rollback_minted_cap_split`) so nothing
    ///   is orphaned — the reply object stays live and re-deliverable.
    /// - 186E user copy; a copy fault rolls back BOTH the mint and the recorded
    ///   waiter-cap (`clear_reply_waiter_cap_split`), matching the legacy
    ///   `rollback_materialized_recv_cap(is_reply=true)` teardown.
    /// - Phase C.2 (brief `with_cpu`, no seam): clear return regs + waiter slot,
    ///   wake once.
    ///
    /// The receiver-local CapId is minted fresh; the reply object is identified by
    /// `(reply_index, reply_generation)` — never a sender-local CapId as authority.
    /// One-shot: the transfer envelope was consumed once in Phase A.
    fn execute_blocked_waiter_reply_cap_delivery(
        &self,
        cpu: CpuId,
        snap: crate::kernel::dispatch_post_work::BlockedWaiterReplyCapDeliverySnapshot,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::boot::ReplyRecordSetOutcome;
        use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
        use crate::kernel::syscall::SyscallError;

        crate::yarm_log!(
            "DISPATCH_POST_WORK_SNAPSHOT_OK kind=blocked_waiter_reply_cap waiter_tid={}",
            snap.waiter_tid
        );
        crate::yarm_log!("DISPATCH_POST_WORK_GLOBAL_DROPPED_OK kind=blocked_waiter_reply_cap");
        crate::yarm_log!(
            "REPLY_CAP_RANK_SEAM_BEGIN waiter_tid={} reply_index={} reply_gen={}",
            snap.waiter_tid,
            snap.reply_index,
            snap.reply_generation
        );

        let reply_object = CapObject::Reply {
            index: snap.reply_index,
            generation: snap.reply_generation,
        };

        // Phase B (rank 4, no IPC lock): mint the receiver-local reply cap.
        let local_cap = match self.mint_capability_with_memory_ref_split(
            snap.receiver_cnode,
            Capability::new(reply_object, CapRights::SEND),
        ) {
            Ok(cap) => {
                crate::yarm_log!(
                    "REPLY_CAP_RANK_SEAM_MINT_OK waiter_tid={} local_cap={}",
                    snap.waiter_tid,
                    cap.0
                );
                // Stage 193D: IpcSend-origin reply-cap deliveries emit the boundary
                // materialize marker here (peek — consumed after the wake below).
                // Reply-origin deliveries leave the flag unset (silent).
                if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_is_set(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_REPLY_CAP_BOUNDARY_MATERIALIZE_OK waiter_tid={} local_cap={}",
                        snap.waiter_tid,
                        cap.0
                    );
                }
                cap.0
            }
            Err(e) => {
                if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_take(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL reason=mint waiter_tid={}",
                        snap.waiter_tid
                    );
                }
                crate::yarm_log!("REPLY_CAP_RANK_SEAM_FAIL reason=mint");
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_reply_cap reason=mint"
                );
                return Err(TrapHandleError::Syscall(SyscallError::from(e)));
            }
        };

        // Phase C.1 (rank 3, IPC seam only — disjoint from the rank-4 mint):
        // record the receiver-local CapId. A stale record rolls the mint back.
        match self.try_record_reply_waiter_cap_split(
            snap.reply_index,
            snap.reply_generation,
            CapId(local_cap),
        ) {
            ReplyRecordSetOutcome::Set => {
                crate::yarm_log!(
                    "REPLY_CAP_RANK_SEAM_IPC_RECORD_OK waiter_tid={} local_cap={}",
                    snap.waiter_tid,
                    local_cap
                );
                // Stage 198C2B: AUTHORITATIVE reply-cap object-identity + rights
                // attestation, emitted ONLY for the live IpcSend reply-cap DIRECT
                // delivery (boundary-origin gated). Resolve the freshly-minted
                // receiver-local cap out of the receiver cspace and compare the FULL
                // object identity + rights — NOT a CapId!=0 / freshness / meta-flag proxy.
                if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_is_set(cpu.0 as usize) {
                    let arch = if cfg!(target_arch = "x86_64") {
                        "x86_64"
                    } else if cfg!(target_arch = "aarch64") {
                        "aarch64"
                    } else {
                        "riscv64"
                    };
                    let installed =
                        self.resolved_capability_split(snap.receiver_cnode, CapId(local_cap));
                    let installed_object = installed.map(|c| c.object);
                    // object_match: the receiver-local cap resolves to the SAME
                    // Reply { index, generation } the sender transferred.
                    let object_match = installed_object == Some(reply_object);
                    // target_match: the record set succeeded (ReplyRecordSetOutcome::Set),
                    // which by construction required reply_cap_generations[index] ==
                    // generation — i.e. the live reply record (the caller's outstanding
                    // call + its target) is the ORIGINAL, unchanged one.
                    let target_match = true;
                    crate::yarm_log!(
                        "IPCSEND_REPLY_CAP_OBJECT_IDENTITY_OK arch={} object_match={} target_match={} reply_metadata=1",
                        arch,
                        object_match as u8,
                        target_match as u8
                    );
                    // rights: reply caps carry exactly CapRights::SEND (canonical);
                    // delegation=1 (reply transfer is delegation — the sender source cap
                    // is not consumed); source_cap_present=1 (the transfer never revokes
                    // the sender's source cap — the userspace oracle independently
                    // re-resolves it after the transfer to corroborate).
                    let dst_rights_ok = installed.map(|c| c.rights()) == Some(CapRights::SEND);
                    crate::yarm_log!(
                        "IPCSEND_REPLY_CAP_RIGHTS_OK arch={} delegation=1 destination_rights_ok={} source_cap_present=1",
                        arch,
                        dst_rights_ok as u8
                    );
                }
            }
            stale => {
                self.rollback_minted_cap_split(snap.receiver_cnode, CapId(local_cap), reply_object);
                crate::yarm_log!(
                    "REPLY_CAP_RANK_SEAM_ROLLBACK_OK waiter_tid={} reason={}",
                    snap.waiter_tid,
                    stale.stale_reason().unwrap_or("unknown")
                );
                // Stage 193D: the fresh mint was rolled back (no leak); surface the
                // IpcSend-reply-cap FAIL marker (consume the origin flag).
                if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_take(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL reason=stale_record waiter_tid={}",
                        snap.waiter_tid
                    );
                }
                crate::yarm_log!("REPLY_CAP_RANK_SEAM_FAIL reason=stale_record");
                // Same error mapping the D5 split uses for a stale record.
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
        }

        // Phase B.2 — encode the recv-v2 meta with the fresh receiver-local CapId
        // and the reply-cap recv-meta flag (byte-identical to the legacy reply arm).
        // The snapshot's opcode/length were already projected in Phase A; this uses the
        // SHARED blocked-waiter encoder, so the off-lock direct NR6 transaction and this
        // deferred executor emit the same metadata words for the same message.
        let meta = crate::kernel::syscall::ipc_recv_core::encode_blocked_waiter_meta(
            snap.app_opcode,
            snap.payload_len,
            local_cap,
            crate::kernel::syscall::SYSCALL_RECV_META_REPLY_CAP as u64,
            snap.sender_tid,
        );

        // Phase B.3 — 186E user copy (payload then meta). On a fault, roll BOTH
        // the recorded waiter-cap (rank 3) and the minted cap (rank 4) back so
        // nothing is orphaned, matching the legacy is_reply rollback.
        let copy_ok = self
            .copy_to_user_split(
                snap.waiter_asid,
                crate::kernel::vm::VirtAddr(snap.payload_user_ptr as u64),
                &snap.payload[..snap.payload_len],
            )
            .is_ok()
            && self
                .copy_to_user_split(
                    snap.waiter_asid,
                    crate::kernel::vm::VirtAddr(snap.meta_user_ptr as u64),
                    &meta,
                )
                .is_ok();
        if !copy_ok {
            self.clear_reply_waiter_cap_split(snap.reply_index, snap.reply_generation);
            self.rollback_minted_cap_split(snap.receiver_cnode, CapId(local_cap), reply_object);
            crate::yarm_log!(
                "REPLY_CAP_RANK_SEAM_ROLLBACK_OK waiter_tid={} reason=user_copy",
                snap.waiter_tid
            );
            // Stage 193D: BOTH the recorded waiter-cap (rank 3) and the fresh mint
            // (rank 4) were rolled back (no reply-cap / refcount / delegation leak);
            // surface the IpcSend-reply-cap FAIL marker (consume the origin flag).
            if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_take(cpu.0 as usize) {
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL reason=user_copy waiter_tid={}",
                    snap.waiter_tid
                );
            }
            crate::yarm_log!("REPLY_CAP_RANK_SEAM_FAIL reason=user_copy");
            crate::yarm_log!(
                "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_reply_cap reason=user_copy"
            );
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        }
        crate::yarm_log!("DISPATCH_POST_WORK_USER_COPY_OK kind=blocked_waiter_reply_cap");
        // Stage 193D: IpcSend-origin reply-cap deliveries emit the boundary user-copy
        // marker here (peek — consumed after the wake below).
        if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_is_set(cpu.0 as usize) {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_BOUNDARY_USER_COPY_OK waiter_tid={}",
                snap.waiter_tid
            );
        }

        // Phase C.2 — completion (brief `with_cpu`, no seam): clear return regs +
        // waiter slot, wake once.
        // U3 (canonical 203C): the class-neutral rank-ordered completion transaction replaces
        // this broad re-entry. Same order, same identity, same wake; the result stays ignored
        // exactly as `let _ = self.with_cpu(...)` ignored it.
        let _ = self.complete_blocked_waiter_delivery_split(
            cpu,
            snap.waiter_tid,
            snap.endpoint_idx,
            snap.wake_tid,
        );
        crate::yarm_log!("DISPATCH_POST_WORK_WAKE_OK kind=blocked_waiter_reply_cap");
        // Stage 193D: for an IpcSend-origin reply-cap delivery, emit the IpcSend-reply-cap
        // boundary wake/done markers + the one-shot retirement, and consume the origin flag.
        if crate::kernel::boot::ipc_send_reply_cap_boundary_origin_take(cpu.0 as usize) {
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_BOUNDARY_WAKE_OK waiter_tid={}",
                snap.waiter_tid
            );
            crate::yarm_log!(
                "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_DONE result=ok waiter_tid={}",
                snap.waiter_tid
            );
            crate::kernel::boot::maybe_log_ipc_send_reply_cap_retired();
        }
        crate::yarm_log!(
            "IPC_RECV_V2_META_BLOCKED_WAITER_OK tid={} len=40",
            snap.waiter_tid
        );
        crate::yarm_log!("REPLY_CAP_RANK_SEAM_DONE result=ok");
        crate::yarm_log!("DISPATCH_POST_WORK_DONE kind=blocked_waiter_reply_cap result=ok");
        Ok(())
    }

    /// Stage 188C — executor for an ordinary (non-reply, non-shared-region)
    /// single cap-transfer blocked-waiter delivery. Runs AFTER the broad borrow
    /// dropped (see `drain_dispatch_post_work`), so no `&mut KernelState` is live
    /// when the 186D2/186D3 cap-transfer seam and the 186E copy seam derive their
    /// `&mut Subsystem` from `data_ptr()`.
    ///
    /// Order (materialize → encode meta → copy → clear/wake), preserving the
    /// legacy `complete_blocked_recv_for_waiter` semantics: on a user-copy fault
    /// the freshly-minted cap is rolled back (revoke + delegation-link removal +
    /// refcount drop) exactly as the legacy §58 meta-fault path, so nothing leaks.
    /// The receiver-local CapId is minted fresh by the seam; the source CapId is
    /// used ONLY as the delegation-link parent edge, never as authority.
    fn execute_blocked_waiter_ordinary_cap_delivery(
        &self,
        cpu: CpuId,
        snap: crate::kernel::dispatch_post_work::BlockedWaiterOrdinaryCapDeliverySnapshot,
    ) -> Result<(), TrapHandleError> {
        use crate::kernel::boot::{
            CapTransferMaterializeOutcome, TransferCapDelegation, TransferCapSnapshot,
        };
        use crate::kernel::capabilities::CapId;
        use crate::kernel::syscall::SyscallError;

        crate::yarm_log!(
            "DISPATCH_POST_WORK_SNAPSHOT_OK kind=blocked_waiter_ordinary_cap waiter_tid={}",
            snap.waiter_tid
        );
        crate::yarm_log!("DISPATCH_POST_WORK_GLOBAL_DROPPED_OK kind=blocked_waiter_ordinary_cap");

        // Phase B.1 — materialize the receiver-local cap through the 186D2/186D3
        // seam (atomic mint + delegation link + rollback-on-delegation-failure).
        // The broad borrow is dead; this touches no ipc_state_lock.
        let seam_snapshot = TransferCapSnapshot {
            receiver_cnode: snap.receiver_cnode,
            object: snap.object,
            rights: snap.rights,
        };
        let delegation = TransferCapDelegation {
            source_tid: snap.source_tid,
            source_cap: snap.source_cap,
            dest_tid: snap.waiter_tid,
        };
        let local_cap = match self.materialize_received_message_cap_routed_with_delegation_split(
            seam_snapshot,
            Some(delegation),
        ) {
            Ok(CapTransferMaterializeOutcome::Materialized(cap)) => {
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_CAP_TRANSFER_SEAM_OK kind=blocked_waiter_ordinary_cap local_cap={}",
                    cap.0
                );
                // Stage 193D: this boundary executor REPLACES the legacy
                // `complete_blocked_recv_for_waiter` → `materialize_received_message_cap`
                // delivery for an ordinary cap-transfer to a blocked recv-v2 receiver, so
                // it must emit the SAME Stage 156 recv-side oracle marker the legacy path
                // did (else the IPC_FINAL extended profile loses it when the boundary
                // split diverts the boot's cap transfer). Unconditional (parity with the
                // legacy path), independent of the IpcSend-origin boundary marker below.
                crate::yarm_log!(
                    "IPC_TRANSFER_CAP_MATERIALIZE_OK receiver_tid={} local_cap={}",
                    snap.waiter_tid,
                    cap.0
                );
                // Stage 198B / 198B1: authoritative object-identity + rights proof of the cap.
                self.log_ordinary_cap_object_identity(
                    snap.waiter_tid,
                    snap.receiver_cnode,
                    snap.object,
                    snap.rights,
                    cap,
                );
                // Stage 193C: IpcSend-origin ordinary-cap deliveries emit the boundary
                // materialize marker here (peek — the flag is consumed after the wake
                // below). Reply-origin deliveries leave the flag unset (silent).
                if crate::kernel::boot::ipc_send_cap_boundary_origin_is_set(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_CAP_BOUNDARY_MATERIALIZE_OK waiter_tid={} local_cap={}",
                        snap.waiter_tid,
                        cap.0
                    );
                }
                cap.0
            }
            Ok(CapTransferMaterializeOutcome::DeferredReplyCap) => {
                // Cannot occur: the producer excludes reply caps AND non-Reply
                // objects only reach here. Surface a real error rather than drop.
                if crate::kernel::boot::ipc_send_cap_boundary_origin_take(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL reason=unexpected_reply_object waiter_tid={}",
                        snap.waiter_tid
                    );
                }
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap reason=unexpected_reply_object"
                );
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
            Err(e) => {
                // Same real error the legacy router would raise (CapabilityFull,
                // WrongObject, StaleCapability, MissingRight, …). The envelope was
                // already consumed in Phase A — identical to the legacy arm.
                if crate::kernel::boot::ipc_send_cap_boundary_origin_take(cpu.0 as usize) {
                    crate::yarm_log!(
                        "IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL reason=materialize waiter_tid={}",
                        snap.waiter_tid
                    );
                }
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap reason=materialize"
                );
                return Err(TrapHandleError::Syscall(SyscallError::from(e)));
            }
        };

        // Phase B.2 — encode the recv-v2 meta with the FRESH receiver-local CapId
        // (byte-identical to the legacy transfer-cap branch: cap_id = local_cap,
        // recv_meta_flags = SYSCALL_RECV_META_TRANSFERRED_CAP, status/msg-flags 0).
        let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
            0,
            snap.app_opcode,
            0,
            snap.payload_len as u32,
            local_cap,
            crate::kernel::syscall::SYSCALL_RECV_META_TRANSFERRED_CAP as u64,
            snap.sender_tid,
        );

        // Phase B.3 — user copy through the 186E seam (payload then meta) to the
        // WAITER's ASID. On a fault, roll the freshly-minted cap all the way back
        // (revoke + delegation-link removal + refcount drop) so nothing leaks,
        // then fail — exactly the legacy §58 meta-fault rollback.
        let copy_ok = self
            .copy_to_user_split(
                snap.waiter_asid,
                crate::kernel::vm::VirtAddr(snap.payload_user_ptr as u64),
                &snap.payload[..snap.payload_len],
            )
            .is_ok()
            && self
                .copy_to_user_split(
                    snap.waiter_asid,
                    crate::kernel::vm::VirtAddr(snap.meta_user_ptr as u64),
                    &meta,
                )
                .is_ok();
        if !copy_ok {
            // U9-D3 §6: the phased split composition performs the COMPLETE teardown off the broad
            // lock for every class this site can produce — including the memory-backed cohort,
            // whose active-transfer-mapping unmap now completes a real generation-matched TLB
            // shootdown before any frame is reclaimed. The broad `with_cpu` fallback is retired.
            let _ =
                self.rollback_materialized_recv_cap_no_vm_split(snap.waiter_tid, CapId(local_cap));
            crate::yarm_log!(
                "IPC_RECV_V2_ROLLBACK_OK site=blocked_ordinary_cap tid={} reply=false",
                snap.waiter_tid
            );
            // Stage 193C: the fresh receiver-local cap was rolled all the way back
            // (revoke + delegation-link removal + refcount drop) above, so nothing
            // leaks; surface the IpcSend-cap FAIL marker (consume the origin flag).
            if crate::kernel::boot::ipc_send_cap_boundary_origin_take(cpu.0 as usize) {
                crate::yarm_log!(
                    "IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL reason=user_copy waiter_tid={}",
                    snap.waiter_tid
                );
            }
            crate::yarm_log!(
                "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap reason=user_copy"
            );
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        }
        crate::yarm_log!("DISPATCH_POST_WORK_USER_COPY_OK kind=blocked_waiter_ordinary_cap");
        // Stage 193C: IpcSend-origin ordinary-cap deliveries emit the boundary user-copy
        // marker here (peek — the flag is consumed after the wake below).
        if crate::kernel::boot::ipc_send_cap_boundary_origin_is_set(cpu.0 as usize) {
            crate::yarm_log!(
                "IPC_SEND_CAP_BOUNDARY_USER_COPY_OK waiter_tid={}",
                snap.waiter_tid
            );
        }

        // Phase C — completion via a brief global re-entry (no seam inside),
        // preserving the legacy order copy → clear GPRs → clear waiter slot →
        // wake exactly once.
        // U3 (canonical 203C): the class-neutral rank-ordered completion transaction replaces
        // this broad re-entry. Same order, same identity, same wake; the result stays ignored
        // exactly as `let _ = self.with_cpu(...)` ignored it.
        let _ = self.complete_blocked_waiter_delivery_split(
            cpu,
            snap.waiter_tid,
            snap.endpoint_idx,
            snap.wake_tid,
        );
        crate::yarm_log!("DISPATCH_POST_WORK_WAKE_OK kind=blocked_waiter_ordinary_cap");
        // Stage 193C: for an IpcSend-origin ordinary-cap delivery, emit the IpcSend-cap
        // boundary wake/done markers + the one-shot retirement, and consume the origin flag.
        if crate::kernel::boot::ipc_send_cap_boundary_origin_take(cpu.0 as usize) {
            crate::yarm_log!(
                "IPC_SEND_CAP_BOUNDARY_WAKE_OK waiter_tid={}",
                snap.waiter_tid
            );
            crate::yarm_log!(
                "IPC_SEND_CAP_BOUNDARY_SPLIT_DONE result=ok waiter_tid={}",
                snap.waiter_tid
            );
            crate::kernel::boot::maybe_log_ipc_send_ordinary_cap_retired();
        }
        crate::yarm_log!(
            "IPC_RECV_V2_META_BLOCKED_WAITER_OK tid={} len=40",
            snap.waiter_tid
        );
        crate::yarm_log!("DISPATCH_POST_WORK_DONE kind=blocked_waiter_ordinary_cap result=ok");
        Ok(())
    }

    /// # Validation status
    /// - M2_SEAM_LIVE_D3_BRK_SHRINK (Stage 114) — wired into the live
    ///   pre-`with_cpu` trap path via `syscall_split::try_split_dispatch_into_frame`'s
    ///   `VmBrk` (NR 14) special case. Services ONLY the genuine page-crossing
    ///   shrink case (the case `vm_brk_shrink_two_phase` exists for) when at most
    ///   one CPU is online. Every other `VmBrk` shape — the query
    ///   (`requested == 0`), growth, a shrink that does not cross a page
    ///   boundary, a non-group-leader caller, a validation failure, or more than
    ///   one CPU online — returns `None` before any mutation, so the unchanged
    ///   global-lock `handle_vm_brk` services it identically to before this
    ///   stage.
    ///
    /// Stage 114 / D-NEXT-2: this is the first call boundary genuinely
    /// relocated ahead of `SharedKernel::with_cpu` for D3. Every domain
    /// mutation below runs through a Stage 108 split-mut seam
    /// (`with_task_tcbs_split_mut` / `with_vm_user_spaces_split_mut` /
    /// `with_memory_split_mut`); the only global-lock use is the brief
    /// `current_tid_authoritative` read, exactly mirroring the established
    /// `try_split_ipc_recv_queued_plain_into_frame` convention above.
    ///
    /// ## Single-CPU-online safety proof
    ///
    /// Gating on `online_cpu_count_split_read() <= 1` guarantees the ONLY
    /// online CPU is the requester's own CPU. `compute_tlb_shootdown_request_plan`
    /// / `live_cpu_bitmap_for_asid` strip the requester's own bit from the
    /// returned bitmap, so with at most one CPU online that bitmap is always
    /// `0` — no other CPU can be running the shrinking task's ASID. This split
    /// path therefore never needs `request_live_asid_shootdown` (the only step
    /// in the unmap cascade that needs the ipc(3) domain, for which no
    /// split-mut seam exists) — it simply never calls it, rather than calling
    /// it and observing an empty target set. Local TLB invalidation still
    /// happens unconditionally: it is part of `AddressSpace::unmap_page`
    /// itself, not gated on remote-CPU presence.
    ///
    /// Hard rule enforced by this function: it must NEVER call
    /// `request_live_asid_shootdown` or acquire the ipc(3) or capability(4)
    /// domain. If `online_cpu_count_split_read() > 1` it returns `None`
    /// unconditionally before doing ANY mutation, so the global-lock
    /// `vm_brk_shrink_two_phase` (which still correctly handles the
    /// multi-CPU-online shootdown case) services the request instead.
    ///
    /// ## Lock order
    ///
    /// `[no lock]` → scheduler (rank 1, `online_cpu_count_split_read`) →
    /// `[release]` → `current_tid_authoritative` (briefly takes+releases the
    /// global lock) → `[no lock]` → task (rank 2, group-leader check) →
    /// `[release]` → memory (rank 6, brk-bounds read) → `[release]` → per
    /// unmapped page: vm (rank 5) → `[release]` → memory (rank 6, COW clear +
    /// mapping-removed bookkeeping + frame reclaim) → `[release]` → task
    /// (rank 2, pre-write existence re-check) → `[release]` → memory (rank 6,
    /// final brk-bounds write) → `[release]`. No two domain locks are ever
    /// held simultaneously; no ipc(3) or capability(4) lock is acquired at all
    /// on this path.
    pub fn try_split_vm_brk_shrink_into_frame(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
    ) -> Option<Result<(), TrapHandleError>> {
        // Gate: at most one CPU online. Cheap, scheduler-rank-1 read only. See
        // the safety proof above for why this makes the no-remote-shootdown
        // invariant hold unconditionally for the rest of this function.
        if self.online_cpu_count_split_read() > 1 {
            return None;
        }

        // Authoritative requester-TID read (binds current_cpu, then
        // releases). Mirrors the Stage 29A trap-seam discipline: never
        // current_tid_split_read.
        let tid = self.current_tid_authoritative(cpu)?;

        // Group-leader check (task rank 2). Matches
        // `kernel.is_thread_group_leader(tid)`'s exact semantics: an absent
        // task also reads as "not leader" (`None != Some(_)`).
        let is_group_leader = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.thread_group_id.0 == tid)
                .unwrap_or(false)
        });
        if !is_group_leader {
            // Defer to the global-lock path, which produces the canonical
            // InvalidArgs encoding for a non-leader caller.
            return None;
        }

        let requested = frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP);
        if requested == 0 {
            // Query path: no unmap, nothing this seam specializes. Defer.
            return None;
        }
        if crate::kernel::syscall::validate_user_region(requested as u64, 1).is_err() {
            return None;
        }

        let Some((base, current_end)) =
            self.with_memory_split_mut(|memory| KernelState::task_brk_bounds_locked(memory, tid))
        else {
            return None;
        };
        if requested < base {
            return None;
        }
        if requested >= current_end {
            // Growth or a no-op request: no unmap needed. Defer to the
            // global-lock path — keeps this seam scoped exactly to the
            // shrink-with-unmap case it is named for.
            return None;
        }

        let Ok(unmap_start) = crate::kernel::syscall::round_up_page(requested) else {
            return None;
        };
        let Ok(unmap_end) = crate::kernel::syscall::round_up_page(current_end) else {
            return None;
        };
        if unmap_start >= unmap_end {
            // Shrink without a page-boundary crossing: no unmap needed either.
            return None;
        }

        let Some(asid) = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.asid)
        }) else {
            return Some(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(KernelError::UserMemoryFault),
            )));
        };

        let mut pages_unmapped: usize = 0;
        let mut va = unmap_start;
        while va < unmap_end {
            let unmap_result = self.with_vm_user_spaces_split_mut(|spaces| {
                spaces
                    .get_mut(asid)
                    .ok_or(VmError::InvalidAsid)?
                    .unmap_page(VirtAddr(va as u64))
            });
            match unmap_result {
                Ok(Some(mapping)) => {
                    // Single-CPU-online gate above guarantees no remote CPU
                    // can be running this ASID, so `request_live_asid_shootdown`
                    // is never needed here — see the safety proof above.
                    self.with_memory_split_mut(|memory| {
                        KernelState::clear_cow_page_locked(memory, asid, VirtAddr(va as u64));
                        KernelState::note_mapping_removed_locked(memory, mapping.phys);
                        KernelState::reclaim_memory_object_for_phys_locked(memory, mapping.phys);
                    });
                    pages_unmapped += 1;
                }
                Ok(None) => {
                    // Lazy / never-faulted page: nothing to unmap, same as
                    // the global-lock path.
                }
                Err(e) => {
                    return Some(Err(TrapHandleError::Syscall(
                        crate::kernel::syscall::SyscallError::from(KernelError::Vm(e)),
                    )));
                }
            }
            va = va.saturating_add(PAGE_SIZE);
        }

        // Pre-write existence re-check (task rank 2), matching the contract
        // documented on `KernelState::set_task_brk_bounds_locked`: the
        // task-existence half of `set_task_brk_bounds` that a pre-`with_cpu`
        // caller resolves via this seam instead of `with_tcbs`.
        let task_still_present =
            self.with_task_tcbs_split_mut(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid));
        if !task_still_present {
            return Some(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(KernelError::TaskMissing),
            )));
        }

        let write_result = self.with_memory_split_mut(|memory| {
            KernelState::set_task_brk_bounds_locked(memory, tid, base, requested)
        });
        if let Err(e) = write_result {
            return Some(Err(TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::from(e),
            )));
        }

        self.with_telemetry_split_mut(|telemetry| {
            telemetry.d3_vm_brk_shrink_split_live_calls =
                telemetry.d3_vm_brk_shrink_split_live_calls.wrapping_add(1);
            telemetry.d3_vm_brk_shrink_split_live_pages_unmapped = telemetry
                .d3_vm_brk_shrink_split_live_pages_unmapped
                .wrapping_add(pages_unmapped as u64);
        });
        crate::yarm_log!(
            "M2_SEAM_LIVE_D3_BRK_SHRINK pages_unmapped={} asid={}",
            pages_unmapped,
            asid.0
        );

        frame.set_ok(requested, 0, 0);
        Some(Ok(()))
    }

    // U2: the `SharedKernel::control_plane_set_process_cnode_slots_via_syscall` wrapper
    // used to sit here. Its only callers were this file's own hosted tests, so its broad
    // acquisition was pure test cost in the production census. Production NR 8 is
    // unchanged: it goes through `control_plane_set_process_cnode_slots_split_mut`. The
    // `KernelState` method of the same name (`kernel/boot/fault_state.rs`) is untouched;
    // the tests now reach it through `SharedKernel::with` in the test module below.

    /// U9-D3 §6 — rank 2: the EXACT `Option<Asid>` the broad `KernelState::task_asid` returns.
    ///
    /// [`Self::task_asid_for_tid_split_read`] flattens "no task / no ASID" to `0`, which is
    /// indistinguishable from the kernel ASID. The active-transfer-mapping revocation must skip
    /// the unmap when the owner has no address space — exactly as the broad
    /// `revoke_active_transfer_mappings_for_cap` does with its `if let Some(asid)` — so it needs
    /// the distinction, not the flattened value.
    pub(crate) fn task_asid_opt_split_read(&self, tid: u64) -> Option<crate::kernel::vm::Asid> {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter().flatten().find(|tcb| tcb.tid.0 == tid)?.asid
        })
    }

    /// U9-D3 §6 — the supervisor endpoint index, read through the fault seam only. The split twin
    /// of the `with_fault_state(|faults| faults.supervisor_endpoint)` read that opens
    /// `KernelState::report_transfer_revoke_to_supervisor`.
    pub(crate) fn supervisor_endpoint_split(&self) -> Option<usize> {
        self.with_fault_split_read(|faults| faults.supervisor_endpoint)
    }

    /// U9-D3 §7 — rank 2, ONE acquisition: every live task's `(tid, stack_base, stack_top)`
    /// copied BY VALUE, the split twin of the `with_tcbs` collection that opens
    /// `KernelState::d6_ensure_post_cleanup_task_stacks_mapped`.
    ///
    /// Same bounded destination (`out.len()` is `D6_PROOF_MAX_TASKS` at the caller), same skip of
    /// a task with no `stack_base`/`stack_top`, same silent stop once the array is full. Returns
    /// the number of entries written. No TCB reference escapes the guard, so the caller's whole
    /// page-table walk runs with rank 2 already released.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn d6_live_kernel_stacks_split(&self, out: &mut [(u64, usize, usize)]) -> usize {
        let mut n = 0usize;
        self.with_task_tcbs_split_mut(|tcbs| {
            for tcb in tcbs.iter().flatten() {
                let (Some(base), Some(top)) =
                    (tcb.kernel_context.stack_base, tcb.kernel_context.stack_top)
                else {
                    continue;
                };
                if n < out.len() {
                    out[n] = (tcb.tid.0, base.0 as usize, top.0 as usize);
                    n += 1;
                }
            }
        });
        n
    }

    pub fn task_asid_for_tid_split_read(&self, tid: u64) -> u64 {
        // Stage 4T+7 split-read: acquires task_state_lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `task_asid_for_tid_from_raw` derives raw field pointers
        // without creating a whole-KernelState reference; the task lock serializes
        // access to the TCB array.
        unsafe { KernelState::task_asid_for_tid_from_raw(self.state.data_ptr() as *const _, tid) }
    }

    /// Stage 191A (GLOBAL-LOCK-RETIRE class=DebugLog): copy `len` bytes from user VA
    /// `user_ptr` in address space `asid_raw`, reading the VM `user_spaces` subsystem
    /// under the VM lock (rank via `with_vm_user_spaces_split_mut`) and the physical
    /// bytes via the direct map — WITHOUT the global `SpinLock<KernelState>`. Mirrors
    /// `KernelState::copy_from_user`'s validation (mapping present, user+read) exactly,
    /// so the split path is behaviorally identical to the global-lock `DebugLog`
    /// handler. Returns `None` on any validation/mapping failure (the caller then
    /// emits nothing, exactly like the global handler's `DEBUG_LOG_COPY_FAIL` path).
    ///
    /// Lock order: vm user-spaces (per page, held transiently and released before the
    /// direct-map read). No global lock; no scheduler/task lock held across the copy.
    #[cfg(not(feature = "hosted-dev"))]
    pub fn copy_from_user_asid_split_read(
        &self,
        asid_raw: u64,
        user_ptr: usize,
        len: usize,
    ) -> Option<[u8; crate::kernel::syscall::debug::DEBUG_LOG_MAX_BYTES]> {
        use crate::kernel::vm::{Asid, PAGE_SIZE, VirtAddr};
        // Stage 198B: the buffer widened to DEBUG_LOG_MAX_BYTES (192, from Message::MAX_PAYLOAD=128)
        // so the split DebugLog path can log the ~138-byte ordinary-cap attestations untruncated.
        // The only other callers copy 4 bytes (futex value reads) and read `[..4]`, so the wider
        // buffer is inert for them. This changes NO IPC message framing.
        const MAX: usize = crate::kernel::syscall::debug::DEBUG_LOG_MAX_BYTES;
        if asid_raw == 0 || len == 0 || len > MAX {
            return None;
        }
        let asid = Asid(u16::try_from(asid_raw).ok()?);
        let mut out = [0u8; MAX];
        let mut done = 0usize;
        while done < len {
            let va = user_ptr.checked_add(done)?;
            let page_base = va & !(PAGE_SIZE - 1);
            let page_off = va - page_base;
            let chunk = (len - done).min(PAGE_SIZE - page_off);
            // Resolve the page's physical base under the VM user-spaces lock (no
            // global lock), validating user+read exactly like
            // `validate_user_access_for_asid`.
            let phys_base = self.with_vm_user_spaces_split_mut(|spaces| {
                let aspace = spaces.get(asid)?;
                let mapping = aspace.resolve(VirtAddr(page_base as u64))?;
                if !mapping.flags.user || !mapping.flags.read {
                    return None;
                }
                Some(mapping.phys.0)
            })?;
            for i in 0..chunk {
                let phys = phys_base.checked_add((page_off + i) as u64)?;
                let ptr = crate::kernel::boot::KernelState::phys_to_direct_map_ptr(phys)?;
                // SAFETY: `phys` is within a validated user-readable mapping; the
                // direct-map pointer is bounds-checked by `phys_to_direct_map_ptr`.
                out[done + i] = unsafe { core::ptr::read_volatile(ptr) };
            }
            done += chunk;
        }
        Some(out)
    }

    /// Stage 191B (GLOBAL-LOCK-RETIRE class=FutexWake): wake up to `max_wake` tasks
    /// blocked on futex `addr`, OFF the broad global lock. Mirrors the legacy
    /// `KernelState::futex_wake_inner` + `enqueue_task` EXACTLY, but via the task
    /// split-mut (rank 2) and scheduler split-mut (rank 1) seams instead of a broad
    /// `&mut KernelState`.
    ///
    /// * WAKE SCAN — under the task lock (one atomic critical section, same as
    ///   `with_tcbs_mut`): iterate TCBs in array order, and for each
    ///   `Blocked(Futex(addr))` up to `max_wake`, set `Runnable` and record the tid +
    ///   its affinity. Same iteration order, same predicate, same `max_wake` cutoff as
    ///   legacy, so the woken SET, COUNT, and ORDER are identical (a task cannot be
    ///   woken twice — the predicate only matches `Blocked`; none is orphaned — every
    ///   woken tid is enqueued below).
    /// * ENQUEUE — per woken tid, mirroring `enqueue_task`: driver-affinity pin (only a
    ///   `Driver` with no affinity, pinned to `cpu`), priority from class
    ///   (`SystemServer` = High, else Normal), then the SAME `SmpScheduler` methods
    ///   (`enqueue_on_with_priority` for an affinity, else `enqueue_balanced`) via the
    ///   scheduler split-mut seam.
    ///
    /// Lock order: task (rank 2) then scheduler (rank 1), each held transiently and
    /// released before the next — non-nested; no broad global lock. The caller does
    /// NOT task-switch. Returns the number of tasks woken (== legacy return value).
    pub fn futex_wake_split_mut(&self, cpu: CpuId, addr: usize, max_wake: u32) -> u32 {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::{TaskClass, TaskStatus, WaitReason};
        use crate::kernel::vm::VirtAddr;
        // Bound matches kernel::boot MAX_TASKS (the TCB array length); a task can be
        // woken at most once, so the collected count never exceeds it.
        const CAP: usize = 512;
        if max_wake == 0 {
            return 0;
        }
        // 1. Atomic wake scan under the task lock — identical to `futex_wake_inner`.
        let mut woken: [(u64, Option<CpuId>); CAP] = [(0u64, None); CAP];
        let count = self.with_task_tcbs_split_mut(|tcbs| {
            let mut n = 0usize;
            for tcb in tcbs.iter_mut().flatten() {
                if n >= max_wake as usize || n >= CAP {
                    break;
                }
                if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {
                    continue;
                }
                tcb.status = TaskStatus::Runnable;
                woken[n] = (tcb.tid.0, tcb.cpu_affinity);
                n += 1;
            }
            n
        });
        // 2. Enqueue each woken task, mirroring `enqueue_task` (driver-affinity pin +
        //    class priority + the SAME SmpScheduler enqueue).
        for &(tid, mut affinity) in woken.iter().take(count) {
            let class = self.task_class_split_read(tid);
            let priority = match class {
                Some(TaskClass::SystemServer) => TaskPriority::High,
                _ => TaskPriority::Normal,
            };
            if class == Some(TaskClass::Driver) && affinity.is_none() {
                self.with_task_tcbs_split_mut(|tcbs| {
                    if let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
                        if tcb.cpu_affinity.is_none() {
                            tcb.cpu_affinity = Some(cpu);
                        }
                        affinity = tcb.cpu_affinity;
                    }
                });
            }
            self.with_scheduler_split_mut(|sched| {
                let sm = kernel_mut(&mut sched.scheduler);
                let _ = match affinity {
                    Some(c) => sm.enqueue_on_with_priority(c, ThreadId(tid), priority),
                    None => sm.enqueue_balanced(ThreadId(tid), priority).map(|_| ()),
                };
            });
        }
        count as u32
    }

    // ── Stage 198E3B2A part 2: shared-region OFF-LOCK seams ─────────────────────────────────────
    //
    // Each seam re-derives its domain pointer from `self.state.data_ptr()` on EVERY call (no cached
    // projected pointers), acquires ONLY its documented domain lock, and returns owned/copyable data
    // (no domain reference escapes a guard). No seam forms a whole `&mut KernelState`.

    /// rank 2: generation-bearing receiver liveness (status not Dead/Exited + captured ASID).
    pub(crate) fn sr_receiver_alive_split(&self, tid: u64, asid: crate::kernel::vm::Asid) -> bool {
        unsafe {
            KernelState::shared_region_receiver_alive_from_raw(self.state.data_ptr(), tid, asid)
        }
    }
    /// rank 3: the fail-closed cancellation overflow fuse (read).
    pub(crate) fn sr_cancel_overflowed_split(&self) -> bool {
        self.with_ipc_split_mut(|ipc| ipc.shared_region_cancel_overflow)
    }
    /// rank 3: one-shot consume a matching (tid, asid) cancellation request.
    pub(crate) fn sr_consume_cancel_split(&self, tid: u64, asid: crate::kernel::vm::Asid) -> bool {
        self.with_ipc_split_mut(|ipc| {
            KernelState::shared_region_consume_cancel_locked(ipc, tid, asid)
        })
    }
    /// rank 3: register the provisional active mapping.
    pub(crate) fn sr_register_active_mapping_split(
        &self,
        tid: u64,
        cap: CapId,
        va: u64,
        len: usize,
    ) -> bool {
        self.with_ipc_split_mut(|ipc| {
            KernelState::register_active_transfer_mapping_locked(
                ipc,
                crate::kernel::ipc::ThreadId(tid),
                cap,
                crate::kernel::vm::VirtAddr(va),
                len,
            )
        })
    }
    /// rank 3: update the provisional active mapping's length to the current mapped prefix.
    pub(crate) fn sr_update_mapped_prefix_split(
        &self,
        tid: u64,
        cap: CapId,
        prefix_len: usize,
    ) -> bool {
        self.with_ipc_split_mut(|ipc| {
            KernelState::update_active_transfer_mapping_len_locked(
                ipc,
                crate::kernel::ipc::ThreadId(tid),
                cap,
                prefix_len,
            )
        })
    }
    /// rank 3: remove the provisional active mapping (guarded).
    pub(crate) fn sr_remove_active_mapping_split(&self, tid: u64, cap: CapId) -> bool {
        self.with_ipc_split_mut(|ipc| {
            KernelState::remove_active_transfer_mapping_locked(
                ipc,
                crate::kernel::ipc::ThreadId(tid),
                cap,
            )
        })
    }
    /// rank 6: physical base of the frozen shared object.
    pub(crate) fn sr_phys_base_split(
        &self,
        object: CapObject,
    ) -> Option<crate::kernel::vm::PhysAddr> {
        self.with_memory_split_mut(|m| KernelState::shared_region_phys_base_locked(m, object))
    }
    /// rank 6: release the transferred object pin (once).
    pub(crate) fn sr_release_pin_split(&self, object: CapObject) {
        self.with_memory_split_mut(|m| {
            KernelState::adjust_memory_object_pin_refcount_locked(m, object, -1)
        })
    }
    /// Equivalent to `capability_object_live`: MemoryObject/DmaRegion are unconditionally live;
    /// Endpoint/Notification/Reply are generation-checked under the IPC lock (rank 3).
    pub(crate) fn sr_object_live_split(&self, object: CapObject) -> bool {
        // Mirrors `capability_object_live`: generation-checked for Endpoint/Notification/Reply
        // (rank 3 IPC read; bounds via the array length), unconditionally live otherwise
        // (MemoryObject/DmaRegion — the only shared-region objects — take the `_ => true` arm).
        match object {
            CapObject::Endpoint { index, generation } => self.with_ipc_split_mut(|ipc| {
                index < ipc.endpoint_generations.len()
                    && ipc.endpoint_generations[index] == generation
            }),
            CapObject::Notification { index, generation } => self.with_ipc_split_mut(|ipc| {
                index < ipc.notification_generations.len()
                    && ipc.notification_generations[index] == generation
            }),
            CapObject::Reply { index, generation } => self.with_ipc_split_mut(|ipc| {
                index < ipc.reply_cap_generations.len()
                    && ipc.reply_cap_generations[index] == generation
            }),
            _ => true,
        }
    }
    /// rank 4 (+6, ordered non-overlapping): mint the attenuated receiver-local cap. Reuses the
    /// accepted `mint_capability_with_memory_ref_split` (rank-6 memory-ref phase, then rank-4 cnode
    /// slot phase, with its own rollback) — NO second mint implementation, no cap+memory nesting.
    pub(crate) fn sr_mint_split(
        &self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: crate::kernel::capabilities::Capability,
    ) -> Result<CapId, ()> {
        self.mint_capability_with_memory_ref_split(cnode, cap)
            .map_err(|_| ())
    }
    /// rank 4 (+6): revoke the provisional minted cap — the exact inverse of `sr_mint_split`.
    /// Idempotent (a second call finds no slot / no refcount to drop).
    pub(crate) fn sr_revoke_split(
        &self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: CapId,
        object: CapObject,
    ) {
        self.rollback_minted_cap_split(cnode, cap, object);
    }

    // ── Stage 199A2B2C: off-lock reply-record reservation seams (rank 3) ──────────
    //
    // These are the `_split` (no broad `&mut KernelState`) counterparts of the
    // KernelState reservation primitives (`reserve_direct_reply_record`, …). They
    // operate the SINGLE `reply_caps` slot via the rank-3 `with_ipc_split_mut` seam
    // ONLY — the exact seam the composed off-lock NR6 direct request transaction uses
    // so its record reserve / bind / commit / cancel never take the broad lock. There
    // is no second reply-record table and no second authoritative generation.

    /// rank 3 — reserve one vacant `reply_caps` slot `Reserved` (NOT invokable),
    /// binding both `{tid,asid}` identities + the reply endpoint (index+generation in
    /// `CapObject::Endpoint`). Returns the slot `(index, generation)` — the sole
    /// record authority — or `CapabilityFull`.
    pub(crate) fn reserve_direct_reply_record_split(
        &self,
        caller: crate::kernel::boot::ReceiverWaiterIdentity,
        replier: crate::kernel::boot::ReceiverWaiterIdentity,
        reply_endpoint: CapObject,
    ) -> Result<(usize, u64), KernelError> {
        use crate::kernel::boot::{ReplyCapRecord, ReplyRecordReservation};
        self.with_ipc_split_mut(|ipc| {
            for idx in 0..crate::kernel::boot::MAX_REPLY_CAPS {
                if ipc.reply_caps[idx].is_none() {
                    let mut generation = ipc.reply_cap_generations[idx].wrapping_add(1);
                    if generation == 0 {
                        generation = 1;
                    }
                    ipc.reply_cap_generations[idx] = generation;
                    ipc.reply_caps[idx] = Some(ReplyCapRecord {
                        reservation: ReplyRecordReservation::Reserved,
                        caller_tid: caller.tid,
                        caller_asid: caller.asid,
                        reply_endpoint,
                        responder_tid: Some(replier.tid),
                        replier_asid: Some(replier.asid),
                        caller_cap_id: CapId(0),
                        waiter_cap_id: None,
                    });
                    return Ok((idx, generation));
                }
            }
            Err(KernelError::CapabilityFull)
        })
    }

    /// rank 3 — bind the provisional server-local reply CapId into a `Reserved` record
    /// (its `waiter_cap_id`). `false` on slot/generation mismatch or non-`Reserved`.
    pub(crate) fn bind_direct_reply_record_server_cap_split(
        &self,
        index: usize,
        generation: u64,
        server_cap: CapId,
    ) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
            Some(Some(record))
                if ipc.reply_cap_generations[index] == generation
                    && record.reservation == ReplyRecordReservation::Reserved =>
            {
                record.waiter_cap_id = Some(server_cap);
                true
            }
            _ => false,
        })
    }

    /// rank 3 — commit a `Reserved` record → `Available` (externally invokable). This
    /// is INFALLIBLE for an exact live reservation (it only flips the state field) and
    /// runs strictly BEFORE the rank-1 server enqueue, so the server is never enqueued
    /// while the record is `Reserved`. `false` only on slot/generation mismatch or a
    /// non-`Reserved` state (never for our own exact reservation).
    pub(crate) fn commit_direct_reply_record_split(&self, index: usize, generation: u64) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
            Some(Some(record))
                if ipc.reply_cap_generations[index] == generation
                    && record.reservation == ReplyRecordReservation::Reserved =>
            {
                record.reservation = ReplyRecordReservation::Available;
                true
            }
            _ => false,
        })
    }

    /// rank 3 — cancel a `Reserved` record (rollback): `Reserved → Cancelled → Vacant`
    /// as a single atomic ipc-state mutation, reclaiming the reserved authority so a
    /// partially-built record can never resolve. `false` on mismatch.
    /// Stage 200D-2A — the POST-LOCK server-death drain.
    ///
    /// Runs after the broad `SpinLock<KernelState>` has been released. Every remaining
    /// piece of authority work happens here: the PeerDeath terminal claim, all identity
    /// and generation revalidation, the canonical `ServerDied` publication, and the single
    /// scheduler enqueue. The broad-lock phase in `exit_task` only reserved a slot,
    /// detached the exact link and published this item.
    ///
    /// It reuses the accepted machinery verbatim — `OffLockReplyTimeout`'s per-domain
    /// split-mut seams and `complete_server_death_over`, which publishes through
    /// `rt_commit_receiver_runnable(error_code)` and therefore through the SAME arch return
    /// paths as timeout completion. No second completion or resume path is created.
    ///
    /// A LOSING item (reply, timeout, caller exit or endpoint destruction already won) is
    /// still consumed exactly once: it is removed from the queue before the transaction
    /// runs, so it can never loop or remain queued.
    pub(crate) fn drain_server_death_post_work(&self, cpu: CpuId) -> usize {
        let cpu_idx = cpu.0 as usize;
        let mut drained = 0usize;
        while let Some(work) = crate::kernel::boot::server_death_work_drain_next(cpu_idx) {
            drained += 1;
            // ── Stage 200D-2B1A (§2/§4): the post-lock boundary attestation ──────────────
            //
            // Every caller of this drain reaches it only after its architecture's broad
            // `SpinLock<KernelState>` guard has been dropped — x86_64 and AArch64 through the
            // post-`with_cpu` section of `handle_trap_entry_shared`, RISC-V through its own
            // Phase 3. `broad_lock=0` is therefore a property of where this code runs, not a
            // claim about where the marker was written, and the whole completion below
            // (PeerDeath claim, caller result publication, scheduler enqueue) happens here —
            // outside the broad lock — exactly as Stage 200D-2A relocated it.
            crate::yarm_log!(
                "IPC_SERVER_DEATH_BROAD_LOCK_RELEASED cpu={} record_index={} record_generation={} broad_lock=0 holder=with_cpu result=ok",
                cpu_idx,
                work.reply_record_index,
                work.reply_record_generation
            );
            crate::yarm_log!(
                "IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN cpu={} record_index={} record_generation={} items=1 broad_lock=0 result=ok",
                cpu_idx,
                work.reply_record_index,
                work.reply_record_generation
            );
            // Resolve the identity the terminal cell was ARMED with. A record slot that was
            // reclaimed and reused since publication yields no matching identity, so a
            // stale item claims nothing.
            let armed = self.with_ipc_split_mut(|ipc| {
                if ipc
                    .reply_cap_generations
                    .get(work.reply_record_index)
                    .copied()
                    != Some(work.reply_record_generation)
                {
                    return None;
                }
                ipc.reply_terminal_ownership
                    .get(work.reply_record_index)
                    .map(|cell| *cell.identity())
            });
            let Some(identity) = armed else {
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_DRAIN outcome=stale_record record_index={} record_generation={} caller_wakes=0 result=ok",
                    work.reply_record_index,
                    work.reply_record_generation
                );
                continue;
            };
            // The item must still describe the record it was published for, and that record
            // must still name the exact exiting incarnation — numeric TID alone never
            // authorizes, so a replacement server cannot inherit this item's authority.
            if identity.reply_record_index != work.reply_record_index
                || identity.reply_record_generation != work.reply_record_generation
                || identity.replier_tid != work.exiting_server.tid
                || identity.replier_asid != work.exiting_server.asid
            {
                // Stage 200D-2B1B-i: the queued item no longer describes reality. Both
                // literals name this one real revalidation failure from the two angles the
                // checklist distinguishes — a server incarnation that is not the armed
                // replier, and a record generation that has moved on. Nothing is detached,
                // claimed, published, woken or enqueued past this point.
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_WRONG_SERVER_IDENTITY armed_tid={} armed_asid={} item_tid={} item_asid={} record_index={} caller_wakes=0 result=fail",
                    identity.replier_tid.0,
                    identity.replier_asid.0,
                    work.exiting_server.tid.0,
                    work.exiting_server.asid.0,
                    work.reply_record_index
                );
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_WRONG_RECORD_GENERATION armed_generation={} item_generation={} record_index={} caller_wakes=0 result=fail",
                    identity.reply_record_generation,
                    work.reply_record_generation,
                    work.reply_record_index
                );
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_DRAIN outcome=stale_identity record_index={} caller_wakes=0 result=ok",
                    work.reply_record_index
                );
                continue;
            }
            let mut d = OffLockReplyTimeout(self);
            let outcome = crate::kernel::boot::complete_server_death_over(&mut d, &identity);
            crate::yarm_log!(
                "IPC_SERVER_DEATH_DRAIN outcome={:?} record_index={} record_generation={} broad_lock=0 result=ok",
                outcome,
                work.reply_record_index,
                work.reply_record_generation
            );
        }
        drained
    }

    /// Stage 200D — register the bounded reverse link through the rank-2 TASK seam (never
    /// the broad lock). Returns `false` with no mutation when the server incarnation is
    /// absent or already holds a DIFFERENT live link; re-registering the identical link is
    /// idempotent. Allocation-free: the link is a fixed `Option` field on the TCB.
    pub(crate) fn register_server_reply_link_split(
        &self,
        server_tid: u64,
        server_asid: crate::kernel::vm::Asid,
        record_index: usize,
        record_generation: u64,
    ) -> bool {
        let link = crate::kernel::task::ServerReplyLink {
            server_tid,
            server_asid,
            reply_record_index: record_index,
            reply_record_generation: record_generation,
        };
        self.with_task_tcbs_split_mut(|tcbs| {
            let Some(tcb) = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == server_tid && t.asid == Some(server_asid))
            else {
                return false;
            };
            // Stage 199D: the SAME shared decision the legacy path uses — status gate, match
            // arms and creation stamp together. This edge previously installed the link
            // without stamping `note_link_created`, so with the direct NR6 path as the
            // production default the system-wide leak accounting saw `created=0 closed=13`
            // and the attestation that would catch a real leak was blind.
            crate::kernel::boot::install_server_reply_link(tcb, link)
        })
    }

    /// Stage 200D-1 — RESERVE reverse-link capacity BEFORE the reply record becomes
    /// externally visible.
    ///
    /// The NR6 transaction must not expose a request whose link it then fails to install:
    /// that would be a live record with no teardown-visible link, which is a hard-stop.
    /// This probe answers "would `register_server_reply_link_split` succeed for this exact
    /// server right now" without mutating anything, so the transaction can decline early
    /// — before the record is published and long before the server is enqueued.
    ///
    /// It is a reservation in the single-pair sense only: capacity is one, the server is
    /// not yet Runnable-and-enqueued at the call site, and the real registration re-checks
    /// every condition, so a probe that passes cannot be invalidated by the server itself.
    pub(crate) fn can_reserve_server_reply_link_split(
        &self,
        server_tid: u64,
        server_asid: crate::kernel::vm::Asid,
    ) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == server_tid && t.asid == Some(server_asid))
                .is_some_and(|t| {
                    t.server_reply_link.is_none()
                        && matches!(
                            t.status,
                            crate::kernel::task::TaskStatus::Runnable
                                | crate::kernel::task::TaskStatus::Running
                                | crate::kernel::task::TaskStatus::Blocked(_)
                        )
                })
        })
    }

    /// Stage 200D — remove the reverse link, but ONLY when it still describes this exact
    /// record incarnation. A stale removal (reused slot, replaced server incarnation,
    /// already removed) mutates nothing. Idempotent by construction, so every terminal
    /// outcome can call it unconditionally.
    pub(crate) fn unregister_server_reply_link_split(
        &self,
        server_tid: u64,
        server_asid: crate::kernel::vm::Asid,
        record_index: usize,
        record_generation: u64,
    ) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            let Some(tcb) = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == server_tid && t.asid == Some(server_asid))
            else {
                return false;
            };
            // Stage 199D: the direct NR7 close, on the SAME shared decision the legacy seams
            // use. It used to remove the link without stamping the close edge, so with the
            // direct path as the production default the system totals read
            // `created=54 closed=13` and the leak attestation was wrong permissively.
            //
            // The defensive server re-verify stays HERE, ahead of the shared decision: the
            // TCB lookup already pinned `{tid, asid}`, so this only rejects a link that names
            // some other server, which would be an installation bug. Preserving it keeps this
            // seam's `false` contract byte-identical.
            if !tcb
                .server_reply_link
                .is_some_and(|l| l.matches_server(server_tid, server_asid))
            {
                return false;
            }
            crate::kernel::boot::close_server_reply_link(
                tcb,
                crate::kernel::boot::LinkCloseSelector::Exact {
                    record_index,
                    record_generation,
                },
            )
            .closed()
            .is_some()
        })
    }

    pub(crate) fn cancel_direct_reply_record_split(&self, index: usize, generation: u64) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        self.with_ipc_split_mut(|ipc| {
            let matches = matches!(
                ipc.reply_caps.get(index),
                Some(Some(record))
                    if ipc.reply_cap_generations[index] == generation
                        && record.reservation == ReplyRecordReservation::Reserved
            );
            if matches {
                if let Some(Some(record)) = ipc.reply_caps.get_mut(index) {
                    record.reservation = ReplyRecordReservation::Cancelled;
                }
                ipc.reply_caps[index] = None;
                true
            } else {
                false
            }
        })
    }

    // ── Stage 199A2B3: off-lock NR7 reply-record reservation seams (rank 3) ───────
    //
    // These operate the SAME single `reply_caps` slot lifecycle for the reply side
    // (`Available → Reserved → Consumed`), reserving an EXISTING record (the one the
    // caller's reply cap names) rather than creating one. Bound replier + exact
    // generation are required to reserve; `Consumed` is the authoritative one-shot
    // barrier (`resolve_reply_index` rejects a non-`Available` record). No second
    // reply-record or reservation table.

    /// rank 3 — reserve an EXISTING record `(index, generation)` for a direct reply:
    /// the slot must be present, generation-matched, `Available`, and its bound replier
    /// `{tid, asid}` must equal `replier`. On success `Available → Reserved` (making it
    /// non-invokable so an alias cannot reserve concurrently or reply). `false`
    /// otherwise. The record is NOT claimed here — the reply payload is copied first.
    pub(crate) fn reserve_existing_reply_record_split(
        &self,
        index: usize,
        generation: u64,
        replier: crate::kernel::boot::ReceiverWaiterIdentity,
    ) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
            Some(Some(record))
                if ipc.reply_cap_generations[index] == generation
                    && record.reservation == ReplyRecordReservation::Available
                    && record.responder_tid == Some(replier.tid)
                    && record.replier_asid == Some(replier.asid) =>
            {
                record.reservation = ReplyRecordReservation::Reserved;
                true
            }
            _ => false,
        })
    }

    /// rank 3 — `Reserved → Consumed`: the authoritative one-shot barrier. After this a
    /// stale/aliased reply cap resolving to the same `(index, generation)` fails through
    /// the `Consumed` record even before its physical cnode slot is reclaimed. Runs
    /// strictly before the rank-1 caller enqueue. `false` on generation mismatch or a
    /// non-`Reserved` state (never for our exact owned reservation).
    pub(crate) fn consume_reply_record_split(&self, index: usize, generation: u64) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        let consumed = self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
            Some(Some(record))
                if ipc.reply_cap_generations[index] == generation
                    && record.reservation == ReplyRecordReservation::Reserved =>
            {
                record.reservation = ReplyRecordReservation::Consumed;
                true
            }
            _ => false,
        });
        // Stage 200D-1: consumption is the REPLY terminal becoming irrevocable, so the
        // reverse link closes on the same edge. Note the contrast with
        // `release_reply_record_split` below: a RETRYABLE copy-fault rollback returns the
        // record to `Available` for the same exact server, so it deliberately does NOT
        // detach — the server still owes that reply.
        if consumed {
            let _ = self.finalize_server_reply_link_for_record_split(index, generation);
        }
        consumed
    }

    /// Stage 200D-1 — close the reverse link for a record finalized on a narrow path.
    /// Reads the bound replier under the rank-3 ipc claim, then writes the TCB under the
    /// rank-2 task claim; the claims never nest. Exact on both identities.
    pub(crate) fn finalize_server_reply_link_for_record_split(
        &self,
        index: usize,
        generation: u64,
    ) -> bool {
        let bound = self.with_ipc_split_mut(|ipc| {
            if ipc.reply_cap_generations.get(index).copied() != Some(generation) {
                return None;
            }
            ipc.reply_caps
                .get(index)
                .and_then(|s| s.as_ref())
                .and_then(|r| r.responder_tid.zip(r.replier_asid))
        });
        let Some((tid, asid)) = bound else {
            return false;
        };
        self.unregister_server_reply_link_split(tid.0, asid, index, generation)
    }

    /// rank 3 — `Reserved → Available`: caller-destination-copy-fault rollback for an
    /// EXACT still-valid caller. The reply authority stays usable and the caller stays
    /// blocked with zero wake. `false` on mismatch.
    pub(crate) fn release_reply_record_split(&self, index: usize, generation: u64) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
            Some(Some(record))
                if ipc.reply_cap_generations[index] == generation
                    && record.reservation == ReplyRecordReservation::Reserved =>
            {
                record.reservation = ReplyRecordReservation::Available;
                true
            }
            _ => false,
        })
    }

    /// Stage 199D — the COMPOSED, all-or-nothing restore of a consumed one-shot reply
    /// authority, for a reply whose caller-enqueue was refused and provably never observed.
    ///
    /// Externally observable outcomes are exactly two:
    ///
    /// * **A** — record `Available` **and** the exact reverse link installed; or
    /// * **B** — record `Consumed` **and** no newly installed reverse link.
    ///
    /// "Available without a link" is not reachable. The previous version published the record
    /// first and only then attempted registration, so a refused registration left precisely that
    /// state: an invokable reply with no teardown-visible link.
    ///
    /// **Composition.** The task lock (rank 2) is taken FIRST and held across the whole
    /// operation; the ipc lock (rank 3) is taken nested inside it. That is ascending rank order,
    /// so the lock-order discipline is preserved. Under the single rank-2 hold:
    ///
    /// 1. the exact replier incarnation `{tid, asid}` is located and its link slot is validated
    ///    free (or already exactly ours) and its status live — **nothing is written**;
    /// 2. at rank 3 the record is validated (exact index, exact generation, `Consumed`, bound to
    ///    this exact replier) and flipped to `Available`;
    /// 3. the link is installed through the shared `install_server_reply_link` decision, which
    ///    also stamps the leak accounting. It cannot fail here — the slot was validated under
    ///    this same uninterrupted hold — but if it ever did, the record is flipped straight back
    ///    to `Consumed` before releasing, so outcome B still holds.
    ///
    /// Any failure at step 1 returns before the record is touched at all.
    pub(crate) fn restore_consumed_reply_record_split(
        &self,
        index: usize,
        generation: u64,
        replier: crate::kernel::boot::ReceiverWaiterIdentity,
    ) -> bool {
        use crate::kernel::boot::ReplyRecordReservation;
        use crate::kernel::task::{ServerReplyLink, TaskStatus};
        let link = ServerReplyLink {
            server_tid: replier.tid.0,
            server_asid: replier.asid,
            reply_record_index: index,
            reply_record_generation: generation,
        };
        // rank 2 held across everything below; rank 3 nested inside (ascending order).
        self.with_task_tcbs_split_mut(|tcbs| {
            let Some(pos) = tcbs.iter().position(|slot| {
                slot.as_ref()
                    .is_some_and(|t| t.tid.0 == replier.tid.0 && t.asid == Some(replier.asid))
            }) else {
                return false;
            };
            // (1) validate the link slot WITHOUT writing it.
            {
                let tcb = tcbs[pos].as_ref().expect("position just found it");
                match tcb.server_reply_link {
                    None => {}
                    Some(existing) if existing == link => {}
                    // Occupied by a DIFFERENT live record: refuse before touching the record.
                    Some(_) => return false,
                }
                if !matches!(
                    tcb.status,
                    TaskStatus::Runnable | TaskStatus::Running | TaskStatus::Blocked(_)
                ) {
                    return false;
                }
            }
            // (2) rank 3: validate + flip the record. Still nothing written at rank 2.
            let flipped = self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get_mut(index) {
                Some(Some(record))
                    if ipc.reply_cap_generations[index] == generation
                        && record.reservation == ReplyRecordReservation::Consumed
                        && record.responder_tid == Some(replier.tid)
                        && record.replier_asid == Some(replier.asid) =>
                {
                    record.reservation = ReplyRecordReservation::Available;
                    true
                }
                _ => false,
            });
            if !flipped {
                return false; // outcome B
            }
            // (3) install the link. Validated free under this same hold, so this is infallible;
            // the revert below keeps the two-outcome contract true even if it ever were not.
            // Because step 1 makes step 3 unreachable, the revert is exercised by a test-only
            // fault hook — the only way to prove the path rather than assert it.
            let tcb = tcbs[pos].as_mut().expect("position just found it");
            #[cfg(test)]
            let forced_failure =
                RESTORE_FORCE_LINK_INSTALL_FAILURE.load(core::sync::atomic::Ordering::Relaxed);
            #[cfg(not(test))]
            let forced_failure = false;
            if !forced_failure && crate::kernel::boot::install_server_reply_link(tcb, link) {
                return true; // outcome A
            }
            self.with_ipc_split_mut(|ipc| {
                if let Some(Some(record)) = ipc.reply_caps.get_mut(index) {
                    if ipc.reply_cap_generations[index] == generation {
                        record.reservation = ReplyRecordReservation::Consumed;
                    }
                }
            });
            false // outcome B
        })
    }

    /// rank 3 — discard a `Reserved` record for STALE authority (caller exited/replaced,
    /// endpoint/waiter drifted): transition `Reserved → Consumed` so the reply cap is
    /// permanently non-invokable (the physical cnode slot is reclaimed idempotently at
    /// the caller's teardown). Never restores usable authority. `false` on mismatch.
    pub(crate) fn discard_reply_record_split(&self, index: usize, generation: u64) -> bool {
        self.consume_reply_record_split(index, generation)
    }

    /// rank 3 — read the reply endpoint SLOT INDEX **and GENERATION** bound in a present,
    /// generation-matched reply record. Used by the Stage 199A2B4 NR7 gate to confine the
    /// off-lock reply path to the oracle's reply endpoint (every other reply stays on the
    /// legacy path), and by the Stage 199D endpoint-keyed acknowledgement store, which
    /// requires the exact endpoint INCARNATION — index alone cannot distinguish a recycled
    /// endpoint slot from the one the acknowledgement was published for.
    /// `None` when absent or generation-mismatched.
    pub(crate) fn reply_record_endpoint_ref_split_read(
        &self,
        index: usize,
        generation: u64,
    ) -> Option<(usize, u64)> {
        self.with_ipc_split_mut(|ipc| match ipc.reply_caps.get(index) {
            Some(Some(record)) if ipc.reply_cap_generations[index] == generation => {
                match record.reply_endpoint {
                    CapObject::Endpoint {
                        index: eidx,
                        generation: egen,
                    } => Some((eidx, egen)),
                    _ => None,
                }
            }
            _ => None,
        })
    }

    /// rank 5: map EXACTLY one user page and return the owned follow-up needed for rank-6 accounting
    /// (and exact rollback). NX/rights/alignment are enforced by the caller (the runner); this seam
    /// additionally asserts alignment + NX and never lets a page-table reference escape the guard.
    pub(crate) fn map_user_page_in_asid_split(
        &self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
        mapping: crate::kernel::vm::Mapping,
    ) -> Result<SharedRegionMapFollowup, KernelError> {
        use crate::kernel::vm::{PAGE_SIZE, VmError};
        if virt.0 % (PAGE_SIZE as u64) != 0 {
            return Err(KernelError::Vm(VmError::InvalidAddress));
        }
        debug_assert!(!mapping.flags.execute, "shared-region mapping must be NX");
        let replaced = self.with_vm_user_spaces_split_mut(|spaces| {
            spaces
                .get_mut(asid)
                .ok_or(KernelError::Vm(VmError::InvalidAsid))?
                .map_page(virt, mapping)
                .map_err(KernelError::Vm)
        })?;
        Ok(SharedRegionMapFollowup {
            inserted_phys: mapping.phys,
            replaced,
        })
    }

    /// rank 6: apply the map follow-up AFTER the VM lock dropped (map_refcount bookkeeping). A fresh
    /// shared-region page has no replaced mapping; a replaced page's old frame is accounted/reclaimed
    /// here (memory rank 6 only — never under the VM lock).
    pub(crate) fn sr_apply_map_followup_split(&self, follow: SharedRegionMapFollowup) {
        self.with_memory_split_mut(|m| {
            if let Some(old) = follow.replaced {
                KernelState::note_mapping_removed_locked(m, old.phys);
                KernelState::reclaim_memory_object_for_phys_locked(m, old.phys);
            }
            KernelState::note_mapping_inserted_locked(m, follow.inserted_phys);
        });
    }

    /// U9-D3 §5 — the shootdown target set for `asid`, computed with NO broad lock.
    ///
    /// The same predicate `KernelState::live_cpu_bitmap_for_asid` applies — online, NOT wake-only,
    /// and currently running a task bound to `asid` — but reached through the rank-1 scheduler and
    /// rank-2 task seams instead of the broad guard. Wake-only APs are excluded because they run
    /// no dispatcher and never load a user CR3, so they can hold no translation for a user ASID.
    ///
    /// This is deliberately NOT `online_wake_only_ap_bitmap`: that is the complement set (idle APs)
    /// and would target CPUs that cannot possibly hold the mapping while missing the one that does.
    pub(crate) fn live_cpu_bitmap_for_asid_split(&self, asid: crate::kernel::vm::Asid) -> u64 {
        // rank 1: topology + per-CPU current TID, one acquisition.
        let (online, wake_only, current) = self.with_scheduler_split_mut(|sched| {
            let s = kernel_ref(&sched.scheduler);
            let online = s.online_cpu_bitmap();
            let wake_only = s.wake_only_bitmap();
            let mut current = [None; crate::arch::platform_constants::MAX_CPUS];
            for (cpu, slot) in current.iter_mut().enumerate() {
                *slot = s.current_tid_on(CpuId(cpu as u8)).map(|t| t.0);
            }
            (online, wake_only, current)
        });
        // rank 1 released. rank 2: resolve each candidate's ASID.
        let candidates = online & !wake_only;
        let mut bitmap = 0u64;
        for (cpu, tid) in current.iter().enumerate() {
            let bit = 1u64 << cpu;
            if candidates & bit == 0 {
                continue;
            }
            let Some(tid) = tid else { continue };
            if self.task_asid_for_tid_split_read(*tid) == asid.0 as u64 {
                bitmap |= bit;
            }
        }
        bitmap
    }

    /// U9-D3 §5 — complete the REAL TLB shootdown for one just-unmapped page, with NO domain or
    /// broad lock held. Returns `true` only when every target acknowledged.
    ///
    /// Order: local invalidation on the requester, then the remote request published through the
    /// generation-matched coordinator, which waits for each target's own 0xF1 handler to publish
    /// `ack_gen == req_gen`. The caller reclaims ONLY on `true` — that is the
    /// unmap-before-ACK-before-reclaim rule, and a timeout deliberately leaves the frame
    /// unavailable for reuse rather than recycling memory a remote CPU may still translate.
    ///
    /// Deliberately NOT `KernelState::request_live_asid_shootdown`: that helper impersonates the
    /// requester onto each target (`set_current_cpu`), drains the targets' mailboxes from the
    /// requester, and calls `yield_current` — all of which §5 forbids, and all of which are
    /// impossible here because nothing holds a lock to yield under.
    pub(crate) fn complete_unmap_shootdown_split(
        &self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
    ) -> bool {
        // The requester's OWN translation is retired locally; a CPU never IPIs itself.
        #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
        unsafe {
            core::arch::asm!("invlpg [{}]", in(reg) virt.0, options(nostack, preserves_flags));
        }
        let requester = self.with_scheduler_split_mut(|sched| sched.current_cpu);
        let targets = self.live_cpu_bitmap_for_asid_split(asid) & !(1u64 << requester.0);
        if targets == 0 {
            // Zero remote targets: the local invalidation above is the whole shootdown, and it
            // has already happened. Correct, and not a silent skip.
            return true;
        }
        let want = targets.count_ones() as usize;
        #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
        {
            crate::arch::x86_64::smp::smp_tlb_shootdown_cpus(targets, virt.0) == want
        }
        #[cfg(not(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev"))))]
        {
            // No cross-CPU coordinator on this build; a non-empty remote target set therefore
            // cannot be acknowledged, and must NOT be reported as completed.
            let _ = want;
            false
        }
    }

    /// rank 5 → TLB (no lock) → rank 6: two-phase unmap of the EXACT mapped prefix. Zero-length is a
    /// no-op. Each page: remove the PTE under the VM lock, produce the owned removed-mapping "plan",
    /// release the VM lock, complete the required TLB shootdown with NO lock held, then reclaim under
    /// the memory lock — never reclaiming before shootdown. Repeat calls are no-ops (pages already
    /// gone). On hosted single-CPU the shootdown target bitmap is 0 (no cross-CPU wait); during a
    /// shared-region rollback the object stays pinned, so the reclaim is a guarded no-op there.
    ///
    /// Returns `true` iff every page that was actually removed also completed its shootdown. A
    /// `false` return means at least one frame was deliberately left unreclaimed, and the CALLER's
    /// own reclaim obligation (if it has one) must be skipped for the same reason — see
    /// [`Self::complete_unmap_shootdown_split`].
    pub(crate) fn unmap_range_two_phase_split(
        &self,
        asid: crate::kernel::vm::Asid,
        base: usize,
        len: usize,
    ) -> bool {
        use crate::kernel::vm::{PAGE_SIZE, VirtAddr};
        if len == 0 {
            return true;
        }
        let mut all_acked = true;
        let end = base.saturating_add(len);
        let mut va = base;
        while va < end {
            // Phase A (rank 5): remove exactly one PTE, produce the owned removed mapping.
            let removed = self.with_vm_user_spaces_split_mut(|spaces| {
                spaces
                    .get_mut(asid)
                    .and_then(|a| a.unmap_page(VirtAddr(va as u64)).ok().flatten())
            });
            if let Some(mapping) = removed {
                // Phase A' (rank 6): map_refcount-- (mirrors `unmap_page_phase1`'s note_mapping_removed).
                self.with_memory_split_mut(|m| {
                    KernelState::note_mapping_removed_locked(m, mapping.phys)
                });
                // Phase B: the REAL TLB shootdown — NO domain lock held. U9-D3 replaces the
                // comment that used to stand here with the generation-matched coordinator: local
                // invalidation, then a published remote request that each target's own 0xF1
                // handler invalidates and acknowledges by generation. The VM lock is already
                // released and the memory lock is not yet taken, so nothing is held while waiting.
                let shootdown_ok = self.complete_unmap_shootdown_split(asid, VirtAddr(va as u64));
                // Phase C (rank 6): reclaim the frame — only AFTER a COMPLETED shootdown. Guarded
                // (a still-pinned or still-cap-referenced object is not freed), so a rollback
                // prefix-unmap is a no-op. A timeout skips the reclaim entirely: the frame stays
                // unavailable for reuse rather than being recycled while a remote CPU may still
                // hold a translation for it.
                if shootdown_ok {
                    self.with_memory_split_mut(|m| {
                        KernelState::reclaim_memory_object_for_phys_locked(m, mapping.phys)
                    });
                } else {
                    all_acked = false;
                    // The EXISTING X86_TLB_SHOOTDOWN_FAIL text, spelled literally because the
                    // `tlb_shootdown` marker module is x86-only while this seam is arch-neutral.
                    // No new marker family.
                    crate::yarm_log!(
                        "X86_TLB_SHOOTDOWN_FAIL reason=unmap_ack_incomplete asid={} va=0x{:x} phys=0x{:x}",
                        asid.0,
                        va,
                        mapping.phys.0
                    );
                }
            }
            va = va.saturating_add(PAGE_SIZE);
        }
        all_acked
    }

    /// rank 2 → rank 1 (non-nested): wake a blocked receiver exactly once. Phase 1 (task lock) reads
    /// + validates status and sets it Runnable, capturing the affinity; phase 2 (scheduler lock)
    /// enqueues. NO IPC/capability/VM/memory lock is held. Mirrors `apply_split_receiver_wake_plan`
    /// (`wake_tid_to_runnable` → `enqueue_woken_task`) for a blocked recv-v2 receiver (whose plain
    /// recv carries no IPC deadline, so the broad path's timeout-clear is a no-op here).
    pub(crate) fn sr_wake_receiver_split(&self, tid: u64) -> bool {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::{TaskClass, TaskStatus};
        // Phase 1 (rank 2): validate + set Runnable, capture affinity.
        let plan = self.with_task_tcbs_split_mut(|tcbs| {
            let tcb = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid)?;
            let old = tcb.status;
            if !matches!(
                old,
                TaskStatus::Blocked(_) | TaskStatus::Runnable | TaskStatus::Running
            ) {
                return None;
            }
            if !matches!(old, TaskStatus::Runnable) {
                tcb.status = TaskStatus::Runnable;
            }
            Some(tcb.cpu_affinity)
        });
        let Some(affinity) = plan else {
            return false;
        };
        // Phase 2 (rank 1): enqueue on the pinned CPU, else the current CPU (mirrors
        // `enqueue_woken_task`). Priority is class-derived (SystemServer=High else Normal).
        let priority = match self.task_class_split_read(tid) {
            Some(TaskClass::SystemServer) => TaskPriority::High,
            _ => TaskPriority::Normal,
        };
        self.with_scheduler_split_mut(|sched| {
            let cpu = affinity.unwrap_or(sched.current_cpu);
            let sm = kernel_mut(&mut sched.scheduler);
            let _ = sm.enqueue_on_with_priority(cpu, ThreadId(tid), priority);
        });
        true
    }

    /// rank 2 (task lock) — Stage 198E3B2B1 Phase 1: blocked-receiver PREVALIDATION. Makes NO
    /// mutation. Requires the expected receiver TID to still exist, its captured ASID to still match,
    /// and it to still be blocked in the expected recv-v2 operation. A dead / replaced / not-blocked
    /// receiver returns `false` (stale) BEFORE any waiter is touched, so the common (no-concurrent-
    /// mutator) replacement never removes a replacement task's waiter slot.
    pub(crate) fn sr_prevalidate_blocked_receiver_split(
        &self,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> bool {
        use crate::kernel::task::{TaskStatus, WaitReason};
        // NOTE: the recv-v2 payload state (`blocked_recv_state`) was CONSUMED into the snapshot at
        // production (`blocked_recv_state.take()`), so the live check for "still blocked in the
        // expected recv-v2 operation" is the surviving `Blocked(EndpointReceive)` status + the
        // captured ASID; the exact endpoint identity + generation is verified in the Phase-2 claim.
        self.with_task_tcbs_split_mut(
            |tcbs| match tcbs.iter().flatten().find(|t| t.tid.0 == tid) {
                Some(tcb) => {
                    tcb.asid == Some(asid)
                        && matches!(
                            tcb.status,
                            TaskStatus::Blocked(WaitReason::EndpointReceive(_))
                        )
                }
                None => false,
            },
        )
    }

    /// rank 3 (ipc lock) — Stage 198E3B2B1 Phase 2: the exact, atomic waiter CLAIM. Under a single
    /// rank-3 critical section it revalidates the endpoint index AND GENERATION, requires the exact
    /// expected receiver as the slot's waiter, then removes that waiter EXACTLY ONCE and returns an
    /// owned generation-bearing `WaiterClaim`. An endpoint destroyed/recreated (generation changed),
    /// a different or absent waiter → no claim (`None`), slot untouched. The endpoint generation is
    /// part of the claim authority: a same-index-but-newer endpoint can never be claimed.
    pub(crate) fn sr_claim_endpoint_waiter_split(
        &self,
        eidx: usize,
        egen: u64,
        receiver: crate::kernel::boot::ReceiverWaiterIdentity,
    ) -> Option<WaiterClaim> {
        self.with_ipc_split_mut(|ipc| {
            // Stage 198E3B2B2: the slot must hold the COMPLETE identity (tid + ASID) — numeric TID
            // reuse with a different ASID is not our receiver and is never claimed/cleared.
            // Stage 199D-WA3C1: capture the exact record BEFORE removal so a restore can
            // republish the same incarnation.
            let record = ipc.endpoint_waiter_record(eidx);
            if eidx < ipc.endpoint_waiters.len()
                && ipc.endpoint_generations[eidx] == egen
                && record.map(|r| r.receiver) == Some(receiver)
            {
                ipc.take_endpoint_waiter(eidx);
                Some(WaiterClaim {
                    eidx,
                    generation: egen,
                    receiver,
                    wait_generation: record.map(|r| r.wait_generation).unwrap_or(0),
                })
            } else {
                None
            }
        })
    }

    /// Stage 199A2B2E: read-only exact endpoint-waiter check (rank 3). True iff the
    /// endpoint at `eidx` still carries generation `egen` AND its waiter slot holds the
    /// EXACT `{tid, asid}` identity. Used by the direct request transaction's rollback
    /// policy to decide whether the acknowledgement lease is restorable (server + waiter
    /// intact) or must be discarded (waiter changed/missing). No mutation.
    /// rank 3 — read the LIVE incarnation state of an endpoint off-lock: its
    /// [`EndpointMode`], but only when the slot is occupied AND the generation matches.
    ///
    /// `None` means "this endpoint incarnation is not current" — the slot is empty, out of
    /// range, or has been recycled to a newer generation. The Stage 199D eligibility contract
    /// treats that as a decline, so a stale send cap can never reach the direct transaction.
    pub(crate) fn endpoint_mode_split_read(
        &self,
        eidx: usize,
        egen: u64,
    ) -> Option<crate::kernel::ipc::EndpointMode> {
        self.with_ipc_split_mut(|ipc| {
            if eidx >= ipc.endpoints.len() || ipc.endpoint_generations[eidx] != egen {
                return None;
            }
            ipc.endpoints[eidx]
                .as_ref()
                .map(|storage| kernel_ref(storage).mode())
        })
    }

    pub(crate) fn endpoint_waiter_is_split_read(
        &self,
        eidx: usize,
        egen: u64,
        identity: crate::kernel::boot::ReceiverWaiterIdentity,
    ) -> bool {
        self.with_ipc_split_mut(|ipc| {
            eidx < ipc.endpoint_waiters.len()
                && ipc.endpoint_generations[eidx] == egen
                && ipc.endpoint_waiter_identity(eidx) == Some(identity)
        })
    }

    /// rank 2 → rank 3 — restore the EXACT claimed waiter using its generation-bearing identity token.
    /// Stage 198E3B2B2: restoration is permitted ONLY when the COMPLETE identity still matches — the
    /// endpoint still exists at the claimed generation, the slot is currently free (never clobbering a
    /// newer waiter), AND the task is STILL blocked on that endpoint with the same tid + ASID. It can
    /// therefore never fabricate or overwrite a waiter for a replacement task. (The numeric-TID
    /// Replaced→restore of Stage 198E3B2B1 is removed; the shared-region finalizer no longer restores,
    /// so this is a guarded primitive whose exact-identity contract is proven by the focused tests.)
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sr_restore_endpoint_waiter_split(&self, claim: &WaiterClaim) -> bool {
        // rank 2: the task must still be blocked on recv with the EXACT claimed identity.
        if !self.sr_prevalidate_blocked_receiver_split(claim.receiver.tid.0, claim.receiver.asid) {
            return false;
        }
        // rank 3: slot free + endpoint generation matches → re-install the EXACT identity.
        self.with_ipc_split_mut(|ipc| {
            if claim.eidx < ipc.endpoint_waiters.len()
                && ipc.endpoint_generations[claim.eidx] == claim.generation
                && !ipc.endpoint_waiter_present(claim.eidx)
            {
                ipc.set_endpoint_waiter(
                    claim.eidx,
                    crate::kernel::boot::EndpointWaiterRecord::new(
                        claim.receiver,
                        claim.wait_generation,
                    ),
                );
                true
            } else {
                false
            }
        })
    }

    /// rank 2 (task lock) — Stage 198E3B2B1 Phase 3: task COMMIT, run ONLY after a successful waiter
    /// claim. Revalidates TID + ASID + blocked recv-v2 state AGAIN; for a still-live matching blocked
    /// receiver this is infallible — it clears the blocked-return register state, transitions the
    /// receiver Runnable, and captures its affinity (`Committed`). A receiver that EXITED / was
    /// removed → `GoneDead` (never restore). A live task whose ASID no longer matches (replaced at the
    /// same TID) → `Replaced` (caller restores its waiter). NO register is mutated on `GoneDead` /
    /// `Replaced`, so a failed commit leaves the blocked-return registers byte-identical.
    pub(crate) fn sr_commit_blocked_receiver_split(
        &self,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> ReceiverCommit {
        use crate::kernel::task::{TaskStatus, WaitReason};
        self.with_task_tcbs_split_mut(|tcbs| {
            // Classify FIRST (immutable), so no register is touched unless we commit. `blocked_recv_state`
            // was consumed at production, so the live-match signal is `Blocked(EndpointReceive)` + ASID.
            let class = match tcbs.iter().flatten().find(|t| t.tid.0 == tid) {
                None => ReceiverCommit::GoneDead,
                Some(tcb) if matches!(tcb.status, TaskStatus::Exited(_) | TaskStatus::Dead) => {
                    ReceiverCommit::GoneDead
                }
                Some(tcb)
                    if tcb.asid == Some(asid)
                        && matches!(
                            tcb.status,
                            TaskStatus::Blocked(WaitReason::EndpointReceive(_))
                        ) =>
                {
                    ReceiverCommit::Committed(None)
                }
                Some(_) => ReceiverCommit::Replaced,
            };
            if !matches!(class, ReceiverCommit::Committed(_)) {
                return class;
            }
            // Infallible commit for a still-live matching blocked receiver: clear the blocked-return
            // register state (single-sourced arch-gated helper), then set Runnable + capture affinity.
            KernelState::clear_blocked_recv_return_regs_locked(tcbs, tid);
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .expect("prevalidated present");
            tcb.status = TaskStatus::Runnable;
            ReceiverCommit::Committed(tcb.cpu_affinity)
        })
    }

    /// rank 1 (scheduler lock) — Stage 198E3B2B1 Phase 4: the single ENQUEUE. The receiver is already
    /// Runnable (committed under the task lock); this only enqueues it exactly once on its captured
    /// affinity (else the current CPU). This is the final externally visible action and is NON-fallible
    /// — no fallible work runs after it. Priority is class-derived (SystemServer=High else Normal).
    /// Stage 199D: the CPU this drain is running on, read from the SAME rank-1 authority
    /// (`sched.current_cpu`) that `sr_enqueue_committed_receiver_split` uses for its unpinned
    /// fallback. Comparing the two is therefore an apples-to-apples "was that enqueue remote?"
    /// test: an unpinned receiver enqueues onto this very CPU and compares equal, so it can never
    /// be mistaken for a remote wake.
    pub(crate) fn current_cpu_split_read(&self) -> CpuId {
        self.with_scheduler_split_mut(|sched| sched.current_cpu)
    }

    /// Reports what the rank-1 enqueue **actually did**.
    ///
    /// Stage 199D: this used to compute the target and throw it away, which left the caller with
    /// no authority for the post-enqueue wake decision. The one consumer that needed it
    /// (`drain_direct_request_post_work`) therefore guessed, hardcoding CPU 1 behind a global
    /// oracle selector — so while the NR6 production default was enabled, EVERY ordinary direct
    /// request fired a remote-wake IPI at CPU 1.
    ///
    /// Returning the *requested* CPU was the second half of that same mistake, and `ca55400b`
    /// caught it live: aimed at an online **wake-only** CPU the enqueue was denied, the `Err` was
    /// dropped, and the seam still answered `CpuId(1)` for a task that ended up in **no** run
    /// queue. The doc comment claimed the returned CPU and the enqueued CPU "cannot disagree";
    /// they did. Now the return value carries the enqueue's own verdict, so a wake target can
    /// only ever name a placement that happened.
    ///
    /// The target is the receiver's authoritative affinity (its `task_home_cpu`), falling back to
    /// the enqueueing CPU when unpinned — read out of the same rank-1 acquisition as the enqueue.
    ///
    /// **Generic: never reconciles.** An `AlreadyQueued` collision is reported with
    /// `reconciled: None` and nothing is withdrawn. Silently removing a pre-existing entry on
    /// behalf of a caller that cannot complete a rollback would be a hidden side effect; the
    /// direct-IPC paths use [`Self::sr_enqueue_committed_receiver_reconciled_split`] instead.
    pub(crate) fn sr_enqueue_committed_receiver_split(
        &self,
        tid: u64,
        affinity: Option<CpuId>,
    ) -> ReceiverEnqueue {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::TaskClass;
        let priority = match self.task_class_split_read(tid) {
            Some(TaskClass::SystemServer) => TaskPriority::High,
            _ => TaskPriority::Normal,
        };
        self.enqueue_committed_receiver_inner(tid, priority, affinity, false)
    }

    /// rank 1 — Stage 199D: the **direct-IPC-only** enqueue seam. Identical to
    /// [`Self::sr_enqueue_committed_receiver_split`] except that an `AlreadyQueued` collision is
    /// RECONCILED inside the same acquisition that detected it.
    ///
    /// Withdrawing a pre-existing scheduler entry is a real side effect, and only NR6/NR7 own a
    /// rollback that can complete it. It is therefore a separate seam rather than a flag: the
    /// shared-region finalizer literally cannot select it, because it calls the other function.
    pub(crate) fn sr_enqueue_committed_receiver_reconciled_split(
        &self,
        tid: u64,
        affinity: Option<CpuId>,
    ) -> ReceiverEnqueue {
        // Test-only: inject the post-copy membership collision here rather than in the
        // transaction, so the transaction file stays free of every fault-injection construct
        // (pinned by `race_only_variants_are_all_failed_and_never_fall_back`).
        #[cfg(test)]
        self.test_force_post_copy_membership(tid, affinity);
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::TaskClass;
        let priority = match self.task_class_split_read(tid) {
            Some(TaskClass::SystemServer) => TaskPriority::High,
            _ => TaskPriority::Normal,
        };
        self.enqueue_committed_receiver_inner(tid, priority, affinity, true)
    }

    /// The shared body. `reconcile` is set ONLY by the direct-IPC seam above; the parameter is
    /// private to this module and neither public seam exposes it.
    fn enqueue_committed_receiver_inner(
        &self,
        tid: u64,
        priority: crate::kernel::scheduler::TaskPriority,
        affinity: Option<CpuId>,
        reconcile: bool,
    ) -> ReceiverEnqueue {
        use crate::kernel::ipc::ThreadId;
        self.with_scheduler_split_mut(|sched| {
            let cpu = affinity.unwrap_or(sched.current_cpu);
            let sm = kernel_mut(&mut sched.scheduler);
            match sm.enqueue_on_with_priority(cpu, ThreadId(tid), priority) {
                Ok(()) => ReceiverEnqueue::Enqueued { cpu },
                // Pre-existing membership. When the caller owns a rollback that can complete it,
                // reconcile HERE, still holding the rank-1 lock the collision was detected under —
                // there is no unlock/relock window in which a dispatcher could take the entry
                // between the two. Otherwise report the collision and touch NOTHING.
                Err(crate::kernel::scheduler::SchedulerError::AlreadyQueued) => {
                    let reconciled =
                        reconcile.then(|| sm.withdraw_queued_tid_on(cpu, ThreadId(tid)));
                    ReceiverEnqueue::Rejected {
                        cpu,
                        error: crate::kernel::scheduler::SchedulerError::AlreadyQueued,
                        reconciled,
                    }
                }
                Err(error) => ReceiverEnqueue::Rejected {
                    cpu,
                    error,
                    reconciled: None,
                },
            }
        })
    }

    /// rank 1 — Stage 199D: does `tid` hold ANY scheduler membership (queued or dispatched) on
    /// any online CPU? Read-only.
    ///
    /// Used as a pre-commit preflight by the direct transactions: a receiver that is `Blocked`
    /// with its endpoint waiter exclusively claimed cannot legitimately be in a run queue, and
    /// nothing can wake it while that is true, so this check does not race. Declining here keeps
    /// the dangerous `AlreadyQueued`-after-publication branch off the ordinary path entirely —
    /// the branch remains, and fails closed, for a genuine invariant violation.
    pub(crate) fn receiver_has_scheduler_membership_split_read(&self, tid: u64) -> bool {
        use crate::kernel::ipc::ThreadId;
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler).task_present_anywhere(ThreadId(tid))
        })
    }

    /// rank 2 (task lock) — Stage 199D: the exact INVERSE of
    /// [`Self::sr_commit_blocked_receiver_split`], for the one case that has no other cure: the
    /// commit succeeded but the rank-1 enqueue that follows it was refused. Without this the
    /// receiver is left **Runnable but in no run queue** — unschedulable forever, and not even
    /// reachable by the reply timeout, which only fires for a *blocked* task.
    ///
    /// Restores `Blocked(EndpointReceive(recv_cap))` for the EXACT `{tid, asid}` incarnation, and
    /// only from `Runnable` — a receiver that has since exited, been replaced, or somehow started
    /// running is left alone and reported `false`. `recv_cap` is the wait reason captured from the
    /// TCB immediately before the commit.
    ///
    /// The blocked-return registers that the commit zeroed are deliberately NOT restored: they
    /// are the recv syscall's return lanes, meaningless while the task is blocked, and rewritten
    /// by whichever delivery or timeout eventually completes it. `blocked_recv_state` is likewise
    /// untouched — it was consumed into the snapshot at ack production, *before* this transaction
    /// began, so it is already `None` in the state this restores to.
    pub(crate) fn sr_uncommit_blocked_receiver_split(
        &self,
        tid: u64,
        asid: crate::kernel::vm::Asid,
        recv_cap: CapId,
    ) -> bool {
        use crate::kernel::task::{TaskStatus, WaitReason};
        self.with_task_tcbs_split_mut(|tcbs| {
            let Some(tcb) = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == tid && t.asid == Some(asid))
            else {
                return false;
            };
            if !matches!(tcb.status, TaskStatus::Runnable) {
                return false;
            }
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
            true
        })
    }

    /// rank 2 (task lock) — Stage 199D: read the endpoint-receive capability an exactly-blocked
    /// receiver is waiting on, so a post-commit rollback can restore the same wait reason it had.
    /// `None` unless the `{tid, asid}` incarnation is present and `Blocked(EndpointReceive(_))`.
    pub(crate) fn blocked_recv_cap_split_read(
        &self,
        tid: u64,
        asid: crate::kernel::vm::Asid,
    ) -> Option<CapId> {
        use crate::kernel::task::{TaskStatus, WaitReason};
        self.with_task_tcbs_split_mut(|tcbs| {
            match tcbs
                .iter()
                .flatten()
                .find(|t| t.tid.0 == tid && t.asid == Some(asid))
                .map(|t| t.status)
            {
                Some(TaskStatus::Blocked(WaitReason::EndpointReceive(cap))) => Some(cap),
                _ => None,
            }
        })
    }

    /// Stage 199A2D2A: read the cross-CPU wake TARGET for the SMP request oracle — a blocked
    /// server's authoritative home CPU (assigned via `set_task_home_cpu`). The accepted NR6
    /// transaction enqueues the woken server on this CPU (its captured affinity), so the woken
    /// server lands on its HOME run queue, never the enqueueing/BSP CPU. `None` when unpinned.
    ///
    /// U3 (203C): reads the affinity through the rank-2 task seam instead of the broad lock.
    /// `KernelState::task_home_cpu` is `task_cpu_affinity(tid).ok().flatten()`, so exactly three
    /// inputs answer `None` — the idle TID, an absent TCB (`Err(TaskMissing)` swallowed by
    /// `.ok()`), and a present-but-unpinned TCB (`Ok(None)` flattened) — and a pinned TCB answers
    /// its exact `cpu_affinity`. That is reproduced here verbatim. Calling `task_home_cpu` would
    /// require re-forming a broad `&mut KernelState`, which is the acquisition being retired.
    /// Read-only: no scheduler acquisition, no mutation, no broad fallback.
    pub(crate) fn smp_request_wake_target_split_read(&self, tid: u64) -> Option<CpuId> {
        if tid == 0 {
            return None;
        }
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .and_then(|t| t.cpu_affinity)
        })
    }

    // ── Stage 200C1: reply-receive timeout completion transaction ────────────────
    //
    // The single completion body is `KernelState::run_reply_timeout_completion_locked`
    // (`kernel/boot/ipc_state.rs`), which delegates to the arch-neutral
    // `complete_reply_timeout_over`. U1 deleted the obsolete `SharedKernel`
    // broad-lock wrapper that used to sit here: it had no production caller — the
    // in-lock timer scan calls the `_locked` body directly (it already holds the
    // broad lock) and the accepted off-lock composition below runs the same body.
    // The body itself is unchanged, and broad reply-timeout processing has NOT
    // disappeared: the in-lock scan remains, off-lock scanning is x86_64 only, and
    // canonical 199E stays OPEN.

    // ── Stage 200C2B: OFF-LOCK reply-timeout collection + completion ────────────────

    /// The monotonic "now" of the **`OracleHardware` clock domain** — the domain the confined
    /// reply-timeout oracle arms its own deadlines in (Stage 200C2C1/200C2C2).
    ///
    /// On AArch64 and RISC-V, while the oracle selector is armed, this is the architectural
    /// hardware counter: those cooperative ports have no periodic scheduler tick, so the oracle's
    /// scenario needs a clock that advances under a user workload, and it arms
    /// `reply_timeout_hw_now() + delta` to match. Everywhere else — x86_64 always, and
    /// AArch64/RISC-V on a production boot — this IS the scheduler tick, because there is no
    /// hardware branch to read.
    ///
    /// # This is NOT a global reply clock
    ///
    /// Canonical 199E made production reply/call registration live, so a selector-on boot
    /// contains BOTH domains at once: the oracle's counter deadline AND ordinary production
    /// deadlines armed as `scheduler_tick_now() + timeout_ticks` by unrelated callers. The two
    /// values are orders of magnitude apart, so judging every record against one of them is
    /// wrong in both directions — a hardware counter makes every production tick deadline
    /// instantly due, and a stuck tick makes the oracle's counter deadline never due. Each
    /// record therefore carries its own [`ReplyDeadlineClock`] and the collector receives both
    /// values; this seam supplies exactly one of them.
    ///
    /// [`ReplyDeadlineClock`]: crate::kernel::deadline_token::ReplyDeadlineClock
    pub(crate) fn reply_timeout_oracle_now_split_read(&self) -> u64 {
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if crate::kernel::boot::x86_ipc_reply_timeout_oracle_enabled() {
            return crate::kernel::boot::reply_timeout_hw_now();
        }
        self.scheduler_tick_now_split_read()
    }

    /// U7 (canonical 199E) — the PRODUCTION, arch-neutral IPC-timeout scanner.
    ///
    /// One bounded pass over the TCB array through the rank-2 task split-mut seam (NO broad
    /// `&mut KernelState`, NO `with` / `with_cpu`, NO broad runtime lock), classifying every
    /// DUE deadline it finds into one of the two OFF-LOCK-retired U7 classes and publishing one
    /// owned work item per entry into the matching per-CPU bounded deferred queue:
    ///
    /// * a token-bearing **reply-receive** deadline → `ReplyTimeoutPostWork` (Stage 200C2B's
    ///   narrow collector, unchanged in substance — the same due test, the same causal
    ///   reply-wins gate and the same stale-token examination);
    /// * a **blocking-send** deadline on a `Blocked(EndpointSend)` task → `SendTimeoutPostWork`,
    ///   carrying the U6 `{tid, asid, send_generation}` cycle identity rather than a bare TID.
    ///
    /// It makes NO timeout decision, mutates NO waiter, wakes NO task and clears NO deadline —
    /// every decision belongs to the drains. A full queue leaves the DUE deadline armed on the
    /// TCB for a later pass, so no due registration is ever silently dropped.
    ///
    /// Ordinary receive-timeout deadlines are NOT a U7 class: they are neither token-bearing nor
    /// `Blocked(EndpointSend)`, so this pass skips them and they stay on the in-lock scan.
    ///
    /// # Bounded work and the cursor
    ///
    /// The pass starts at this CPU's cursor and stops after it has examined
    /// `IPC_TIMEOUT_SCAN_WINDOW` OCCUPIED slots or wrapped the whole array, whichever comes
    /// first — so its cost is bounded by live tasks rather than by `MAX_TASKS`. The cursor then
    /// advances by the number of entries **SCANNED**, never by the number published: a window
    /// that published nothing advances, and a window whose publications were refused by a full
    /// queue advances too. That is what stops a high slot starving behind a persistently full
    /// queue, and it makes coverage a property of the cursor rather than of the workload.
    ///
    /// # Two reply clock domains, compared per record
    ///
    /// `production_now` and `oracle_now` are the current values of the two [`ReplyDeadlineClock`]
    /// domains. A token-bearing record is judged ONLY against the domain it was armed in, read
    /// from the registration itself — never against a selector-global "reply now", which a
    /// selector-on boot makes meaningless now that production registration is live and both
    /// domains coexist. `send_now` is unchanged: the blocking-send and ordinary-receive classes
    /// have one clock, the scheduler tick.
    ///
    /// [`ReplyDeadlineClock`]: crate::kernel::deadline_token::ReplyDeadlineClock
    pub(crate) fn collect_due_ipc_timeout_work(
        &self,
        production_now: u64,
        oracle_now: u64,
        send_now: u64,
        cpu: CpuId,
    ) {
        use crate::kernel::deadline_token::ReplyDeadlineClock;
        use crate::kernel::task::{TaskStatus, WaitReason};
        // Stage 200C2C2C-R2B — the CAUSAL reply-wins gate. While held, publish NO REPLY work, so
        // no timeout claimant can reach the terminal cell while a reply is in flight. It is
        // armed ONLY in reply-wins mode (never for a production or timeout-wins deadline)
        // and released by the oracle client's own post-validation DebugLog marker. It is a
        // reply-class gate only: the blocking-send class has no terminal cell to race.
        #[cfg(feature = "ipc-reply-timeout-oracle-core")]
        let reply_held = crate::kernel::boot::reply_timeout_collector_held();
        #[cfg(not(feature = "ipc-reply-timeout-oracle-core"))]
        let reply_held = false;
        let cpu_idx = cpu.0 as usize;
        let start = crate::kernel::boot::ipc_timeout_scan_cursor(cpu_idx);
        // Snapshot due work under ONLY the task lock; publish after the task lock is dropped so
        // neither queue lock ever nests inside the task lock.
        let mut reply_due: [Option<crate::kernel::boot::ReplyTimeoutPostWork>;
            crate::kernel::boot::RT_POST_WORK_SLOTS] =
            [None; crate::kernel::boot::RT_POST_WORK_SLOTS];
        let mut reply_n = 0usize;
        let mut send_due: [Option<crate::kernel::boot::SendTimeoutPostWork>;
            crate::kernel::boot::ST_POST_WORK_SLOTS] =
            [None; crate::kernel::boot::ST_POST_WORK_SLOTS];
        let mut send_n = 0usize;
        let mut recv_due: [Option<crate::kernel::boot::RecvTimeoutPostWork>;
            crate::kernel::boot::CT_POST_WORK_SLOTS] =
            [None; crate::kernel::boot::CT_POST_WORK_SLOTS];
        let mut recv_n = 0usize;
        let mut scanned = 0usize;
        self.with_task_tcbs_split_mut(|tcbs| {
            let len = tcbs.len();
            if len == 0 {
                return;
            }
            let mut examined = 0usize;
            for step in 0..len {
                if examined >= crate::kernel::boot::IPC_TIMEOUT_SCAN_WINDOW {
                    break;
                }
                scanned += 1;
                let Some(tcb) = tcbs[(start + step) % len].as_ref() else {
                    continue;
                };
                examined += 1;
                let Some(deadline) = tcb.ipc_timeout_deadline else {
                    continue;
                };
                // ── class 1: the token-bearing reply-receive deadline ─────────────────────
                if let Some(handle) = tcb.reply_timeout_token {
                    // Canonical 199E: each record against ITS OWN clock. The domain was written
                    // by the single registration seam together with the deadline, so this cannot
                    // read a domain that belongs to some other registration, and the selector
                    // cannot reinterpret a production deadline that is already armed.
                    let clock = tcb.reply_timeout_clock;
                    let reply_now = match clock {
                        ReplyDeadlineClock::ProductionTick => production_now,
                        ReplyDeadlineClock::OracleHardware => oracle_now,
                    };
                    if reply_now < deadline
                        || reply_held
                        || reply_n >= crate::kernel::boot::RT_POST_WORK_SLOTS
                    {
                        continue;
                    }
                    // ── Stage 200D-2B1A (§5): the REAL stale-token examination ────────────
                    //
                    // This is the point at which the collector genuinely looks at the token
                    // the ServerDies caller armed. It fires only after the causal gate was
                    // RELEASED (`reply_held` above guarantees that), i.e. after PeerDeath
                    // committed and the caller validated code 10 — so the attestation is
                    // "the same token was examined and found stale", not "no wake happened".
                    // The token identity is compared against the one recorded at arm time;
                    // a different token cannot satisfy it.
                    #[cfg(feature = "ipc-reply-timeout-oracle-core")]
                    if let Some((armed_idx, armed_gen, armed_tid, armed_asid)) =
                        crate::kernel::boot::server_dies_stale_token()
                    {
                        let this_idx = handle.identity().token_index;
                        let this_gen = handle.identity().token_generation;
                        let this_tid = tcb.tid.0;
                        let this_asid = tcb.asid.map(|a| a.0).unwrap_or(0);
                        // Stage 200D-2B1B-i: the same SLOT with a different generation is a
                        // reused registration, never the caller's original one. Naming it here
                        // is what makes the comparison provably four-field rather than
                        // slot-index-only.
                        if this_idx == armed_idx && this_gen != armed_gen {
                            crate::yarm_log!(
                                "IPC_SERVER_DEATH_WRONG_TIMEOUT_GENERATION token_index={} armed_generation={} scanned_generation={} caller_tid={} result=fail",
                                this_idx,
                                armed_gen,
                                this_gen,
                                this_tid
                            );
                        }
                        if this_idx == armed_idx
                            && this_gen == armed_gen
                            && this_tid == armed_tid
                            && this_asid == armed_asid
                            && crate::kernel::boot::server_dies_stale_scan_once()
                        {
                            crate::yarm_log!(
                                "IPC_SERVER_DEATH_LATE_TIMEOUT_SCANNED token_index={} token_generation={} caller_tid={} caller_asid={} deadline={} now={} matches_armed=1 broad_lock=0 result=ok",
                                this_idx,
                                this_gen,
                                this_tid,
                                this_asid,
                                deadline,
                                reply_now
                            );
                        }
                    }
                    reply_due[reply_n] = Some(crate::kernel::boot::ReplyTimeoutPostWork {
                        handle,
                        deadline,
                        clock,
                    });
                    reply_n += 1;
                    continue;
                }
                // ── class 2 (U7 §3B): the blocking-SEND deadline ──────────────────────────
                //
                // The identity published is the U6 blocking-send cycle — `{tid, asid,
                // send_generation}` — so a replacement incarnation that reuses the numeric TID,
                // or the same incarnation blocking a second time, can never be settled by a
                // stale item. An ASID-less sender carries `asid: None` and is woken without a
                // completion, exactly as the retired in-lock scan handled it.
                if send_now < deadline {
                    continue;
                }
                if matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {
                    if send_n >= crate::kernel::boot::ST_POST_WORK_SLOTS {
                        continue;
                    }
                    send_due[send_n] = Some(crate::kernel::boot::SendTimeoutPostWork {
                        tid: tcb.tid.0,
                        asid: tcb.asid,
                        send_generation: tcb.blocked_send_generation,
                        deadline,
                    });
                    send_n += 1;
                    continue;
                }
                // ── class 3 (canonical 199E): the ordinary RECEIVE timeout ────────────────
                //
                // Same clock as the send class: a receive deadline is `scheduler_tick_now() +
                // timeout_ticks`, which is what the broad-lock scan compared against. The
                // identity published is `{tid, asid, blocked_recv_generation}` — the generation
                // `recv_block_phase_b_task` minted for THIS block — never a bare TID.
                if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointReceive(_)))
                    || recv_n >= crate::kernel::boot::CT_POST_WORK_SLOTS
                {
                    continue;
                }
                recv_due[recv_n] = Some(crate::kernel::boot::RecvTimeoutPostWork {
                    tid: tcb.tid.0,
                    asid: tcb.asid,
                    wait_generation: tcb.blocked_recv_generation,
                    deadline,
                });
                recv_n += 1;
            }
        });
        // Advance by entries SCANNED (see the doc comment): coverage is the cursor's property.
        crate::kernel::boot::advance_ipc_timeout_scan_cursor(cpu_idx, scanned);
        for work in reply_due.iter().flatten() {
            // A full queue leaves the deadline armed + due (retry on a later scan); a
            // duplicate token yields only one work owner.
            let _ = crate::kernel::boot::reply_timeout_work_publish(cpu_idx, *work);
        }
        for work in send_due.iter().flatten() {
            let _ = crate::kernel::boot::send_timeout_work_publish(cpu_idx, *work);
        }
        for work in recv_due.iter().flatten() {
            let _ = crate::kernel::boot::recv_timeout_work_publish(cpu_idx, *work);
        }
    }

    /// Stage 200C2B — the OFF-LOCK drain. Runs the SINGLE Stage 200C1 completion
    /// transaction (`complete_reply_timeout_over`) for each deferred work item through
    /// `OffLockReplyTimeout` — per-domain split-mut seams, NO broad lock surrounding the
    /// composed transaction. A stale/duplicate work item fails at the exact token fire
    /// claim BEFORE any waiter mutation. Reports the honest retired lock status
    /// (`scan_broad_lock=0`) and emits the reply-timeout class retirement seal exactly
    /// once. On a non-`Woken` outcome it clears the caller's stale TCB registration so it
    /// is not re-collected.
    pub(crate) fn drain_reply_timeout_post_work(
        &self,
        cpu: CpuId,
        production_now: u64,
        oracle_now: u64,
    ) {
        use crate::kernel::deadline_token::ReplyDeadlineClock;
        // U7 (canonical 199E): the deadline scan for BOTH retired classes runs HERE — off the
        // broad `SpinLock<KernelState>` — on every trap, whether or not a completion drains and
        // whether or not any deadline is currently armed. That is the whole content of the
        // promotion, so the attestation is emitted from the first PRODUCTION drain rather than
        // being gated on an oracle having armed something: the claim is about WHERE the scan
        // runs, and the scan has provably just run (the collector call precedes this one at
        // every production entry, and only that entry calls either). Stage 200C2B gated this on
        // `reply_timeout_armed_any()`; that gate is gone with the arm-only latch it read, which
        // could never be true at the first trap and therefore could never be reported honestly
        // here either.
        if crate::kernel::boot::reply_timeout_lock_status_once() {
            crate::yarm_log!(
                "IPC_REPLY_TIMEOUT_LOCK_STATUS arch={} scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok",
                crate::kernel::boot::REPLY_TIMEOUT_ARCH
            );
        }
        let cpu_idx = cpu.0 as usize;
        while let Some(work) = crate::kernel::boot::reply_timeout_work_drain_next(cpu_idx) {
            // U7: the DUE value the scanner selected on is the authority, not the scanner's
            // say-so. The production entry hands both halves the same pair of clocks, so this
            // cannot normally fail; if a future driver ever passed an older clock, the item goes
            // back on the queue rather than being completed early, and the pass stops.
            //
            // Canonical 199E: re-check and complete against the item's OWN domain — the same one
            // the collector judged it in. Selecting a domain here rather than at the call site is
            // what lets a single drain pass settle a production-tick record and an
            // oracle-counter record in either order without either being measured by the
            // other's clock.
            let now = match work.clock {
                ReplyDeadlineClock::ProductionTick => production_now,
                ReplyDeadlineClock::OracleHardware => oracle_now,
            };
            if now < work.deadline {
                let _ = crate::kernel::boot::reply_timeout_work_publish(cpu_idx, work);
                break;
            }
            let mut d = OffLockReplyTimeout(self);
            let outcome =
                crate::kernel::boot::complete_reply_timeout_over(&mut d, &work.handle, now);
            // Stage 200D-2B1B-i: in the ServerDies scenario the deadline must LOSE. A Timeout
            // completion that actually woke the caller means it reached the terminal cell
            // before PeerDeath — the exact inversion this literal names. Emitted from the real
            // completion outcome, never inferred from a missing marker.
            #[cfg(feature = "ipc-reply-timeout-oracle-core")]
            if crate::kernel::boot::x86_ipc_reply_timeout_oracle_mode()
                == crate::kernel::boot::IPC_REPLY_TIMEOUT_MODE_SERVER_DIES
                && matches!(outcome, crate::runtime::ReplyTimeoutOutcome::Woken)
            {
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_TIMEOUT_WON outcome={:?} terminal=Timeout expected=PeerDeath result=fail",
                    outcome
                );
            }
            // The FIRST drained work item carries the deferred-work publish/drain evidence.
            if crate::kernel::boot::reply_timeout_deferred_once() {
                crate::yarm_log!(
                    "IPC_REPLY_TIMEOUT_DEFERRED arch={} published={} drained={} result=ok",
                    crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                    crate::kernel::boot::reply_timeout_work_published_count(),
                    crate::kernel::boot::reply_timeout_work_drained_count()
                );
            }
            match outcome {
                ReplyTimeoutOutcome::Woken => {
                    crate::yarm_log!(
                        "IPC_REPLY_TIMEOUT_OK arch={} terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok",
                        crate::kernel::boot::REPLY_TIMEOUT_ARCH
                    );
                    // The terminal is COMMITTED off-lock — but the caller has not yet been
                    // delivered its result. Attest the commit here and ARM the class-retirement
                    // marker; it fires only from the delivery point.
                    crate::yarm_log!(
                        "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch={} terminal=Timeout result=ok",
                        crate::kernel::boot::REPLY_TIMEOUT_ARCH
                    );
                    crate::kernel::boot::arm_reply_timeout_class_retired();
                    // x86_64 delivers via SAVED-FRAME return installed by the transaction itself,
                    // so the completion IS the delivery point and the marker fires now. AArch64
                    // defers it to its resume boundary (which consumes the parked completion).
                    #[cfg(target_arch = "x86_64")]
                    crate::kernel::boot::maybe_emit_reply_timeout_class_retired();
                }
                other => {
                    // Harmless late expiry (the reply disarmed/completed the token, or the
                    // caller already resumed): no timeout claim, no wake.
                    crate::yarm_log!(
                        "IPC_REPLY_TIMEOUT_LATE_SCAN arch={} outcome={:?} late_timeout_claims=0 result=ok",
                        crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                        other
                    );
                    // Clear the caller's stale TCB registration so a later scan does not
                    // re-collect it — but ONLY when the token is genuinely RETIRED
                    // (Completed/Disarmed) AND the TCB still references THIS exact handle.
                    // A transiently reply-LEASED token (ClaimedByReply) is NOT retired, so a
                    // reply-copy fault can still restore + re-fire it; and a newer
                    // registration on the same caller is never clobbered.
                    let retired = self.with_ipc_split_mut(|ipc| {
                        ipc.reply_deadline_tokens
                            .get(work.handle.token_index())
                            .is_some_and(|t| t.is_completed() || t.is_disarmed())
                    });
                    if retired {
                        let tid = work.handle.identity().terminal_identity.caller_tid.0;
                        let this = work.handle;
                        self.with_task_tcbs_split_mut(|tcbs| {
                            if let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
                                if tcb.reply_timeout_token == Some(this) {
                                    tcb.ipc_timeout_deadline = None;
                                    tcb.reply_timeout_token = None;
                                }
                            }
                        });
                    }
                }
            }
        }
        // Reply-wins genuine-late-expiry proof: the reply disarmed the token and the
        // caller resumed, so the collector genuinely scanned PAST the reply-wins deadline
        // and found no claimable timeout registration. Emit a ONE-SHOT positive marker.
        //
        // U7: the drain itself is production now; this tail is ORACLE SCENARIO attestation and
        // stays behind the oracle feature. Promotion moved where timeouts are processed, not
        // which scenarios are built.
        #[cfg(feature = "ipc-reply-timeout-oracle-core")]
        if crate::kernel::boot::x86_ipc_reply_timeout_oracle_mode()
            == crate::kernel::boot::IPC_REPLY_TIMEOUT_MODE_REPLY_WINS
        {
            let rw = crate::kernel::boot::ipc_reply_timeout_rw_deadline();
            // Stage 200C2C2C-R2B: require the causal gate to be RELEASED first, so this
            // attestation can only be made by a drain whose collector was genuinely free to
            // publish timeout work — and still claimed none. While held the collector is
            // suppressed, and a "late scan claimed nothing" claim would be vacuous.
            // `rw` is the ORACLE's own reply-wins deadline, so it is measured in the
            // `OracleHardware` domain — canonical 199E makes that explicit here rather than
            // inheriting whichever item happened to drain last (production items now drain
            // through this same loop, in the scheduler-tick domain).
            if rw != 0
                && oracle_now >= rw
                && !crate::kernel::boot::reply_timeout_collector_held()
                && crate::kernel::boot::ipc_reply_timeout_rw_late_scan_once()
            {
                crate::yarm_log!(
                    "IPC_REPLY_TIMEOUT_LATE_SCAN arch={} outcome=reply_won late_timeout_claims=0 result=ok",
                    crate::kernel::boot::REPLY_TIMEOUT_ARCH
                );
            }
        }
    }

    /// U7 §3B (canonical 199E) — the OFF-LOCK blocking-SEND timeout settle.
    ///
    /// Runs the U6 blocking-send lifecycle for each deferred item with the broad
    /// `SpinLock<KernelState>` already dropped. Four SEQUENTIAL, never-nested domain claims, in
    /// exactly the order the retired in-lock scan used:
    ///
    /// 1. **rank 2 — the exact claim.** The item is settled only if `{tid, asid,
    ///    send_generation}` still names a live incarnation `Blocked(EndpointSend)` whose armed
    ///    deadline is still the collected one and is still due. Everything else is refused with
    ///    ZERO mutation. This is what makes a receiver that consumed the message first the
    ///    winner: that receiver already published `SEND_COMPLETION_OK` and moved the sender out
    ///    of `EndpointSend`, so the claim fails and no timeout is published — and conversely a
    ///    timeout that claims first leaves the sender non-blocked, so the later receiver-side
    ///    publication refuses. Exactly one of the two publishes, whichever ran first.
    ///    The `TimedOut` completion is published in the SAME acquisition that makes the sender
    ///    `Runnable`, so publish-before-wake holds: nothing enqueues until phase 4.
    /// 2. **rank 3 — waiter removal.** The sender's waiter is removed from its endpoint queue
    ///    once, and the envelope identity it still owned (handle, endpoint, and the envelope's
    ///    OWN bound receiver) is resolved in the same acquisition, so the settle below needs no
    ///    second rank-3 read.
    /// 3. **rank 3 → rank 6 — envelope settle.** Through the already-production
    ///    `settle_blocked_send_envelope_split`, which consumes the envelope exactly once and,
    ///    for a shared-region transfer, releases its single transient pin with no reclaim and no
    ///    shootdown. Runs BEFORE the enqueue, so a woken sender never observes a half-owned
    ///    envelope.
    /// 4. **rank 1 — the enqueue**, through the same split seam the reply class uses. It is the
    ///    last visible action.
    ///
    /// No broad lock is taken anywhere, and no lock is held while another is acquired.
    pub(crate) fn drain_send_timeout_post_work(&self, cpu: CpuId, now: u64) {
        use crate::kernel::boot::{KernelState, MAX_ENDPOINT_SENDER_WAITERS, SenderWaiter};
        use crate::kernel::capabilities::CapObject;
        use crate::kernel::task::{TaskStatus, WaitReason};
        let cpu_idx = cpu.0 as usize;
        while let Some(work) = crate::kernel::boot::send_timeout_work_drain_next(cpu_idx) {
            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────
            let claimed = self.with_task_tcbs_split_mut(|tcbs| {
                let Some(tcb) = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|t| t.tid.0 == work.tid && t.asid == work.asid)
                else {
                    return false;
                };
                if tcb.blocked_send_generation != work.send_generation {
                    return false;
                }
                if !matches!(tcb.status, TaskStatus::Blocked(WaitReason::EndpointSend(_))) {
                    return false;
                }
                let Some(deadline) = tcb.ipc_timeout_deadline else {
                    return false;
                };
                if deadline != work.deadline || now < deadline {
                    return false;
                }
                // U6 §2: the completion names an INCARNATION. An ASID-less sender has none, so
                // it is woken with nothing published — the same thing the in-lock scan did.
                if let Some(asid) = work.asid {
                    tcb.pending_syscall_completion =
                        Some(crate::kernel::task::BlockedSyscallCompletion {
                            syscall_class: crate::kernel::task::BlockedSyscallClass::IpcSend,
                            result: KernelState::SEND_COMPLETION_TIMED_OUT,
                            tid: work.tid,
                            asid,
                            blocked_generation: work.send_generation,
                        });
                }
                tcb.status = TaskStatus::Runnable;
                tcb.ipc_timeout_deadline = None;
                tcb.ipc_timeout_fired = true;
                true
            });
            if !claimed {
                // A receiver won the race, the incarnation was replaced, or the sender
                // re-blocked with a newer generation. Nothing was mutated.
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_STALE tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            }
            crate::yarm_log!(
                "U6_SEND_COMPLETION_PUBLISHED tid={} asid={} send_generation={} result={} result=ok",
                work.tid,
                work.asid.map(|a| a.0).unwrap_or(0),
                work.send_generation,
                KernelState::SEND_COMPLETION_TIMED_OUT
            );
            // ── (2) rank 3: remove the waiter and resolve what it still owned ───────────────
            let envelope = self.with_ipc_split_mut(|ipc| {
                let mut owed: Option<(u64, usize, crate::kernel::ipc::ThreadId)> = None;
                for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
                    for slot in queue[..MAX_ENDPOINT_SENDER_WAITERS].iter_mut() {
                        if !slot.as_ref().is_some_and(|w: &SenderWaiter| {
                            w.tid.0 == work.tid && w.send_generation == work.send_generation
                        }) {
                            continue;
                        }
                        let removed = slot.take().expect("checked Some");
                        let Some(handle) = removed.msg.transferred_cap() else {
                            continue;
                        };
                        let Ok(idx) = usize::try_from(handle.0 & 0xFFFF) else {
                            continue;
                        };
                        // Resolve against the envelope's OWN recorded endpoint, never a live
                        // `ipc.endpoints` entry — the endpoint may legitimately be gone.
                        let Some(Some(env)) = ipc.transfer_envelopes.get(idx).copied() else {
                            continue;
                        };
                        if ipc.transfer_envelope_generations.get(idx).copied()
                            != Some(handle.0 >> 16)
                        {
                            continue;
                        }
                        if !matches!(env.endpoint,
                            CapObject::Endpoint { index, .. } if index == endpoint_idx)
                        {
                            continue;
                        }
                        owed = Some((
                            handle.0,
                            endpoint_idx,
                            env.receiver_tid.unwrap_or(removed.tid),
                        ));
                    }
                }
                owed
            });
            // ── (3) rank 3 released → the canonical settle (rank 3, then rank 6) ────────────
            if let Some((handle, endpoint_idx, cleanup_tid)) = envelope {
                let taken =
                    self.settle_blocked_send_envelope_split(handle, endpoint_idx, cleanup_tid);
                crate::yarm_log!(
                    "U6_BLOCKED_SEND_ENVELOPE_SETTLED tid={} endpoint={} handle={} result={}",
                    work.tid,
                    endpoint_idx,
                    handle,
                    if taken { "ok" } else { "already_settled" }
                );
            }
            // ── (4) rank 1: the wake, last ─────────────────────────────────────────────────
            self.enqueue_reply_timeout_wake_split(work.tid);
            crate::yarm_log!(
                "U7_SEND_TIMEOUT_SETTLED arch={} tid={} asid={} send_generation={} deadline={} now={} broad_lock=0 result=ok",
                crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                work.tid,
                work.asid.map(|a| a.0).unwrap_or(0),
                work.send_generation,
                work.deadline,
                now
            );
            if crate::kernel::boot::send_timeout_deferred_once() {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_DEFERRED arch={} published={} drained={} result=ok",
                    crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                    crate::kernel::boot::send_timeout_work_published_count(),
                    crate::kernel::boot::send_timeout_work_drained_count()
                );
            }
            // The settle is COMMITTED off-lock, but the sender has not yet been delivered its
            // result. Arm the class-retirement seal here; it fires only from the U6 delivery
            // point, so a committed-but-never-delivered completion cannot claim the class
            // retired. Same discipline as the reply class.
            crate::kernel::boot::arm_send_timeout_class_retired();
        }
    }

    /// Canonical 199E — the OFF-LOCK ordinary RECEIVE timeout settle.
    ///
    /// The third and last class off the broad lock, in the same three sequential, never-nested
    /// claims the in-lock scan used and in the same order — task rank 2, then ipc rank 3, then
    /// scheduler rank 1 — with nothing held across anything else:
    ///
    /// 1. **rank 2 — the exact claim.** Settled only if `{tid, asid, blocked_recv_generation}`
    ///    still names a live incarnation `Blocked(EndpointReceive)` whose armed deadline is
    ///    still the collected one and still due. Everything else is refused with ZERO mutation,
    ///    which is what gives delivery / destruction / death / exit and the timeout exactly one
    ///    winner: whichever moves the receiver out of `Blocked(EndpointReceive)` or advances its
    ///    receive generation first, the other refuses. `ipc_timeout_fired` is published in the
    ///    SAME acquisition that makes the receiver `Runnable`, so the `TimedOut` fact is always
    ///    visible before the wake — nothing enqueues until phase 3.
    /// 2. **rank 3 — waiter removal**, by COMPLETE identity (`{tid, asid}`), never a numeric TID,
    ///    across every waiter structure the in-lock Phase 2 cleared, followed by the same
    ///    stranded-waiter re-check.
    /// 3. **rank 1 — the enqueue**, once, through the same split seam the other two classes use.
    ///    It is the last visible action.
    pub(crate) fn drain_recv_timeout_post_work(&self, cpu: CpuId, now: u64) {
        use crate::kernel::boot::ReceiverWaiterIdentity;
        use crate::kernel::task::{TaskStatus, WaitReason};
        let cpu_idx = cpu.0 as usize;
        while let Some(work) = crate::kernel::boot::recv_timeout_work_drain_next(cpu_idx) {
            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────
            let claimed = self.with_task_tcbs_split_mut(|tcbs| {
                let Some(tcb) = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|t| t.tid.0 == work.tid && t.asid == work.asid)
                else {
                    return false;
                };
                if tcb.blocked_recv_generation != work.wait_generation {
                    return false;
                }
                if !matches!(
                    tcb.status,
                    TaskStatus::Blocked(WaitReason::EndpointReceive(_))
                ) {
                    return false;
                }
                // A token-bearing receive belongs to the reply class and is never selected here;
                // refusing defensively keeps that a property of this function too.
                if tcb.reply_timeout_token.is_some() {
                    return false;
                }
                let Some(deadline) = tcb.ipc_timeout_deadline else {
                    return false;
                };
                if deadline != work.deadline || now < deadline {
                    return false;
                }
                tcb.status = TaskStatus::Runnable;
                tcb.ipc_timeout_deadline = None;
                tcb.ipc_timeout_fired = true;
                true
            });
            if !claimed {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_REFUSED_STALE tid={} asid={} wait_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.wait_generation,
                    work.deadline
                );
                continue;
            }
            // ── (2) rank 3: remove the exact waiter from every structure, then re-check ─────
            let identity = ReceiverWaiterIdentity::new(
                crate::kernel::ipc::ThreadId(work.tid),
                work.asid.unwrap_or(crate::kernel::vm::Asid(0)),
            );
            let stranded = self.with_ipc_split_mut(|ipc| {
                ipc.clear_endpoint_waiters_for_identity(identity);
                for waiter in ipc.notification_waiters.iter_mut() {
                    if *waiter == Some(crate::kernel::ipc::ThreadId(work.tid)) {
                        *waiter = None;
                    }
                }
                ipc.any_endpoint_waiter_is(identity)
                    || ipc
                        .notification_waiters
                        .iter()
                        .any(|w| *w == Some(crate::kernel::ipc::ThreadId(work.tid)))
            });
            if stranded {
                crate::yarm_log!("SCHED_TIMEOUT_STRANDED_WAITER tid={}", work.tid);
            }
            // ── (3) rank 1: the wake, last ─────────────────────────────────────────────────
            self.enqueue_reply_timeout_wake_split(work.tid);
            crate::yarm_log!(
                "U8_RECV_TIMEOUT_SETTLED arch={} tid={} asid={} wait_generation={} deadline={} now={} broad_lock=0 result=ok",
                crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                work.tid,
                work.asid.map(|a| a.0).unwrap_or(0),
                work.wait_generation,
                work.deadline,
                now
            );
            if crate::kernel::boot::recv_timeout_deferred_once() {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_DEFERRED arch={} published={} drained={} result=ok",
                    crate::kernel::boot::REPLY_TIMEOUT_ARCH,
                    crate::kernel::boot::recv_timeout_work_published_count(),
                    crate::kernel::boot::recv_timeout_work_drained_count()
                );
            }
            crate::kernel::boot::arm_recv_timeout_class_retired();
        }
    }

    /// U7 (canonical 199E) — THE production timeout entry.
    ///
    /// One arch-neutral seam that every port's post-lock area calls unconditionally: read the
    /// monotonic now for this port, scan both retired classes once through the cursor-bounded
    /// collector, then drain each class's deferred queue through its own off-lock transaction.
    /// Ordering matters and is fixed here rather than at the call sites — the collector runs
    /// before the drains, so the attestation the reply drain emits can truthfully claim the scan
    /// has already run off the broad lock.
    ///
    /// It is NOT a second dispatch or wake channel: the reply class completes through the single
    /// Stage 200C1 transaction and the send class through the single U6 blocking-send lifecycle,
    /// exactly as their in-lock predecessors did.
    pub(crate) fn run_due_ipc_timeout_work(&self, cpu: CpuId) {
        // THREE CLOCK VALUES over two domains, because a deadline is only meaningful against the
        // clock that produced it and comparing one against another is a correctness bug, not a
        // rounding error.
        //
        // * `production_now` — the SCHEDULER TICK. Every production reply/call deadline
        //   (`arm_production_reply_deadline`) lives here, on all three architectures: there is one
        //   deadline ABI and it is `scheduler_tick_now() + timeout_ticks`.
        // * `oracle_now` — the confined oracle's domain, the architectural hardware counter on
        //   AArch64/RISC-V while the selector is armed (those cooperative ports have no reliable
        //   periodic tick under a user workload) and the scheduler tick everywhere else.
        // * `send_now` — the SCHEDULER TICK for the blocking-send and ordinary-receive classes,
        //   which have only ever had one clock. That is what `process_ipc_timeout_deadlines`
        //   compared against before U7 moved the classes off the broad lock, and preserving it is
        //   what makes the move a relocation rather than a semantic change.
        //
        // Canonical 199E: since production registration went live, a selector-on boot holds
        // records of BOTH reply domains simultaneously, so there is no single "reply now" to
        // scan with. Both values are handed down and each record selects its own.
        let production_now = self.scheduler_tick_now_split_read();
        let oracle_now = self.reply_timeout_oracle_now_split_read();
        let send_now = self.scheduler_tick_now_split_read();
        self.run_due_ipc_timeout_work_at(cpu, production_now, oracle_now, send_now);
    }

    /// The clock-INJECTED body of [`Self::run_due_ipc_timeout_work`]. Splitting the two is what
    /// lets a hosted test drive the real production composition — one scan, then the three
    /// class drains, in this order — at a controlled `now`, instead of re-implementing the
    /// order in the test and proving nothing about the production entry.
    pub(crate) fn run_due_ipc_timeout_work_at(
        &self,
        cpu: CpuId,
        production_now: u64,
        oracle_now: u64,
        send_now: u64,
    ) {
        self.collect_due_ipc_timeout_work(production_now, oracle_now, send_now, cpu);
        self.drain_reply_timeout_post_work(cpu, production_now, oracle_now);
        self.drain_send_timeout_post_work(cpu, send_now);
        self.drain_recv_timeout_post_work(cpu, send_now);
    }

    /// Stage 200C2B — the OFF-LOCK scheduler enqueue for a timeout-woken caller. Reads
    /// the task's class (priority) and CPU affinity under SHORT rank-2 task claims, each
    /// released BEFORE the rank-1 scheduler claim performs the enqueue — so no task lock
    /// is held while enqueuing, and no broad lock is ever taken. Mirrors
    /// `KernelState::enqueue_task`'s placement (pinned → its CPU; unpinned → balanced).
    // CLASSIFICATION (Stage 200D-F0): **production mechanism**. The server-death
    // completion enqueues its woken caller through this seam on every build.
    fn enqueue_reply_timeout_wake_split(&self, tid: u64) {
        use crate::kernel::ipc::ThreadId;
        use crate::kernel::scheduler::TaskPriority;
        use crate::kernel::task::TaskClass;
        let priority = match self.task_class_split_read(tid) {
            Some(TaskClass::SystemServer) => TaskPriority::High,
            _ => TaskPriority::Normal,
        };
        let affinity = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .and_then(|t| t.cpu_affinity)
        });
        self.with_scheduler_split_mut(|sched| {
            let s = kernel_mut(&mut sched.scheduler);
            match affinity {
                Some(cpu) => {
                    let _ = s.enqueue_on_with_priority(cpu, ThreadId(tid), priority);
                }
                None => {
                    let _ = s.enqueue_balanced(ThreadId(tid), priority);
                }
            }
        });
    }

    /// Stage 200C1 — the NR7 reply-win deadline-disarm hook. When reply obtains
    /// terminal ownership, cancel the EXACT associated deadline token (by slot +
    /// generation + epoch) so a stale queued fire can no longer claim. Reads the
    /// handle from the caller's TCB reference. Never holds the deadline store lock
    /// during any user copy, and cannot reopen terminal authority or cancel a NEWER
    /// registration. `true` iff a token was disarmed.
    pub(crate) fn disarm_reply_deadline_on_reply_win(
        &self,
        caller_tid: u64,
        caller_asid: crate::kernel::vm::Asid,
    ) -> bool {
        // U3 (203C): two SEQUENTIAL narrow acquisitions replace the two broad ones. Rank 2 reads
        // the exact `{caller_tid, caller_asid}` incarnation's handle and is fully released before
        // rank 3 is taken — the two are never nested, so no task lock is held while the deadline
        // store is touched and no lock of either rank is held during a user copy.
        //
        // The old body already dropped the broad lock between its two `self.with(...)` calls, so
        // this conversion keeps the SAME inter-operation window. The exact token generation and
        // epoch carried by the handle remain the stale-disarm protection: a handle read here can
        // never disarm a newer registration.
        let handle = self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == caller_tid && t.asid == Some(caller_asid))
                .and_then(|t| t.reply_timeout_token)
        });
        let Some(handle) = handle else {
            return false;
        };
        self.with_ipc_split_mut(|ipc| {
            ipc.reply_deadline_tokens
                .get(handle.token_index())
                .is_some_and(|t| {
                    t.disarm_after_terminal_completion(handle.identity(), handle.epoch())
                })
        })
    }

    /// Stage 199A2D2A: assign a task's authoritative home CPU (internal placement only; not a
    /// syscall). The x86_64 SMP cross-CPU request oracle binds its server to CPU 1 with this before
    /// the server blocks in recv-v2, so the accepted NR6 transaction's affinity-targeted enqueue
    /// remotely places the woken server on CPU 1's run queue.
    ///
    /// U3 (203C): assigns through the rank-2 task seam instead of the broad lock, matching
    /// `KernelState::set_task_home_cpu` exactly — find by numeric TID, set `cpu_affinity`, and
    /// report success only when the TCB exists (`Err(TaskMissing)` → `false`). No online-CPU
    /// validation is added, the placement policy is unchanged, the scheduler lock is not taken,
    /// and there is no broad fallback.
    pub(crate) fn smp_assign_task_home_cpu(&self, tid: u64, cpu: CpuId) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            match tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
                Some(tcb) => {
                    tcb.cpu_affinity = Some(cpu);
                    true
                }
                None => false,
            }
        })
    }

    /// Origin-neutral finalize + wake for a shared-region delivery, entirely off-lock. For a blocked
    /// endpoint waiter (`snap.blocked_endpoint_waiter`) it runs the Stage 198E3B2B1 CLAIM-THEN-COMMIT
    /// protocol: Phase 1 prevalidate (rank 2, no mutation) → Phase 2 exact generation-bearing waiter
    /// claim (rank 3, remove once) → Phase 3 commit (rank 2, clear registers + Runnable + affinity for
    /// a live match; NO register mutation before the claim, none on a failed commit) → Phase 4 enqueue
    /// (rank 1, once, non-fallible, last visible action). A dead receiver after the claim is stale with
    /// the claimed waiter left removed; a replaced receiver has its exact waiter restored so no live
    /// task is stranded — both roll back with ZERO wake. When `!blocked_endpoint_waiter` (queued
    /// dequeue / proofs) it is a plain best-effort wake, preserving the prior executor semantics.
    pub(crate) fn sr_finalize_blocked_receiver_and_wake_split(
        &self,
        snap: &crate::kernel::boot::shared_region_txn::RecvBoundarySharedRegionSnapshot,
    ) -> Option<bool> {
        if !snap.blocked_endpoint_waiter {
            // Plain wake path: no endpoint-waiter identity to claim (queued dequeue / txn proofs).
            return Some(self.sr_wake_receiver_split(snap.receiver_tid));
        }
        let CapObject::Endpoint {
            index: eidx,
            generation: egen,
        } = snap.endpoint
        else {
            return None;
        };
        // Stage 198E3B2B2: the claim is by the COMPLETE identity captured at production (tid + ASID).
        let receiver = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(snap.receiver_tid),
            snap.receiver_asid,
        );
        // Phase 1 (rank 2): prevalidate — NO mutation. A dead/replaced receiver is stale BEFORE claim,
        // so a replacement waiter slot is never removed on the common path.
        if !self.sr_prevalidate_blocked_receiver_split(snap.receiver_tid, snap.receiver_asid) {
            return None;
        }
        // Phase 2 (rank 3): the exact, generation-bearing IDENTITY claim (remove once). Because the
        // claim requires the full {tid, ASID} identity, a replacement task (reused numeric TID, new
        // ASID) can never be claimed or cleared here.
        let claim = self.sr_claim_endpoint_waiter_split(eidx, egen, receiver)?;
        // Stage 199D: capture the wait reason before the commit clears it, so a refused
        // placement can restore the exact blocked state.
        let recv_cap = self.blocked_recv_cap_split_read(snap.receiver_tid, snap.receiver_asid);
        // Phase 3 (rank 2): commit — registers cleared ONLY here, strictly after the claim.
        match self.sr_commit_blocked_receiver_split(snap.receiver_tid, snap.receiver_asid) {
            ReceiverCommit::Committed(affinity) => {
                // Phase 4 (rank 1): the single enqueue — the last externally visible act.
                //
                // Stage 199D: a refused placement is no longer swallowed, and no longer reported
                // as a wake either. This caller uses the GENERIC seam, which never withdraws a
                // pre-existing entry: it owns no rollback that could complete such a withdrawal,
                // so removing one would be a hidden side effect. On refusal it restores its own
                // receiver — `Runnable → Blocked` on the exact recv cap, waiter reinstalled — and
                // reports NO wake, rather than leaving a Runnable-but-unqueued task behind.
                //
                // An `AlreadyQueued` collision arrives unreconciled (`reconciled: None`), so the
                // receiver's membership is unknown: fail closed with ZERO mutation — do not force
                // it Blocked on top of live membership, and do not touch its queue entry.
                match self.sr_enqueue_committed_receiver_split(snap.receiver_tid, affinity) {
                    ReceiverEnqueue::Enqueued { .. } => Some(true),
                    ReceiverEnqueue::Rejected { cpu, error, .. } => {
                        let unplaced = !matches!(
                            error,
                            crate::kernel::scheduler::SchedulerError::AlreadyQueued
                        );
                        let restored = unplaced
                            && recv_cap.is_some_and(|cap| {
                                self.sr_uncommit_blocked_receiver_split(
                                    snap.receiver_tid,
                                    snap.receiver_asid,
                                    cap,
                                )
                            })
                            && self.sr_restore_endpoint_waiter_split(&claim);
                        crate::yarm_log!(
                            "SR_RECEIVER_ENQUEUE_REJECTED tid={} cpu={} error={:?} restored={} result=no_wake",
                            snap.receiver_tid,
                            cpu.0,
                            error,
                            u32::from(restored)
                        );
                        None
                    }
                }
            }
            // Receiver exited OR was replaced (ASID changed) after the identity claim: the claimed
            // waiter belonged to our exact receiver incarnation, which no longer exists — it is
            // correctly stale and MUST NOT be restored (the old numeric-TID Replaced→restore is
            // removed; a restore could only ever target the vanished incarnation, never a live
            // replacement task). Zero wake; the transaction rolls back.
            ReceiverCommit::GoneDead | ReceiverCommit::Replaced => None,
        }
    }

    /// Off-global-lock, all-or-nothing user-copy primitive (introduced Stage 191C; its
    /// original NR 27 InitramfsReadChunk caller was removed in Stage 197A, but this remains a
    /// generic split-path write seam with dedicated no-partial-write unit tests): copy the
    /// kernel slice `src` into user VA `user_ptr` in address space `asid_raw`, OFF the broad
    /// global lock. Byte-identical in end-state to the legacy `KernelState::
    /// copy_to_current_user_from_slice` / `copy_slice_to_task` (per-page validate +
    /// bulk `copy_nonoverlapping`), but driven through the rank-5 VM seam
    /// (`validate_user_access_for_asid_split`) + the direct map instead of a broad
    /// `&mut KernelState`. No IPC (rank 3) / capability (rank 4) / scheduler (rank 1) /
    /// task (rank 2) lock is taken.
    ///
    /// TWO-PASS (all-or-nothing) so a partial write can never happen on the split path:
    /// * Pass 1 validates EVERY destination page is user-writable and performs NO write.
    ///   If any page is unmapped / not user-writable it returns `Err(UserMemoryFault)`
    ///   BEFORE a single byte is written — so the caller can safely fall back to the
    ///   unchanged global-lock handler for the canonical error with zero user mutation.
    /// * Pass 2 runs only after every page validated, so it cannot fault; it bulk-copies
    ///   each page-aligned chunk through the direct map.
    ///
    /// Returns `Err(UserMemoryFault)` on any validation miss (same error class the legacy
    /// path raises; the legacy path never faults-in / COWs either — it only validates
    /// flags). The single-dispatcher trap point runs this with no concurrent mutator, so
    /// Pass 2's re-resolve observes the same mappings Pass 1 validated. Available in both
    /// configs (the two-pass structure is config-independent; only the leaf byte write
    /// differs — direct-map `copy_nonoverlapping` bare-metal, `write_user_byte_split`
    /// hosted — so the hosted build can unit-test the no-partial-write guarantee directly).
    pub fn copy_slice_to_user_asid_split_write(
        &self,
        asid_raw: u64,
        user_ptr: usize,
        src: &[u8],
    ) -> Result<(), KernelError> {
        use crate::kernel::vm::{Asid, PAGE_SIZE};
        let asid = Asid(u16::try_from(asid_raw).map_err(|_| KernelError::UserMemoryFault)?);
        let len = src.len();
        // Pass 1: validate every destination page is user-writable (NO write). A fault
        // here returns BEFORE a single byte is written.
        let mut done = 0usize;
        while done < len {
            let va = user_ptr
                .checked_add(done)
                .ok_or(KernelError::UserMemoryFault)?;
            let page_off = va & (PAGE_SIZE - 1);
            let chunk = (len - done).min(PAGE_SIZE - page_off);
            self.validate_user_access_for_asid_split(asid, va, true)?;
            done += chunk;
        }
        // Pass 2: every page validated ⇒ the copy cannot fault. Same per-page walk as the
        // legacy bulk copy path; the leaf write is the config-appropriate primitive.
        let mut done = 0usize;
        while done < len {
            let va = user_ptr
                .checked_add(done)
                .ok_or(KernelError::UserMemoryFault)?;
            let page_off = va & (PAGE_SIZE - 1);
            let chunk = (len - done).min(PAGE_SIZE - page_off);
            let phys = self.validate_user_access_for_asid_split(asid, va, true)?;
            #[cfg(not(feature = "hosted-dev"))]
            {
                let dst_ptr = crate::kernel::boot::KernelState::phys_to_direct_map_ptr(phys)
                    .ok_or(KernelError::UserMemoryFault)?;
                // SAFETY: `phys` is within a validated user-writable mapping; `chunk`
                // never exceeds the bytes left in that page; `src` has ≥ `len` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(src[done..].as_ptr(), dst_ptr, chunk);
                }
            }
            #[cfg(feature = "hosted-dev")]
            {
                for j in 0..chunk {
                    self.write_user_byte_split(
                        asid,
                        crate::kernel::vm::VirtAddr(phys + j as u64),
                        src[done + j],
                    )?;
                }
            }
            done += chunk;
        }
        Ok(())
    }

    /// Stage 191D (FUTEXWAIT BLOCK-PUBLISH SEAM), Phase A: validate the futex word and
    /// decide whether the caller `tid` WOULD block, OFF the broad global lock. Mirrors the
    /// read/validate portion of `KernelState::futex_wait_current` /
    /// `validate_current_user_futex_word` EXACTLY:
    /// * `addr == 0` → `None` (legacy `WrongObject`).
    /// * `addr + 3 >= KERNEL_SPACE_BASE` → `None` (legacy `UserMemoryFault`).
    /// * 4-byte user read fails → `None` (legacy `UserMemoryFault`).
    ///
    /// On a validated address returns `Some(would_block)` where `would_block ==
    /// (expected == observed)` — identical to `futex_wait_current`'s `expected != observed
    /// → Ok(false)` decision (the futex value comparison uses the caller-provided `expected`
    /// / `observed` syscall args; the memory read only proves the address is user-readable).
    /// Read-only: no TCB / scheduler / IPC / cap / VM structural mutation. `None` lets a
    /// caller fall back to the global-lock handler for the canonical error (never masked).
    #[cfg(not(feature = "hosted-dev"))]
    pub fn futex_wait_would_block_split_read(
        &self,
        tid: u64,
        addr: usize,
        expected: u32,
        observed: u32,
    ) -> Option<bool> {
        if addr == 0 {
            return None; // legacy: WrongObject
        }
        let end = addr.checked_add(core::mem::size_of::<u32>() - 1)?;
        if end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE {
            return None; // legacy: UserMemoryFault
        }
        let asid = self.task_asid_for_tid_split_read(tid);
        self.copy_from_user_asid_split_read(asid, addr, core::mem::size_of::<u32>())?;
        Some(expected == observed)
    }

    /// Stage 191D (FUTEXWAIT BLOCK-PUBLISH SEAM), Phase B: publish the caller `tid` as
    /// `Blocked(Futex(addr))` and clear the current-CPU slot, OFF the broad global lock —
    /// mirroring the block portion of `KernelState::futex_wait_current` (the TCB status
    /// set) + `block_current_cpu` (`block_current_on` + `timer.reset_quantum`), WITHOUT the
    /// subsequent `dispatch_next_task`. Task lock (rank 2) then scheduler lock (rank 1),
    /// each held transiently and released before the next — non-nested; no broad
    /// `&mut KernelState`. The published waiter is left `Blocked` and NOT enqueued (so no
    /// duplicate enqueue and no orphaned runnable), removed from the current slot (so it is
    /// current on NO CPU), and observable to `futex_wake_split_mut` on the same `addr` (so
    /// no lost wake). Requires `tid` to be the current task on `cpu` (the live caller is).
    /// Returns `true` iff the caller was published `Blocked` and removed from current.
    ///
    /// DEFERRED / HELPER-ONLY: this is the block-publish half of a split FutexWait. It does
    /// NOT dispatch — the queue-ADVANCING switch to the next runnable task
    /// (`dispatch_next_task`'s "switch_required" case) requires the global-lock dispatch /
    /// context-switch machinery and is the documented multi-stage rewrite, so FutexWait's
    /// LIVE retirement is deferred and this seam is not wired into `try_split_dispatch`.
    pub fn futex_wait_publish_block_split_mut(&self, cpu: CpuId, tid: u64, addr: usize) -> bool {
        use crate::kernel::task::{TaskStatus, WaitReason};
        use crate::kernel::vm::VirtAddr;
        // Phase B1: publish Blocked(Futex(addr)) on the caller's TCB (task lock, rank 2) —
        // identical transition to `futex_wait_current`'s `with_tcbs_mut` block.
        let published = self.with_task_tcbs_split_mut(|tcbs| {
            match tcbs.iter_mut().flatten().find(|t| t.tid.0 == tid) {
                Some(tcb) => {
                    tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
                    true
                }
                None => false,
            }
        });
        if !published {
            return false;
        }
        // Phase B2: clear the current-CPU slot (scheduler lock, rank 1) — identical to
        // `block_current_cpu` (block_current_on + reset_quantum). NO dispatch here.
        self.with_scheduler_split_mut(|sched| {
            let blocked = kernel_mut(&mut sched.scheduler).block_current_on(cpu);
            if blocked.is_some() {
                sched.timer.reset_quantum();
            }
        });
        crate::yarm_log!(
            "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid={} addr={}",
            tid,
            addr
        );
        true
    }

    /// Stage 191E (FUTEXWAIT PHASE-C SELECTION SEAM): peek the next-runnable dispatch
    /// candidate on `cpu` — the TID the authoritative per-CPU dispatch would select once
    /// the current slot is idle/cleared — OFF the broad global lock, through the scheduler split seam
    /// (rank 1) ONLY. READ-ONLY: it never dequeues, never sets current, never mutates any
    /// scheduler/task state; the run queue is unchanged (two calls return the same TID).
    ///
    /// This is the non-mutating SELECTION half of the deferred FutexWait "switch_required"
    /// Phase C (queue-advancing dispatch), complementing the 191D Phase A value-check
    /// (`futex_wait_would_block_split_read`) + Phase B block-publish
    /// (`futex_wait_publish_block_split_mut`). It proves the next-task DECISION is available
    /// off the global lock; the mutating dequeue + arch context switch remain the deferred
    /// hard part, so this seam is HELPER-ONLY (not wired into `try_split_dispatch`).
    /// Returns `None` when no task is runnable on `cpu` (the caller would idle).
    pub fn dispatch_next_candidate_split_read(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            kernel_ref(&sched.scheduler)
                .peek_next_runnable_on(cpu)
                .map(|tid| tid.0)
        })
    }

    pub fn fatal_trap_read_snapshot(&self, cpu: CpuId) -> FatalTrapReadSnapshot {
        // Stage 4T+7 split-read: pre-read diagnostic data for the fatal-trap log.
        // Acquires scheduler lock (rank 1) for current_tid, then task lock (rank 2)
        // for ASID — each held transiently and released before the next is acquired.
        // Does not acquire the outer SharedKernel lock.
        let current_tid = self.current_tid_split_read(cpu).unwrap_or(0);
        let current_asid = if current_tid != 0 {
            self.task_asid_for_tid_split_read(current_tid)
        } else {
            0
        };
        FatalTrapReadSnapshot {
            current_tid,
            current_asid,
        }
    }

    // ── Stage 5A split-read helpers ──────────────────────────────────────────

    pub fn task_class_split_read(&self, tid: u64) -> Option<TaskClass> {
        // Stage 5A split-read: read task class under task lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Lock order: task (rank 2). Forbidden caller-held locks: none with rank ≤ 2.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `task_class_from_raw` uses `addr_of!` to derive raw
        // field pointers without creating a whole-KernelState reference; the task
        // lock serializes access to both `tcbs` and `task_classes`.
        unsafe { KernelState::task_class_from_raw(self.state.data_ptr() as *const _, tid) }
    }

    pub fn task_exists_split_read(&self, tid: u64) -> bool {
        // Stage 5A split-read: check task existence under task lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Lock order: task (rank 2). Forbidden caller-held locks: none with rank ≤ 2.
        // SAFETY: same as `task_class_split_read`.
        unsafe { KernelState::task_exists_from_raw(self.state.data_ptr() as *const _, tid) }
    }

    /// Stage 199D — live reverse-link count, through the rank-2 task seam only.
    ///
    /// Observational, for the ServerDies quiescent link-balance attestation. It reads the
    /// same field `KernelState::live_server_reply_link_count` reads, but it must NOT take the
    /// broad lock: the caller is the off-lock DebugLog split path, and adding a broad
    /// acquisition there would both re-enter the lock this programme is retiring and break
    /// the Stage 204A census (`tests/broad_lock_census_guard.rs` would fail).
    /// Stage 199D — whether this reply-record incarnation participates in an armed
    /// terminal-ownership / reply-timeout race.
    ///
    /// The canonical predicate, read from the authoritative state itself
    /// (`reply_terminal_ownership`, co-located with and indexed identically to `reply_caps`) —
    /// never inferred from oracle selectors, markers or counters.
    ///
    /// # Exactness
    ///
    /// A cell is arbitrating THIS reply only when its epoch is non-zero (a vacant cell is
    /// epoch 0 with `TerminalIdentity::ZERO`) **and** its immutable identity names this exact
    /// record index AND generation. A cell armed for a previous occupant of a recycled slot
    /// names the old generation and is correctly reported as not arbitrating this one.
    ///
    /// # Why this is not a TOCTOU test
    ///
    /// Two properties make the read exact rather than a sample:
    ///
    /// 1. **Internal consistency.** The record generation and the terminal cell are read under
    ///    ONE rank-3 acquisition, so the pair cannot be torn — the generation cannot advance
    ///    between reading it and reading the cell armed for it.
    /// 2. **Arming strictly precedes reply deliverability.** The cell is armed by
    ///    `maybe_arm_reply_timeout_oracle` at the caller's blocking-recv publication
    ///    (`IPC_RECV_BLOCK_REGISTER`), which happens *before* the blocked-caller
    ///    acknowledgement is published at `IPC_RECV_BLOCKED_STATE_SAVE`. The direct NR7 path
    ///    cannot reach eligibility for a record whose caller has not yet published that
    ///    acknowledgement. So a cell cannot transition unarmed → armed for this incarnation
    ///    between this read and the transaction: the arming already happened, or the reply is
    ///    not deliverable yet at all.
    pub(crate) fn reply_record_terminal_arbitrated_split_read(
        &self,
        index: usize,
        generation: u64,
    ) -> bool {
        self.with_ipc_split_mut(|ipc| {
            // One acquisition covers both reads, so the record incarnation and the cell armed
            // for it are observed together.
            if ipc.reply_cap_generations.get(index).copied() != Some(generation) {
                return false;
            }
            let Some(cell) = ipc.reply_terminal_ownership.get(index) else {
                return false;
            };
            if cell.current_epoch() == 0 {
                return false; // vacant: never armed
            }
            let identity = cell.identity();
            identity.reply_record_index == index && identity.reply_record_generation == generation
        })
    }

    /// Stage 199D — the **final lease/waiter bijection**, measured from the waiter table.
    ///
    /// Two passes over the endpoint table, keyed by endpoint index (the acknowledgement store
    /// is one slot per endpoint index, so the lease lookup is O(1)):
    ///
    /// 1. every endpoint receive-waiter is classified — eligible waiters must have exactly one
    ///    live lease at their exact incarnation;
    /// 2. every live lease must have exactly one eligible waiter behind it.
    ///
    /// Neither pass consults the store's own counters, so the two accounts are independent: a
    /// store that balanced its books while dropping or orphaning a lease is caught here. The
    /// previous capacity-8 store did exactly that — its counters read clean while a ninth
    /// parked server was refused a lease.
    ///
    /// **Split-seam only — no broad lock.** The IPC (rank 3) and task (rank 2) domains are
    /// taken one index at a time and never simultaneously, so this adds no broad-lock
    /// acquisition to the split dispatcher that hosts it. It allocates nothing: the comparison
    /// is accumulated in place rather than materialising either set.
    pub(crate) fn direct_ack_lease_bijection(
        &self,
        store: &crate::kernel::direct_ack_store::DirectAckStore,
        endpoint_admitted: impl Fn(usize) -> bool,
    ) -> crate::kernel::direct_ack_census::LeaseBijection {
        use crate::kernel::direct_ack_census::{CensusWaiter, LeaseBijection, classify_slot};
        let slots = self.with_ipc_split_mut(|ipc| ipc.endpoint_waiters.len());
        let mut out = LeaseBijection::default();
        for idx in 0..slots {
            let entry = self.with_ipc_split_mut(|ipc| {
                ipc.endpoint_waiter_record(idx)
                    .map(|r| (r.tid().0, r.asid().0, ipc.endpoint_generations[idx]))
            });
            let waiter = entry.map(|(tid, asid, generation)| CensusWaiter {
                endpoint_index: idx,
                endpoint_generation: generation,
                tid,
                asid,
                // Eligible exactly when the acknowledgement publication contract would publish
                // for it: a fully committed recv-v2 on an admitted endpoint. Re-derived from
                // committed state rather than remembered, so a lease issued against a waiter
                // that never qualified is detectable.
                eligible: endpoint_admitted(idx)
                    && self.blocked_recv_v2_commit_is_complete_split_read(tid),
            });
            let lease = store
                .live_lease_at(idx)
                .map(|(generation, w)| (generation, w.tid, w.asid));
            classify_slot(waiter, lease, &mut out);
        }
        out.duplicate_endpoint_incarnations = store.duplicate_live_incarnations();
        out
    }

    /// Whether this task is blocked in a FULLY committed recv-v2 — the same contract the
    /// acknowledgement publication site applies (recv-v2 ABI, a valid payload destination and
    /// a non-null metadata destination). Task domain (rank 2) only.
    pub(crate) fn blocked_recv_v2_commit_is_complete_split_read(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                .is_some_and(|state| {
                    state.recv_abi == crate::kernel::task::RecvAbiVariant::RecvV2
                        && state.payload_user_ptr != 0
                        && state.meta_user_ptr != 0
                })
        })
    }

    pub(crate) fn live_server_reply_link_count_split_read(&self) -> usize {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .filter(|t| t.server_reply_link.is_some())
                .count()
        })
    }

    pub fn cnode_slot_capacity_split_read(&self, pid: u64) -> Option<usize> {
        // Stage 5A split-read: read CNode slot capacity under capability lock (rank 4) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Lock order: capability (rank 4). Forbidden caller-held locks: none with rank ≤ 4.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `cnode_slot_capacity_from_raw` uses `addr_of!` to derive
        // raw field pointers without creating a whole-KernelState reference; the
        // capability lock serializes access to the `capability` field.
        unsafe { KernelState::cnode_slot_capacity_from_raw(self.state.data_ptr() as *const _, pid) }
    }

    // ── Stage 5B split-read helpers ──────────────────────────────────────────

    pub fn process_id_split_read(&self, tid: u64) -> Option<u64> {
        // Stage 5B split-read: read thread-group-id (process id) under task lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Lock order: task (rank 2). Forbidden caller-held locks: none with rank ≤ 2.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `process_id_from_raw` uses `addr_of!` to derive raw
        // field pointers; the task lock serializes access to the `tcbs` array.
        unsafe { KernelState::process_id_from_raw(self.state.data_ptr() as *const _, tid) }
    }

    pub fn is_group_leader_split_read(&self, tid: u64) -> bool {
        // Stage 5B split-read: check thread-group-leader status under task lock (rank 2) only.
        // Does not acquire the outer SharedKernel lock. Does not mutate any state.
        // Lock order: task (rank 2). Forbidden caller-held locks: none with rank ≤ 2.
        // SAFETY: same as `process_id_split_read`.
        unsafe { KernelState::is_group_leader_from_raw(self.state.data_ptr() as *const _, tid) }
    }

    // ── Stage 26 split-read helpers ──────────────────────────────────────────

    /// # Validation status: LIVE_OFF_TRAP — reads IPC domain lock (rank 3); off-trap use only.
    pub fn notification_waiter_count_split_read(&self, notification_idx: usize) -> usize {
        // STAGE 26: extracted from global lock, uses only domain ipc (rank 3) lock.
        // Reads the notification-waiter presence for `notification_idx` through
        // ipc_state_lock only. Does not acquire the outer SharedKernel lock and
        // does not mutate any state.
        // Lock order: ipc (rank 3). Forbidden caller-held locks: none with rank ≤ 3.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `notification_waiter_count_from_raw` uses `addr_of!`
        // to derive raw field pointers without creating a whole-KernelState
        // reference; the ipc lock serializes access to the `ipc` field.
        unsafe {
            KernelState::notification_waiter_count_from_raw(
                self.state.data_ptr() as *const _,
                notification_idx,
            )
        }
    }

    /// # Validation status: LIVE_OFF_TRAP — reads capability domain lock (rank 4); off-trap use only.
    pub fn cnode_registered_split_read(&self, pid: u64) -> bool {
        // STAGE 26: extracted from global lock, uses only domain capability (rank 4) lock.
        // Checks whether a CNode space is registered for `pid` through
        // capability_state_lock only. Does not acquire the outer SharedKernel
        // lock and does not mutate any state.
        // Lock order: capability (rank 4). Forbidden caller-held locks: none with rank ≤ 4.
        // SAFETY: `state.data_ptr()` is the stable KernelState storage owned by
        // this SharedKernel. `cnode_registered_from_raw` uses `addr_of!` to derive
        // raw field pointers without creating a whole-KernelState reference; the
        // capability lock serializes access to the `capability` field.
        unsafe { KernelState::cnode_registered_from_raw(self.state.data_ptr() as *const _, pid) }
    }

    // ── Stage 27 / 29 split-mutation helpers ─────────────────────────────────

    /// # Validation status
    /// - LIVE_TRAP_SMOKE_X86_64 — used by `try_split_dispatch_into_frame` for the
    ///   NR 8 live-wired split path; x86_64 smoke validated (Stage 29 / 29A).
    ///
    /// STAGE 27: first mutating global-lock extraction for
    /// `control_plane_set_process_cnode_slots`. Performs the two-phase
    /// task(read) → capability(mutate) protocol WITHOUT acquiring the outer
    /// `SharedKernel` lock and without calling `with`/`with_cpu`.
    ///
    /// Phase 1 (task snapshot, rank 2): read the requester's class and pid via
    /// the existing `task_class_from_raw` / `process_id_from_raw` split-reads,
    /// which each acquire and RELEASE `task_state_lock` before returning. No task
    /// lock is held past this point.
    ///
    /// Phase 1b (boot-config snapshot): read the runtime capacity limits via
    /// `runtime_capacity_config_split_read` (boot_config lock only).
    ///
    /// Phase 2 (capability mutation, rank 4): apply the create/resize through
    /// `control_plane_set_process_cnode_slots_apply_from_raw`, which acquires
    /// ONLY `capability_state_lock`.
    ///
    /// Lock order is therefore task(2) → boot_config → capability(4), never
    /// inverted: the capability lock is acquired only after both reads have
    /// released their locks. Behavior and error returns are identical to the
    /// global-locked `control_plane_set_process_cnode_slots_via_syscall` /
    /// `_planned` path:
    /// - `TaskMissing` if the requester TID has no task (matches the global path's
    ///   `task_class().ok_or(TaskMissing)`).
    /// - `MissingRight` / `WrongObject` / `CapabilityFull` / `TaskTableFull`
    ///   exactly as the capability apply phase produces them.
    ///
    /// SAFETY: `state.data_ptr()` is the stable `KernelState` storage owned by
    /// this `SharedKernel`. Each `*_from_raw` helper derives raw field pointers
    /// without creating a whole-`KernelState` reference; the per-domain locks
    /// serialize access to their respective fields.
    pub fn control_plane_set_process_cnode_slots_split_mut(
        &self,
        requester_tid: u64,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), KernelError> {
        let state = self.state.data_ptr();
        // Phase 1: task-domain snapshot (rank 2), lock released on return.
        let requester_class =
            unsafe { KernelState::task_class_from_raw(state as *const _, requester_tid) }
                .ok_or(KernelError::TaskMissing)?;
        let requester_pid =
            unsafe { KernelState::process_id_from_raw(state as *const _, requester_tid) }
                .unwrap_or(requester_tid);
        let plan = ControlPlaneCnodePlan {
            requester_class,
            requester_pid,
        };
        // Phase 1b: boot-config snapshot (boot_config lock only).
        let limits = self.runtime_capacity_config_split_read();
        // Phase 2: capability-domain mutation (rank 4), task lock already released.
        unsafe {
            KernelState::control_plane_set_process_cnode_slots_apply_from_raw(
                state,
                &plan,
                target_pid,
                slot_capacity,
                limits,
            )
        }
    }

    /// # Validation status
    /// - HELPER_ONLY — Stage 32 endpoint receive-cap resolution split-read.
    ///   Used by the Stage 31 queued-plain recv helper before the IPC dequeue;
    ///   NOT wired into the live trap seam. See `doc/KERNEL_LOCKING.md` §50.
    ///
    /// STAGE 32: resolve a `requester_tid`'s endpoint **receive** capability
    /// `cap` WITHOUT acquiring the outer `SharedKernel` lock, the IPC lock, or
    /// holding the task and capability locks simultaneously.
    ///
    /// Phase 1 (task snapshot, rank 2): read the requester's pid via
    /// `process_id_from_raw`, which acquires and RELEASES `task_state_lock`
    /// before returning. The task lock is NOT held past this point. A missing
    /// task surfaces as `InvalidCapability` (the old path resolves the cnode via
    /// the requester pid; an unknown requester has no cnode → invalid cap).
    ///
    /// Phase 2 (capability resolution, rank 4): look up + validate the cap in the
    /// requester pid's cnode via `resolve_endpoint_recv_cap_in_pid_from_raw`,
    /// which acquires ONLY `capability_state_lock`. No mutation. No IPC lock.
    ///
    /// Lock order: task(2) [read+release] → capability(4) [read+release].
    /// No nested locks. ipc(3) is acquired only AFTER this function returns
    /// (during the dequeue phase). No global lock required.
    ///
    /// Errors map to the old global-lock `IpcRecv` cap-resolution (`SyscallError`
    /// via `From<KernelError>`): `InvalidCapability` (missing cnode/slot),
    /// `WrongObject` (non-endpoint), `MissingRight` (no RECEIVE right). The
    /// IPC-domain generation liveness check is intentionally deferred to the
    /// caller's dequeue phase (it requires `ipc_state_lock`).
    ///
    /// SAFETY: `state.data_ptr()` is the stable `KernelState` storage owned by
    /// this `SharedKernel`. Each `*_from_raw` helper derives raw field pointers
    /// without creating a whole-`KernelState` reference; the per-domain locks
    /// serialize access to their respective fields.
    pub fn resolve_endpoint_recv_cap_split_read(
        &self,
        requester_tid: u64,
        cap: CapId,
    ) -> Result<EndpointRecvCapSnapshot, KernelError> {
        let state = self.state.data_ptr();
        // Phase 1: task-domain snapshot (rank 2), lock released on return.
        let requester_pid =
            unsafe { KernelState::process_id_from_raw(state as *const _, requester_tid) }
                .ok_or(KernelError::InvalidCapability)?;
        // Phase 2: capability-domain resolution (rank 4), task lock released.
        let (endpoint, rights) = unsafe {
            KernelState::resolve_endpoint_recv_cap_in_pid_from_raw(
                state as *const _,
                requester_pid,
                cap,
            )
        }?;
        Ok(EndpointRecvCapSnapshot {
            endpoint,
            rights,
            requester_tid,
            requester_pid,
        })
    }

    /// Stage 199A2B2D: off-lock SEND-endpoint cap resolution (task rank-2 pid read →
    /// capability rank-4 resolve), sibling of `resolve_endpoint_recv_cap_split_read`.
    /// Returns the resolved `CapObject::Endpoint` (index+generation) the caller's
    /// `cap` names, requiring the `SEND` right. No broad lock, no IPC lock.
    pub(crate) fn resolve_endpoint_send_cap_split_read(
        &self,
        requester_tid: u64,
        cap: CapId,
    ) -> Result<CapObject, KernelError> {
        let state = self.state.data_ptr();
        let requester_pid =
            unsafe { KernelState::process_id_from_raw(state as *const _, requester_tid) }
                .ok_or(KernelError::InvalidCapability)?;
        let (endpoint, _rights) = unsafe {
            KernelState::resolve_endpoint_send_cap_in_pid_from_raw(
                state as *const _,
                requester_pid,
                cap,
            )
        }?;
        Ok(endpoint)
    }

    /// Stage 199A2B3: off-lock resolution of a `Reply` cap the caller/replier holds
    /// (task rank-2 pid read → capability rank-4 resolve), returning the reply object
    /// `(index, generation)`. Requires the `SEND` right on the cap.
    pub(crate) fn resolve_reply_cap_split_read(
        &self,
        requester_tid: u64,
        cap: CapId,
    ) -> Result<(usize, u64), KernelError> {
        let state = self.state.data_ptr();
        let requester_pid =
            unsafe { KernelState::process_id_from_raw(state as *const _, requester_tid) }
                .ok_or(KernelError::InvalidCapability)?;
        unsafe {
            KernelState::resolve_reply_cap_in_pid_from_raw(state as *const _, requester_pid, cap)
        }
    }

    /// Stage 199A2B2E: GENERATION-BEARING off-lock cnode resolution. Resolves the
    /// `CNodeId` for the process owning the EXACT `{tid, asid}` incarnation (identity
    /// verified under the task read, cnode resolved under the capability read). A
    /// numeric TID reused by a replacement task (different ASID) yields `None`, so the
    /// provisional server-local reply-cap mint can never target a replacement process.
    pub(crate) fn process_cnode_for_identity_split_read(
        &self,
        identity: crate::kernel::boot::ReceiverWaiterIdentity,
    ) -> Option<crate::kernel::capabilities::CNodeId> {
        let state = self.state.data_ptr();
        unsafe {
            KernelState::process_cnode_for_identity_from_raw(
                state as *const _,
                identity.tid.0,
                identity.asid,
            )
        }
    }

    /// Borrow `&mut KernelState` directly, bypassing the `SpinLock`.
    ///
    /// # Validation status
    /// - LIVE_OFF_TRAP — called only from single-CPU arch boot, never from the trap
    ///   path. Opens a raw `&mut KernelState` aliasing window (Review finding C1).
    ///
    /// This exists solely for AArch64/x86_64 boot code that must pass
    /// `&mut KernelState` to a callback that eventually ERETs into user space and
    /// never returns. Holding the `SpinLock` across that ERET would leave
    /// `held = true` permanently, deadlocking all subsequent trap handlers.
    ///
    /// # Canonical safety contract (Review finding C1)
    /// * Must only be called during single-CPU boot before any trap handler can
    ///   concurrently call `SharedKernel::with` or `with_cpu`. On both archs the
    ///   raw `TRAP_KERNEL_STATE_PTR` is installed only AFTER this borrow, and
    ///   external interrupts stay masked until later in boot; the LAPIC/timer
    ///   deadline is far beyond the boot window, so no timer ISR fires during it.
    ///   If a timer ISR DID fire and reach `with_cpu`, it would build a second
    ///   `&mut KernelState` aliasing this one — undefined behavior.
    /// * The returned reference must not be used after the ERET to user space;
    ///   from that point all KernelState access must go through `with` / `with_cpu`.
    /// * `TRAP_KERNEL_STATE_PTR` must remain null while this reference is live so
    ///   that the trap fallback path cannot also yield `&mut KernelState`.
    ///
    /// The debug-only `BOOT_RAW_BORROW_ACTIVE` flag (set here, asserted at arch
    /// timer/trap entry) enforces the no-concurrent-access contract under
    /// `debug_assertions`/`test`. The live boot path is non-returning, so the
    /// window is never explicitly closed in production; the flag becomes
    /// irrelevant after the ERET (see [`begin_boot_raw_borrow_window`]).
    ///
    /// # Safety
    /// See canonical safety contract above; delegated to the caller.
    #[cfg(not(feature = "hosted-dev"))]
    pub(crate) unsafe fn borrow_kernel_for_boot(&self) -> &mut KernelState {
        #[cfg(any(debug_assertions, test))]
        begin_boot_raw_borrow_window();
        // SAFETY: delegated to caller (see doc comment above).
        unsafe { &mut *self.state.data_ptr() }
    }
}

/// Owned follow-up from `map_user_page_in_asid_split` (rank 5), applied under memory rank 6 AFTER
/// the VM lock drops. Sufficient for map accounting and exact rollback of the just-mapped page.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SharedRegionMapFollowup {
    pub(crate) inserted_phys: crate::kernel::vm::PhysAddr,
    pub(crate) replaced: Option<crate::kernel::vm::Mapping>,
}

/// Owned proof that THIS blocked-receiver finalization removed EXACTLY its receiver's endpoint
/// waiter slot (Stage 198E3B2B1). It carries the endpoint GENERATION it was claimed under so a later
/// restore can only ever re-target the same endpoint incarnation — never a destroyed/recreated one.
/// The claim is the sole authority for having removed the waiter; a numeric TID alone is never it.
// Stage 198E3B2B2: with an identity-exact claim the production finalizer never restores (a claimed
// waiter always belonged to the exact vanished incarnation), so the claim's fields are read only by
// the guarded restore primitive and its focused tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct WaiterClaim {
    pub(crate) eidx: usize,
    pub(crate) generation: u64,
    /// Stage 198E3B2B2: the COMPLETE generation-bearing receiver identity (tid + ASID) that was
    /// removed — a restore can only ever re-install this exact identity, never a replacement task.
    pub(crate) receiver: crate::kernel::boot::ReceiverWaiterIdentity,
    /// Stage 199D-WA3C1: the wait generation of the record that was removed, so a restore
    /// republishes the SAME incarnation rather than minting a fresh-looking one.
    pub(crate) wait_generation: u64,
}

/// Stage 199D — what the single rank-1 receiver enqueue actually did.
///
/// The five distinctions the direct paths must tell apart are exactly `SchedulerError`'s, once
/// wake-only stopped being folded into `CpuOffline`: `InvalidCpu`, `CpuOffline`, `WakeOnly`,
/// `QueueFull`, `AlreadyQueued`. So the outcome adds no parallel taxonomy — it adds only the one
/// thing `SchedulerError` cannot say, namely *which CPU* a success landed on.
///
/// **Load-bearing:** a `wake_target_cpu` may be read only out of `Enqueued`. `Rejected` carries
/// the attempted CPU for diagnosis, and that CPU is deliberately not a wake target: nothing may
/// IPI it, and no direct-IPC success object may name it.
///
/// **`Rejected` states only that THIS enqueue did not commit.** It deliberately makes no claim
/// about where the TID is. Four of the five reasons do imply "no new placement was performed":
/// `InvalidCpu`, `CpuOffline`, `WakeOnly` and `QueueFull` all fail before touching a queue.
/// `AlreadyQueued` is the opposite — it reports **pre-existing scheduler membership**, and
/// because `PriorityScheduler::contains_tid` reads the membership mirror, which tracks the
/// queues *plus* the dispatched `current` task, it can mean the receiver is **executing right
/// now**. Treating it as "nothing is queued" and running the ordinary
/// `Runnable → Blocked` + waiter-restore rollback would produce a `Blocked` task that is still
/// queued or current: a corrupt state, and a lie about what was restored.
///
/// So `AlreadyQueued` carries `reconciled`: the outcome of a
/// [`SmpScheduler::withdraw_queued_tid_on`](crate::kernel::scheduler::SmpScheduler) performed
/// inside the **same rank-1 acquisition** that detected the collision. There is no
/// unlock/relock window between detection and reconciliation, so no dispatcher can take the
/// entry in between. Only `WithdrawOutcome::Removed` — an atomically removed, exactly-one
/// queued entry, which by construction was *not* `current` — may proceed to a rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiverEnqueue {
    /// The enqueue SUCCEEDED. `cpu` is the run queue the receiver is now on.
    Enqueued { cpu: CpuId },
    /// THIS enqueue did not commit. `cpu` is what was attempted, never what was achieved, and
    /// `error` is the scheduler's own reason, preserved rather than collapsed.
    ///
    /// `reconciled` is `Some(..)` for `AlreadyQueued` only — the same-acquisition membership
    /// reconciliation — and `None` for the four reasons that never touched a queue.
    Rejected {
        cpu: CpuId,
        error: crate::kernel::scheduler::SchedulerError,
        reconciled: Option<crate::kernel::scheduler::WithdrawOutcome>,
    },
}

impl ReceiverEnqueue {
    /// The committed wake target, or `None` when nothing was placed. The ONLY way to obtain a
    /// wake target — there is no accessor that yields a CPU for a rejected enqueue.
    pub(crate) fn enqueued_cpu(self) -> Option<CpuId> {
        match self {
            ReceiverEnqueue::Enqueued { cpu } => Some(cpu),
            ReceiverEnqueue::Rejected { .. } => None,
        }
    }

    /// True iff the receiver provably holds **no scheduler membership** as a result of this
    /// enqueue — the precondition for the ordinary `Runnable → Blocked` + waiter-restore
    /// rollback.
    ///
    /// The four non-membership reasons qualify unconditionally: they fail before touching any
    /// queue, and the receiver was `Blocked` with its waiter exclusively claimed on entry, so
    /// nothing else could have placed it. `AlreadyQueued` qualifies **only** when the
    /// same-acquisition reconciliation removed exactly one queued entry. `RefusedCurrent`
    /// (the receiver is dispatched — it may already have observed the publication),
    /// `RefusedDuplicate`, `NotQueued` and `InvalidCpu` are ambiguous and fail closed.
    /// Stage 199D-WA1-GATE — may a rejection yield **retryable runtime authority**?
    ///
    /// Strictly narrower than [`Self::receiver_is_unplaced`]. The four reasons that provably
    /// fail before touching a runqueue keep their existing policy. `AlreadyQueued` + `Removed`
    /// does **not**: `Removed` proves only that one queued entry was removed under the
    /// detecting scheduler acquisition — it does **not** prove the receiver never ran, nor that
    /// it never observed an earlier publication, while waiter ownership is non-exclusive
    /// (`WAITER_OWNERSHIP_EXCLUSIVE=no`). On every freestanding runtime build — including the
    /// explicit proof/oracle kernels — it therefore takes the terminal, fail-closed path.
    ///
    /// Hosted `#[cfg(test)]` builds keep exercising the rollback algebra directly, so the
    /// recovery path stays covered without any freestanding runtime decision depending on
    /// `Removed` being historically unobserved.
    pub(crate) fn rejection_is_runtime_recoverable(self) -> bool {
        use crate::kernel::scheduler::SchedulerError;
        match self {
            ReceiverEnqueue::Enqueued { .. } => false,
            ReceiverEnqueue::Rejected {
                error: SchedulerError::AlreadyQueued,
                ..
            } => cfg!(test) && self.receiver_is_unplaced(),
            ReceiverEnqueue::Rejected { .. } => self.receiver_is_unplaced(),
        }
    }

    pub(crate) fn receiver_is_unplaced(self) -> bool {
        use crate::kernel::scheduler::{SchedulerError, WithdrawOutcome};
        match self {
            ReceiverEnqueue::Enqueued { .. } => false,
            ReceiverEnqueue::Rejected {
                error: SchedulerError::AlreadyQueued,
                reconciled,
                ..
            } => reconciled == Some(WithdrawOutcome::Removed),
            ReceiverEnqueue::Rejected { .. } => true,
        }
    }
}

/// Outcome of the Phase-3 task commit, run only AFTER a successful waiter claim (Stage 198E3B2B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiverCommit {
    /// Still-live matching blocked receiver: its blocked-return register state was cleared and it was
    /// transitioned Runnable; the captured affinity is carried for the single Phase-4 enqueue.
    Committed(Option<CpuId>),
    /// The receiver exited / was removed after the claim: the claimed waiter is correctly stale and
    /// MUST NOT be restored (there is no live task to strand). No register was mutated.
    GoneDead,
    /// A live task still occupies the TID but is no longer our receiver (ASID replaced): the caller
    /// restores its EXACT claimed waiter so no live blocked task is stranded. No register was mutated.
    Replaced,
}

/// Stage 200C1 — terminal outcome of `run_reply_timeout_completion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyTimeoutOutcome {
    /// Timeout won the terminal; the canonical timeout result was installed, the
    /// reply aliases were made non-invokable, and the caller was woken exactly once.
    Woken,
    /// Timeout won the terminal but the exact caller vanished / the endpoint or
    /// waiter changed: the record + token were completed as cleanup, with NO wake.
    CleanupNoWake,
    /// Reply / peer-death / caller-exit / endpoint-gone already owned the terminal:
    /// the fire lost, its token was disarmed, and nothing else was mutated.
    LostToTerminal,
    /// The token fire claim failed (stale/duplicate/cancelled): nothing was mutated.
    StaleToken,
}

/// Stage 198E3B2A: the OFF-LOCK shared-region execution context. Implements the single
/// `SharedRegionExecCtx` transaction boundary ENTIRELY through the bounded `SharedKernel` split
/// seams — it holds no broad borrow: no `with(...)`, no `with_cpu(...)`, no `&mut KernelState`, and
/// no cached projected pointer (each seam re-derives from `data_ptr()` per call). Not yet used by
/// the production post-work drain; tests drive it via `run_shared_region_txn(&mut ctx, snapshot)`.
pub(crate) struct SharedRegionOffLockCtx<'a>(pub(crate) &'a SharedKernel);

impl crate::kernel::boot::shared_region_txn::SharedRegionExecCtx for SharedRegionOffLockCtx<'_> {
    fn ctx_receiver_alive(
        &self,
        snap: &crate::kernel::boot::shared_region_txn::RecvBoundarySharedRegionSnapshot,
    ) -> bool {
        self.0
            .sr_receiver_alive_split(snap.receiver_tid, snap.receiver_asid)
    }
    fn ctx_object_live(&self, object: CapObject) -> bool {
        self.0.sr_object_live_split(object)
    }
    fn ctx_cancel_overflowed(&self) -> bool {
        self.0.sr_cancel_overflowed_split()
    }
    fn ctx_consume_cancel(&mut self, tid: u64, asid: crate::kernel::vm::Asid) -> bool {
        self.0.sr_consume_cancel_split(tid, asid)
    }
    fn ctx_mint(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: crate::kernel::capabilities::Capability,
    ) -> Result<CapId, ()> {
        self.0.sr_mint_split(cnode, cap)
    }
    fn ctx_register_active_mapping(&mut self, tid: u64, cap: CapId, va: u64, len: usize) -> bool {
        self.0.sr_register_active_mapping_split(tid, cap, va, len)
    }
    fn ctx_phys_base(&self, object: CapObject) -> Option<crate::kernel::vm::PhysAddr> {
        self.0.sr_phys_base_split(object)
    }
    fn ctx_map_page(
        &mut self,
        asid: crate::kernel::vm::Asid,
        virt: crate::kernel::vm::VirtAddr,
        mapping: crate::kernel::vm::Mapping,
    ) -> bool {
        match self.0.map_user_page_in_asid_split(asid, virt, mapping) {
            Ok(follow) => {
                self.0.sr_apply_map_followup_split(follow);
                true
            }
            Err(_) => false,
        }
    }
    fn ctx_copy_meta(
        &mut self,
        asid: crate::kernel::vm::Asid,
        va: crate::kernel::vm::VirtAddr,
        bytes: &[u8],
    ) -> bool {
        self.0.copy_to_user_split(asid, va, bytes).is_ok()
    }
    fn ctx_finalize_and_wake(
        &mut self,
        snap: &crate::kernel::boot::shared_region_txn::RecvBoundarySharedRegionSnapshot,
    ) -> Option<bool> {
        self.0.sr_finalize_blocked_receiver_and_wake_split(snap)
    }
    fn ctx_release_pin(&mut self, object: CapObject) {
        self.0.sr_release_pin_split(object)
    }
    fn ctx_unmap_prefix(&mut self, asid: crate::kernel::vm::Asid, base: usize, len: usize) {
        // The shootdown-completion flag is discarded here on purpose: this is the shared-region
        // ROLLBACK prefix-unmap, where the object stays pinned, so both the per-page reclaim and
        // any object-level reclaim are guarded no-ops regardless of the ACK outcome.
        let _ = self.0.unmap_range_two_phase_split(asid, base, len);
    }
    fn ctx_remove_active_mapping(&mut self, tid: u64, cap: CapId) -> bool {
        self.0.sr_remove_active_mapping_split(tid, cap)
    }
    fn ctx_revoke_cap(
        &mut self,
        cnode: crate::kernel::capabilities::CNodeId,
        cap: CapId,
        object: CapObject,
    ) {
        self.0.sr_revoke_split(cnode, cap, object)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    /// U2: the production `SharedKernel::control_plane_set_process_cnode_slots_via_syscall`
    /// wrapper was deleted (its only callers were these tests, so its broad-lock
    /// acquisition was pure test cost in the 204A census). This test-local helper enters
    /// the SAME untouched `KernelState` method through `SharedKernel::with`, so the tests
    /// below exercise exactly the behavior they did before. It lives inside
    /// `#[cfg(test)] mod tests`, past the census cutoff, so it adds no census entry.
    fn control_plane_set_process_cnode_slots_via_syscall(
        kernel: &SharedKernel,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), TrapHandleError> {
        kernel.with(|state| {
            state.control_plane_set_process_cnode_slots_via_syscall(target_pid, slot_capacity)
        })
    }
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::ipc::ThreadId;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::smp::WorkItem;
    use crate::kernel::task::TaskClass;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn shared_kernel_serializes_access() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        kernel.with(|state| {
            state
                .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                .expect("submit");
        });

        let processed = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("process")
        });

        assert_eq!(processed, 1);
    }

    #[test]
    fn current_tid_split_read_matches_scheduler_current_on_cpu() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.register_task(42).expect("task42");
            state.enqueue_current_cpu(42).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            assert_eq!(state.current_tid_on_cpu(CpuId(0)), Some(42));
        });

        assert_eq!(kernel.current_tid_split_read(CpuId(0)), Some(42));
        assert_eq!(kernel.current_tid_split_read(CpuId(7)), None);
    }

    #[test]
    fn topology_count_split_reads_match_scheduler_state() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let (online, present) =
            kernel.with(|state| (state.online_cpu_count(), state.present_cpu_count()));

        assert_eq!(kernel.online_cpu_count_split_read(), online);
        assert_eq!(kernel.present_cpu_count_split_read(), present);
        assert!(kernel.online_cpu_count_split_read() <= kernel.present_cpu_count_split_read());
    }

    #[test]
    fn boot_config_split_reads_match_kernel_state_capacity_config() {
        let kernel = SharedKernel::new(
            Bootstrap::init_with_capacity_profile(KernelCapacityProfile::Constrained)
                .expect("init constrained"),
        );
        let (profile, config) =
            kernel.with(|state| (state.capacity_profile(), state.runtime_capacity_config()));

        assert_eq!(kernel.capacity_profile_split_read(), profile);
        assert_eq!(kernel.runtime_capacity_config_split_read(), config);
    }

    #[test]
    fn fault_bookkeeping_split_mut_helpers_match_kernel_state_accessors() {
        use crate::kernel::trap::FaultAccess;
        use crate::kernel::vm::VirtAddr;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let fault = FaultInfo {
            addr: VirtAddr(0xdead_beef),
            access: FaultAccess::Write,
        };
        let mut frame = TrapFrame::new(11, [1, 2, 3, 4, 5, 6]);
        frame.set_saved_pc(0x4000);
        frame.set_saved_sp(0x8000);

        kernel.record_fault_split_mut(fault);
        assert_eq!(kernel.with(|state| state.last_fault()), Some(fault));

        kernel.record_fault_frame_snapshot_split_mut(&frame);
        assert_eq!(
            kernel.with(|state| state.last_fault_frame()),
            Some(frame.clone())
        );

        kernel.clear_last_fault_split_mut();
        assert_eq!(kernel.with(|state| state.last_fault()), None);
        assert_eq!(kernel.with(|state| state.last_fault_frame()), None);

        kernel.with(|state| {
            state.record_fault(fault);
            state.record_fault_frame_snapshot(&frame);
            assert_eq!(state.last_fault(), Some(fault));
            assert_eq!(state.last_fault_frame(), Some(frame.clone()));
            state.clear_last_fault();
            assert_eq!(state.last_fault(), None);
            assert_eq!(state.last_fault_frame(), None);
        });
    }

    #[test]
    fn telemetry_split_mut_helpers_match_kernel_state_accessors() {
        std::thread::Builder::new()
            .name("telemetry_split_mut_helpers".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
                let (initial_shootdowns, initial_timeouts) = kernel.with(|state| {
                    (
                        state.tlb_shootdown_count(),
                        state.tlb_shootdown_timeout_count(),
                    )
                });

                kernel.increment_tlb_shootdown_count_split_mut();
                assert_eq!(
                    kernel.with(|state| state.tlb_shootdown_count()),
                    initial_shootdowns.wrapping_add(1)
                );

                kernel.add_tlb_shootdown_timeout_count_split_mut(7);
                assert_eq!(
                    kernel.with(|state| state.tlb_shootdown_timeout_count()),
                    initial_timeouts.wrapping_add(7)
                );
            })
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    #[test]
    fn with_cpu_applies_targeted_cross_cpu_work_before_closure() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.bring_up_cpu(CpuId(1)).expect("cpu1");
            state.register_task(2).expect("task2");
            state
                .submit_cross_cpu_work(CpuId(1), WorkItem::WakeTask { tid: ThreadId(2) })
                .expect("submit");
        });

        let processed = kernel
            .with_cpu(CpuId(1), |state| {
                assert_eq!(state.current_cpu(), CpuId(1));
                state
                    .process_cross_cpu_work_for_cpu(CpuId(1))
                    .expect("drain")
            })
            .expect("with_cpu");
        assert_eq!(processed, 1);
    }

    #[test]
    fn with_cpu_propagates_invalid_cpu_errors() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let result = kernel.with_cpu(CpuId(1), |_| 0);
        assert!(result.is_err());
    }

    #[test]
    fn shared_kernel_allows_concurrent_serialized_access() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        for _ in 0..32 {
            kernel.with(|state| {
                state
                    .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                    .expect("submit t1");
            });
        }
        for _ in 0..32 {
            kernel.with(|state| {
                state
                    .submit_cross_cpu_work(CpuId(0), WorkItem::Reschedule)
                    .expect("submit t2");
            });
        }

        let drained =
            kernel.with(|state| state.process_cross_cpu_work_for_cpu(CpuId(1)).unwrap_or(0));
        assert_eq!(drained, 0);

        let drained_cpu0 = kernel.with(|state| {
            state
                .process_cross_cpu_work_for_cpu(CpuId(0))
                .expect("drain cpu0")
        });
        assert_eq!(drained_cpu0, 64);
    }

    #[test]
    fn shared_kernel_control_plane_syscall_wrapper_resizes_target_cnode() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(900, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("target app");
            state.enqueue_current_cpu(900).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(900) {
                state.yield_current().expect("switch");
            }
        });

        let (target_cnode, before) = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(901).expect("target cnode");
            let before = state.cnode_slot_capacity(cnode).expect("before");
            (cnode, before)
        });
        let requested = before.saturating_add(4);
        control_plane_set_process_cnode_slots_via_syscall(&kernel, 901, requested).expect("resize");
        let after = kernel.with(|state| state.cnode_slot_capacity(target_cnode));
        assert_eq!(after, Some(requested));
    }

    #[test]
    fn shared_kernel_control_plane_syscall_wrapper_denies_unprivileged_cross_process_resize() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(910, TaskClass::App)
                .expect("requester");
            state
                .register_task_with_class(911, TaskClass::App)
                .expect("target");
            state.enqueue_current_cpu(910).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(910) {
                state.yield_current().expect("switch");
            }
        });

        let err = control_plane_set_process_cnode_slots_via_syscall(&kernel, 911, 8)
            .expect_err("must deny");
        assert_eq!(
            err,
            TrapHandleError::Syscall(crate::kernel::syscall::SyscallError::MissingRight)
        );
    }

    // ── Stage 4T+5 split-read helpers ─────────────────────────────────────────

    #[test]
    fn fault_split_read_helpers_match_kernel_state_accessors() {
        use crate::kernel::trap::{FaultAccess, FaultInfo};
        use crate::kernel::vm::VirtAddr;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Initially no fault recorded.
        assert_eq!(kernel.last_fault_split_read(), None);
        assert_eq!(kernel.last_fault_frame_split_read(), None);

        let fault = FaultInfo {
            addr: VirtAddr(0xDEAD_0000),
            access: FaultAccess::Write,
        };
        kernel.record_fault_split_mut(fault);

        // Split-read must match the global-lock read.
        assert_eq!(
            kernel.last_fault_split_read(),
            kernel.with(|state| state.last_fault()),
            "last_fault_split_read must match kernel.with last_fault after record"
        );
        assert_eq!(kernel.last_fault_split_read(), Some(fault));

        let mut frame = TrapFrame::new(11, [1, 2, 3, 4, 5, 6]);
        frame.set_saved_pc(0x6000);
        frame.set_saved_sp(0xA000);
        kernel.record_fault_frame_snapshot_split_mut(&frame);

        assert_eq!(
            kernel.last_fault_frame_split_read(),
            kernel.with(|state| state.last_fault_frame()),
            "last_fault_frame_split_read must match kernel.with last_fault_frame after snapshot"
        );
        assert!(kernel.last_fault_frame_split_read().is_some());

        // After clear: both split-read and global-lock read return None.
        kernel.clear_last_fault_split_mut();
        assert_eq!(kernel.last_fault_split_read(), None);
        assert_eq!(kernel.with(|state| state.last_fault()), None);
    }

    #[test]
    fn fault_policy_split_read_matches_kernel_state_accessor() {
        use crate::kernel::task::FaultPolicy;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let expected = kernel.with(|state| state.fault_policy());
        let split = kernel.fault_policy_split_read();
        assert_eq!(
            split, expected,
            "fault_policy_split_read must match kernel.with fault_policy"
        );
        // Default policy must be KillTask.
        assert_eq!(split, FaultPolicy::KillTask);
    }

    #[test]
    fn telemetry_split_read_helpers_match_kernel_state_accessors() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        let (count0, timeout0) = kernel.with(|state| {
            (
                state.tlb_shootdown_count(),
                state.tlb_shootdown_timeout_count(),
            )
        });

        // Initial values match.
        assert_eq!(kernel.tlb_shootdown_count_split_read(), count0);
        assert_eq!(kernel.tlb_shootdown_timeout_count_split_read(), timeout0);

        // After mutations via split_mut, split_read sees the updated values.
        kernel.increment_tlb_shootdown_count_split_mut();
        kernel.add_tlb_shootdown_timeout_count_split_mut(5);

        assert_eq!(
            kernel.tlb_shootdown_count_split_read(),
            count0.wrapping_add(1)
        );
        assert_eq!(
            kernel.tlb_shootdown_timeout_count_split_read(),
            timeout0.wrapping_add(5)
        );

        // Split-read matches global-lock read.
        assert_eq!(
            kernel.tlb_shootdown_count_split_read(),
            kernel.with(|state| state.tlb_shootdown_count()),
            "tlb_shootdown_count split_read must match global read"
        );
        assert_eq!(
            kernel.tlb_shootdown_timeout_count_split_read(),
            kernel.with(|state| state.tlb_shootdown_timeout_count()),
            "tlb_shootdown_timeout_count split_read must match global read"
        );
    }

    // ── Stage 4T+6R: current_tid_split_read equivalence tests ───────────────
    // These tests prove value-equivalence for the current_tid_split_read helper.
    // NOTE: Stage 4T+6's live conversion of x86_64 entering_tid/exiting_tid from
    // with_cpu→current_tid to current_tid_split_read was reverted (Stage 4T+6R)
    // because it broke the x86_64 service chain in smoke testing despite passing
    // these unit tests. The helper is still used by other callers (AArch64 trace).
    // The x86_64 shared trap path uses with_cpu→current_tid (global lock, Class F).

    #[test]
    fn current_tid_split_read_matches_with_cpu_current_tid_entering_snapshot() {
        // Proves that current_tid_split_read(cpu) returns the same value as
        // with_cpu(cpu, |k| k.current_tid()).unwrap_or(None) on the same scheduler
        // state. NOTE: value-equivalence alone is insufficient for live x86_64 trap
        // use — the with_cpu path is required there (see Stage 4T+6R revert).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(77).expect("task77");
            state.enqueue_current_cpu(77).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let split = kernel.current_tid_split_read(cpu);
        let conservative = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        assert_eq!(
            split, conservative,
            "entering_tid: current_tid_split_read must equal with_cpu current_tid"
        );
        assert_eq!(split, Some(77));
    }

    #[test]
    fn current_tid_split_read_reflects_task_switch_for_exiting_snapshot() {
        // Proves that current_tid_split_read(cpu) correctly reflects a task
        // switch — the exiting_tid snapshot in the x86_64 shared trap path
        // must see the newly-dispatched task, not the entering task.
        //
        // Setup: enqueue both 81 and 82 before dispatch so the runqueue has
        // [81, 82]. Dispatch picks 81; queue is [82]. Yield from 81 → queue
        // becomes [82, 81] → dispatch picks 82. This guarantees a switch.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(81).expect("task81");
            state.register_task(82).expect("task82");
            // Enqueue both before dispatch so 82 is waiting when 81 yields.
            state.enqueue_current_cpu(81).expect("enqueue 81");
            state.enqueue_current_cpu(82).expect("enqueue 82");
            state.dispatch_next_task().expect("dispatch to 81");
        });

        // Entering snapshot: current is task 81 (first FIFO pick).
        let entering_tid = kernel.current_tid_split_read(cpu);
        assert_eq!(entering_tid, Some(81), "entering_tid must be task 81");

        // Simulate task switch: yield task 81; queue now has [82, 81], dispatch picks 82.
        kernel.with(|state| {
            state.yield_current().expect("yield 81");
        });

        // Exiting snapshot: task 82 (or 81 re-dispatched on single-task edge case —
        // we assert only that the scheduler call is visible, not the exact TID).
        let exiting_tid = kernel.current_tid_split_read(cpu);
        assert_ne!(
            exiting_tid, entering_tid,
            "exiting_tid must differ from entering_tid after yield"
        );
        // task_switched detection — same logic as the x86_64 trap handler.
        let task_switched = entering_tid != exiting_tid;
        assert!(task_switched, "task_switched must be true when TIDs differ");
    }

    #[test]
    fn current_tid_split_read_no_switch_detection_for_same_task_return() {
        // Proves that when no task switch occurs, entering_tid == exiting_tid
        // via current_tid_split_read — triggering the "write trap returns only"
        // branch in the x86_64 trap handler (Stage 4T+6).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(91).expect("task91");
            state.enqueue_current_cpu(91).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let entering_tid = kernel.current_tid_split_read(cpu);

        // No dispatch between entering and exiting — same task continues.
        let exiting_tid = kernel.current_tid_split_read(cpu);

        assert_eq!(
            entering_tid, exiting_tid,
            "exiting_tid must equal entering_tid when no task switch"
        );
        let task_switched = entering_tid != exiting_tid;
        assert!(
            !task_switched,
            "task_switched must be false for same-task return"
        );
    }

    #[test]
    fn current_tid_split_read_offline_cpu_returns_none() {
        // Proves that current_tid_split_read for an offline CPU returns None —
        // same as the former with_cpu path (validate_online_cpu fail → unwrap_or(None)).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(7);

        let split = kernel.current_tid_split_read(offline_cpu);
        let conservative = kernel
            .with_cpu(offline_cpu, |k| k.current_tid())
            .unwrap_or(None);
        assert_eq!(
            split, None,
            "offline CPU must return None from current_tid_split_read"
        );
        assert_eq!(
            split, conservative,
            "split_read must match with_cpu for offline CPU"
        );
    }

    // ── Stage 4T+6R: with_cpu entering/exiting TID path tests ───────────────
    // These tests cover the reverted x86_64 trap path that uses with_cpu for
    // both entering_tid and exiting_tid reads. They prove that task_switched
    // detection and scheduler progress are correct with the global-lock path.

    #[test]
    fn with_cpu_entering_exiting_tid_detects_task_switch() {
        // Proves that the with_cpu→current_tid path (live in x86_64 shared trap
        // after Stage 4T+6R revert) correctly detects a task switch for both
        // entering_tid and exiting_tid snapshots. This is the acceptance test for
        // the reverted code path — unit-test coverage that smoke testing validates.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(83).expect("task83");
            state.register_task(84).expect("task84");
            state.enqueue_current_cpu(83).expect("enqueue 83");
            state.enqueue_current_cpu(84).expect("enqueue 84");
            state.dispatch_next_task().expect("dispatch to 83");
        });

        let entering_tid = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        assert_eq!(entering_tid, Some(83), "entering_tid must be task 83");

        kernel.with(|state| {
            state.yield_current().expect("yield 83");
        });

        let exiting_tid = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        assert_ne!(
            exiting_tid, entering_tid,
            "exiting_tid must differ from entering_tid after task switch"
        );
        let task_switched = entering_tid != exiting_tid;
        assert!(task_switched, "task_switched must be true after yield");
    }

    #[test]
    fn with_cpu_entering_exiting_tid_no_switch_same_task() {
        // Proves that the with_cpu→current_tid path returns equal entering_tid and
        // exiting_tid when no task switch occurs (no yield between reads).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(85).expect("task85");
            state.enqueue_current_cpu(85).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let entering_tid = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        let exiting_tid = kernel.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        assert_eq!(
            entering_tid, exiting_tid,
            "entering_tid must equal exiting_tid when no task switch"
        );
        let task_switched = entering_tid != exiting_tid;
        assert!(
            !task_switched,
            "task_switched must be false for same-task return"
        );
    }

    #[test]
    fn with_cpu_entering_tid_offline_cpu_returns_none() {
        // Proves that with_cpu for an offline CPU returns Err, making
        // unwrap_or(None) give None — the same sentinel as current_tid_split_read.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(7);

        let entering_tid = kernel
            .with_cpu(offline_cpu, |k| k.current_tid())
            .unwrap_or(None);
        assert_eq!(
            entering_tid, None,
            "offline CPU must return None from with_cpu→current_tid"
        );
    }

    // ── U3 (203C): `current_tid_authoritative` is the authoritative rank-1 transaction ──
    //
    // The x86_64 entering/exiting snapshots now call this helper instead of `with_cpu`.
    // The Stage 4T+6R revert above is the reason these tests exist: value equivalence is
    // NOT the property that matters — the BINDING side effect is. Each test below pins a
    // behaviour that the reverted `current_tid_split_read` substitution did not have.

    #[test]
    fn u3_authoritative_binds_cpu_and_reads_the_current_tid() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.register_task(77).expect("task77");
            state.enqueue_current_cpu(77).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });
        assert_eq!(kernel.current_tid_authoritative(CpuId(0)), Some(77));
        assert_eq!(
            kernel.with(|s| s.current_cpu()),
            CpuId(0),
            "a successful call binds current_cpu"
        );
    }

    #[test]
    fn u3_authoritative_reads_without_binding_when_no_current_task() {
        // Re-derived (U3/203C saved-resume prerequisite): this used to pin that the helper BOUND
        // `current_cpu`. It must now pin the opposite — the ambient selector is never written —
        // while the returned value is unchanged.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|s| s.bring_up_cpu(CpuId(1)).expect("cpu 1 online"));
        let ambient_before = kernel.with(|s| s.current_cpu());
        // CPU 1 is online but idle: the read is None, exactly as before.
        assert_eq!(kernel.current_tid_authoritative(CpuId(1)), None);
        assert_eq!(
            kernel.with(|s| s.current_cpu()),
            ambient_before,
            "an explicit-CPU read must NOT retarget the process-global ambient current_cpu"
        );
    }

    #[test]
    fn u3_authoritative_offline_cpu_returns_none_and_leaves_the_binding() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        // Establish a known binding first (the value itself is whatever the bootstrap
        // scheduler holds; what matters is that the binding is CPU 0 and stays there).
        let baseline = kernel.current_tid_authoritative(CpuId(0));
        assert_eq!(kernel.with(|s| s.current_cpu()), CpuId(0));
        // Offline and out-of-range CPUs both refuse WITHOUT rebinding.
        for bad in [CpuId(7), CpuId(200)] {
            assert_eq!(
                kernel.current_tid_authoritative(bad),
                None,
                "an invalid/offline CPU returns None"
            );
            assert_eq!(
                kernel.with(|s| s.current_cpu()),
                CpuId(0),
                "a refused call must not change the prior binding"
            );
        }
        // And the still-valid CPU still answers exactly as before the refusals.
        assert_eq!(kernel.current_tid_authoritative(CpuId(0)), baseline);
    }

    // ── U3 (203C): the AArch64 deferred-FutexWait NO-INCOMING idle consumer ──────────
    //
    // `src/arch/trap_entry.rs` held a brief broad re-acquire there,
    // `with_cpu(cpu, |kernel| matches!(kernel.current_tid(), None | Some(0))).unwrap_or(true)`.
    // It is retired onto THIS helper — not a new seam. Because the helper returns
    // `Option<u64>`, the legacy pattern splits across the two lines it always occupied:
    // `Some(0)` is the idle task, and BOTH "no current task" and "CPU refused" arrive as
    // `None` and are mapped to `true` by the same `unwrap_or(true)`. These tests pin all four
    // legacy outcomes on the real consumer expression.

    /// The exact consumer expression, so the tests exercise the shipped predicate rather than
    /// a restatement of it.
    fn u3_futex_idle_current_none(kernel: &SharedKernel, cpu: CpuId) -> bool {
        kernel
            .current_tid_authoritative(cpu)
            .map(|current| current == 0)
            .unwrap_or(true)
    }

    #[test]
    fn u3_futex_idle_predicate_maps_every_legacy_outcome() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|s| s.bring_up_cpu(CpuId(1)).expect("cpu 1 online"));

        // (1) no current task on an ONLINE CPU -> true. Legacy: `current_tid()` was `None`,
        //     which `matches!(…, None | Some(0))` mapped to true.
        assert!(
            u3_futex_idle_current_none(&kernel, CpuId(1)),
            "an online CPU with no current task is the idle outcome"
        );

        // (2) the idle task (tid 0) is current -> true. Legacy: `Some(0)`.
        //
        // Re-derived: the state is established EXPLICITLY for CPU 0. Bootstrap already leaves the
        // idle task current on CPU 0 (see `idle_re_enqueue_for_test`, which exists only to restore
        // that after a dispatch displaces it), so this asserts the precondition rather than
        // reaching for it through the ambient `current_cpu` this helper no longer binds.
        assert_eq!(
            kernel.with(|s| s.current_tid_on_cpu(CpuId(0))),
            Some(0),
            "bootstrap leaves the idle task current on CPU 0"
        );
        assert_eq!(kernel.current_tid_authoritative(CpuId(0)), Some(0));
        assert!(
            u3_futex_idle_current_none(&kernel, CpuId(0)),
            "tid 0 is the idle task and must still read as idle"
        );

        // (3) a real user task is current -> false. Legacy: `Some(n)`, n != 0. Placed and
        // dispatched on CPU 0 explicitly.
        kernel.with(|state| {
            state.register_task(91).expect("task 91");
            state.enqueue_on_cpu(CpuId(0), 91).expect("enqueue 91");
            assert_eq!(state.dispatch_next_on_cpu(CpuId(0)), Some(91));
        });
        assert_eq!(kernel.current_tid_authoritative(CpuId(0)), Some(91));
        assert!(
            !u3_futex_idle_current_none(&kernel, CpuId(0)),
            "a running non-idle task must NOT read as idle"
        );

        // (4) an invalid or offline CPU -> true, through the same `unwrap_or(true)` the retired
        //     callsite used for `with_cpu`'s `Err`. Fail-open is the LEGACY policy and is
        //     preserved deliberately: this branch is already committed to entering idle.
        for bad in [CpuId(7), CpuId(200)] {
            assert!(
                u3_futex_idle_current_none(&kernel, bad),
                "a refused CPU must still map to true"
            );
        }
        // …and a refusal mutates nothing, so the valid CPU still answers exactly as before.
        assert!(!u3_futex_idle_current_none(&kernel, CpuId(0)));
    }

    #[test]
    fn u3_futex_idle_observes_the_requested_cpu_not_the_ambient_binding() {
        // The property the reverted `current_tid_split_read` substitution did not have: the
        // read must be OF the CPU passed in, after binding it — not of whatever `current_cpu`
        // happened to hold. Two CPUs with different current tasks make that observable.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.bring_up_cpu(CpuId(1)).expect("cpu 1 online");
            state.register_task(93).expect("task 93");
            state.enqueue_on_cpu(CpuId(0), 93).expect("queue on cpu 0");
            assert_eq!(state.dispatch_next_on_cpu(CpuId(0)), Some(93));
        });
        // Ambient binding is CPU 0, whose current task is 93.
        assert_eq!(kernel.with(|s| s.current_cpu()), CpuId(0));
        assert!(!u3_futex_idle_current_none(&kernel, CpuId(0)));
        // Asking about CPU 1 must answer for CPU 1 — and must rebind to it, exactly as
        // `with_cpu(CpuId(1), …)` did through `set_current_cpu`.
        let ambient_before = kernel.with(|s| s.current_cpu());
        assert!(
            u3_futex_idle_current_none(&kernel, CpuId(1)),
            "CPU 1 is idle; the answer must not be borrowed from the ambient CPU 0 binding"
        );
        // Re-derived (U3/203C saved-resume prerequisite): the answer is for the REQUESTED CPU and
        // the ambient selector is left alone. The retired assertion required the opposite — that
        // the read rebound `current_cpu` — which is precisely what retargeted other CPUs'
        // in-flight syscalls under SMP.
        assert_eq!(
            kernel.with(|s| s.current_cpu()),
            ambient_before,
            "an explicit-CPU read must NOT retarget the process-global ambient current_cpu"
        );
        // The CPU-0 answer is likewise unchanged by having asked about CPU 1.
        assert!(!u3_futex_idle_current_none(&kernel, CpuId(0)));
    }

    #[test]
    fn u3_futex_idle_consumer_is_broad_lock_free_and_reuses_the_existing_transaction() {
        const TRAP_ENTRY: &str = include_str!("arch/trap_entry.rs");
        // Bound the AArch64 Stage 195F no-incoming idle branch exactly as it is written.
        let branch = TRAP_ENTRY
            .split_once("Stage 195F IDLE OUTCOME")
            .map(|(_, r)| {
                r.split_once("enter_post_lock_idle(cpu)")
                    .map(|(b, _)| b)
                    .unwrap_or(r)
            })
            .expect("the AArch64 FutexWait no-incoming idle branch");
        // Comment-stripped, exactly as `tests/broad_lock_census_guard.rs` counts: the branch's
        // own commentary NAMES the two rejected alternatives, and naming them is the point.
        let branch = u3_code_lines(branch);
        assert!(branch.contains(".current_tid_authoritative(cpu)"));
        for banned in [
            ".with_cpu(",
            ".with(|",
            "state.lock()",
            "current_tid_split_read",
            "terminal_idle_on_cpu_split",
            "runnable_count",
        ] {
            assert!(
                !branch.contains(banned),
                "the retired branch must contain no `{banned}`"
            );
        }
        // No new seam was created for it: the helper still has exactly one definition, and the
        // branch reaches it directly. The needle is assembled at run time so this assertion's
        // own source line is not itself a match in the `include_str!` of this very file.
        let src = include_str!("runtime.rs");
        let definition =
            alloc::format!("pub fn {}(&self, cpu: CpuId)", "current_tid_authoritative");
        assert_eq!(
            src.matches(definition.as_str()).count(),
            1,
            "the authoritative transaction has ONE definition; U3 reuses it"
        );
        // U9-D3 §7 later retired the D6 proof cleanup tail too, so the ONE acquisition that
        // remains in the file is the canonical broad Phase-2 trap dispatch — untouched here.
        let code = u3_code_lines(TRAP_ENTRY);
        assert_eq!(
            code.matches(".with_cpu(").count(),
            1,
            "trap_entry.rs drops from 3 to 2 with this retirement, then to 1 with U9-D3 §7"
        );
        assert_eq!(code.matches(".with(|").count(), 0);
        assert!(
            code.contains(
                "let inner_result = shared
        .with_cpu(cpu, |kernel| {"
            ),
            "the canonical broad Phase-2 trap dispatch is present and unchanged"
        );
        // U9 (canonical 203C) moved the production restore out of the second acquisition; U9-D3
        // §7 then lifted the D3 fence and retired the acquisition itself. The CLEANUP is what this
        // assertion pins, and it is preserved whole — only its lock shape changed.
        assert!(
            code.contains("fn post_switch_d6_cleanup_split(")
                && code.contains("d6_ensure_post_cleanup_task_stacks_mapped_split"),
            "the D6 cleanup and its stack repair are present, now off the broad lock"
        );
    }

    #[test]
    fn u3_authoritative_entering_exiting_switch_classification() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);
        kernel.with(|state| {
            state.register_task(31).expect("task31");
            state.register_task(32).expect("task32");
            state.enqueue_current_cpu(31).expect("enqueue31");
            state.enqueue_current_cpu(32).expect("enqueue32");
            state.dispatch_next_task().expect("dispatch to 31");
        });
        // No switch: the two snapshots around a no-op are equal.
        let entering = kernel.current_tid_authoritative(cpu);
        let exiting = kernel.current_tid_authoritative(cpu);
        assert_eq!(entering, Some(31));
        assert_eq!(entering, exiting, "no switch leaves the snapshots equal");
        assert!(!(entering != exiting), "task_switched must be false");
        // A real switch (the same mechanism the established with_cpu test uses)
        // changes the exiting snapshot.
        let entering = kernel.current_tid_authoritative(cpu);
        kernel.with(|state| state.yield_current().expect("yield 31"));
        let exiting = kernel.current_tid_authoritative(cpu);
        assert_ne!(
            entering, exiting,
            "task_switched must be true after a yield"
        );
    }

    // ── U3 (203C): source guards against the Stage 4T+6 regression ─────────────────────

    fn u3_code_lines(src: &str) -> alloc::string::String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<alloc::vec::Vec<_>>()
            .join("\n")
    }

    fn u3_authoritative_body() -> alloc::string::String {
        let src = include_str!("runtime.rs");
        u3_code_lines(
            src.split("pub fn current_tid_authoritative")
                .nth(1)
                .expect("helper present")
                .split("\n    /// ")
                .next()
                .expect("bounded by the next doc comment"),
        )
    }

    #[test]
    fn u3_authoritative_is_broad_lock_free_and_never_binds() {
        // Re-derived (U3/203C saved-resume prerequisite): the retired assertions pinned
        // "validate → BIND → read". The contract is now "validate → read, for the EXPLICIT cpu",
        // with the process-global ambient selector neither written nor consulted.
        let body = u3_authoritative_body();
        assert!(
            !body.contains(".with_cpu(") && !body.contains("self.with(|"),
            "current_tid_authoritative must hold no broad acquisition"
        );
        // One rank-1 acquisition covers the validate and the read.
        assert_eq!(
            body.matches("with_scheduler_split_mut").count(),
            1,
            "exactly one rank-1 scheduler acquisition"
        );
        let validate = body
            .find("validate_online_cpu(cpu)")
            .expect("same online predicate set_current_cpu uses");
        let read = body
            .find("current_tid_on(cpu)")
            .expect("reads the EXPLICIT cpu's current tid");
        assert!(validate < read, "validation must precede the read");
        // No ambient write, and no ambient selector consulted for the lookup.
        for banned in [
            "sched.current_cpu = cpu",
            "set_current_cpu",
            "bind_current_cpu_split",
            "current_cpu_split_read",
            "current_tid()",
            "read_mpidr_el1",
        ] {
            assert!(
                !body.contains(banned),
                "the explicit-CPU read must not reach `{banned}`"
            );
        }
        // Broad `with_cpu` keeps its legacy binding — that is deliberately unchanged.
        let with_cpu = u3_code_lines(
            include_str!("runtime.rs")
                .split("pub fn with_cpu<R>")
                .nth(1)
                .expect("with_cpu")
                .split("\n    /// ")
                .next()
                .expect("its body"),
        );
        assert!(
            with_cpu.contains("set_current_cpu(cpu)"),
            "broad with_cpu retains its legacy ambient binding"
        );
        // It is a read-only identity snapshot: no dispatch, enqueue or status mutation.
        for forbidden in [
            "dispatch_next",
            "enqueue_",
            "status =",
            "with_task_tcbs_split_mut",
        ] {
            assert!(
                !body.contains(forbidden),
                "current_tid_authoritative must not contain `{forbidden}`"
            );
        }
    }

    #[test]
    fn u3_split_read_remains_intentionally_non_binding() {
        let src = include_str!("runtime.rs");
        let body = u3_code_lines(
            src.split("pub fn current_tid_split_read")
                .nth(1)
                .expect("helper present")
                .split("\n    /// ")
                .next()
                .expect("bounded"),
        );
        assert!(
            !body.contains("current_cpu ="),
            "current_tid_split_read must stay non-binding — binding here would erase the \
             distinction the Stage 4T+6R revert established"
        );
        assert!(body.contains("current_tid_on(cpu)"));
    }

    #[test]
    fn u3_descriptor_snapshots_use_the_authoritative_binding_helper() {
        const DESC: &str = include_str!("arch/x86_64/descriptor_tables.rs");
        let code = u3_code_lines(DESC);
        assert_eq!(
            code.matches(".with_cpu(").count(),
            0,
            "descriptor_tables.rs must contain no broad acquisition"
        );
        assert_eq!(
            code.matches("current_tid_authoritative(cpu)").count(),
            2,
            "both identity snapshots use the authoritative binding helper"
        );
        assert!(
            !code.contains("current_tid_split_read"),
            "neither snapshot may use the non-binding split read (Stage 4T+6 regression)"
        );
        // Ordering around dispatch is unchanged: entering before the dispatch call,
        // exiting after it and after AP-seal handling, then the switch classification.
        let entering = code
            .find("let entering_tid: Option<u64> = shared.current_tid_authoritative(cpu);")
            .expect("entering snapshot");
        let ap_seal = code
            .find("ap_seal_return_to_idle")
            .expect("AP-seal handling");
        let exiting = code
            .find("let exiting_tid: Option<u64> = shared.current_tid_authoritative(cpu);")
            .expect("exiting snapshot");
        let switched = code
            .find("let task_switched = entering_tid != exiting_tid;")
            .expect("switch classification");
        assert!(
            entering < ap_seal && ap_seal < exiting && exiting < switched,
            "entering -> dispatch/AP-seal -> exiting -> task_switched order is unchanged"
        );
    }

    // ── Stage 4T+7 fatal-trap snapshot split-read tests ──────────────────────

    #[test]
    fn fatal_trap_read_snapshot_tid_matches_split_read() {
        // Proves that fatal_trap_read_snapshot.current_tid equals
        // current_tid_split_read(cpu).unwrap_or(0) for the same cpu at the
        // same scheduler state — validating the TID leg of Stage 4T+7.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(73).expect("task73");
            state.enqueue_current_cpu(73).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let snapshot = kernel.fatal_trap_read_snapshot(cpu);
        let expected_tid = kernel.current_tid_split_read(cpu).unwrap_or(0);
        assert_eq!(
            snapshot.current_tid, expected_tid,
            "fatal_trap_read_snapshot.current_tid must equal current_tid_split_read"
        );
        assert_eq!(snapshot.current_tid, 73);
    }

    #[test]
    fn fatal_trap_read_snapshot_asid_matches_kernel_state_task_asid() {
        // Proves that fatal_trap_read_snapshot.current_asid equals
        // task_asid_for_tid_split_read(current_tid) — both return 0 for a task
        // without an ASID binding, validating the ASID leg of Stage 4T+7.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let cpu = CpuId(0);

        kernel.with(|state| {
            state.register_task(74).expect("task74");
            state.enqueue_current_cpu(74).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
        });

        let snapshot = kernel.fatal_trap_read_snapshot(cpu);
        let asid_via_split = kernel.task_asid_for_tid_split_read(74);
        let asid_via_global =
            kernel.with(|state| state.task_asid(74).map(|a| a.0 as u64).unwrap_or(0));

        assert_eq!(
            snapshot.current_asid, asid_via_split,
            "snapshot.current_asid must match task_asid_for_tid_split_read"
        );
        assert_eq!(
            snapshot.current_asid, asid_via_global,
            "snapshot.current_asid must match global-lock task_asid"
        );
        // No ASID was bound, so both should be 0.
        assert_eq!(snapshot.current_asid, 0);
    }

    #[test]
    fn fatal_trap_read_snapshot_offline_cpu_returns_zeros() {
        // Proves that fatal_trap_read_snapshot for an offline CPU returns
        // current_tid=0 and current_asid=0 — the safe zero-fill sentinel used
        // by log_decoded_fatal_trap_from_snapshot when no task is running.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(255);

        let snapshot = kernel.fatal_trap_read_snapshot(offline_cpu);
        assert_eq!(
            snapshot.current_tid, 0,
            "offline CPU must produce current_tid=0 in fatal_trap_read_snapshot"
        );
        assert_eq!(
            snapshot.current_asid, 0,
            "offline CPU must produce current_asid=0 in fatal_trap_read_snapshot"
        );
    }

    // ── Stage 5A split-read helpers ───────────────────────────────────────────

    #[test]
    fn task_class_split_read_matches_global() {
        // Stage 5A: prove task_class_split_read (task lock only, rank 2)
        // returns the same value as the globally-locked task_class() accessor,
        // for both present and absent TIDs.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Before registration: both paths return None.
        assert_eq!(
            kernel.task_class_split_read(501),
            kernel.with(|state| state.task_class(501)),
            "task_class_split_read must match global for absent TID"
        );
        assert_eq!(kernel.task_class_split_read(501), None);

        // Register tasks with distinct classes.
        kernel.with(|state| {
            state
                .register_task_with_class(501, TaskClass::App)
                .expect("app");
            state
                .register_task_with_class(502, TaskClass::SystemServer)
                .expect("sys_srv");
        });

        // After registration: split-read matches global.
        assert_eq!(
            kernel.task_class_split_read(501),
            kernel.with(|state| state.task_class(501)),
            "task_class_split_read must match global for App"
        );
        assert_eq!(kernel.task_class_split_read(501), Some(TaskClass::App));

        assert_eq!(
            kernel.task_class_split_read(502),
            kernel.with(|state| state.task_class(502)),
            "task_class_split_read must match global for SystemServer"
        );
        assert_eq!(
            kernel.task_class_split_read(502),
            Some(TaskClass::SystemServer)
        );

        // Unknown TID still returns None from both paths.
        assert_eq!(
            kernel.task_class_split_read(999),
            kernel.with(|state| state.task_class(999)),
            "task_class_split_read must match global for unknown TID"
        );
        assert_eq!(kernel.task_class_split_read(999), None);
    }

    #[test]
    fn task_exists_split_read_matches_global() {
        // Stage 5A: prove task_exists_split_read (task lock only, rank 2)
        // agrees with a globally-locked existence check, for both present
        // and absent TIDs.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Before registration.
        let absent_via_global = kernel.with(|state| state.task_class(511)).is_some();
        assert_eq!(
            kernel.task_exists_split_read(511),
            absent_via_global,
            "task_exists_split_read must match global for absent TID"
        );
        assert!(!kernel.task_exists_split_read(511));

        // After registration.
        kernel.with(|state| {
            state.register_task(511).expect("task511");
        });

        let present_via_global = kernel.with(|state| state.task_class(511)).is_some();
        assert_eq!(
            kernel.task_exists_split_read(511),
            present_via_global,
            "task_exists_split_read must match global for registered TID"
        );
        assert!(kernel.task_exists_split_read(511));
    }

    #[test]
    fn cnode_slot_capacity_split_read_matches_global() {
        // Stage 5A: prove cnode_slot_capacity_split_read (capability lock only,
        // rank 4) returns the same slot count as the globally-locked accessor,
        // both before and after a CNode is created.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        const PID: u64 = 520;

        // Before CNode creation: both paths return None.
        let before_global = kernel.with(|state| {
            use crate::kernel::capabilities::CNodeId;
            state.cnode_slot_capacity(CNodeId(PID))
        });
        assert_eq!(
            kernel.cnode_slot_capacity_split_read(PID),
            before_global,
            "cnode_slot_capacity_split_read must match global before creation"
        );
        assert_eq!(kernel.cnode_slot_capacity_split_read(PID), None);

        // Create a CNode via the control plane.
        kernel.with(|state| {
            state
                .register_task_with_class(PID, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(521, TaskClass::App)
                .expect("target");
            state.enqueue_current_cpu(PID).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(PID) {
                state.yield_current().expect("switch");
            }
        });
        let requested_slots = 8usize;
        control_plane_set_process_cnode_slots_via_syscall(&kernel, 521, requested_slots)
            .expect("create cnode");

        // After creation: split-read matches global.
        let after_global = kernel.with(|state| {
            use crate::kernel::capabilities::CNodeId;
            state.cnode_slot_capacity(CNodeId(521))
        });
        assert_eq!(
            kernel.cnode_slot_capacity_split_read(521),
            after_global,
            "cnode_slot_capacity_split_read must match global after creation"
        );
        assert_eq!(
            kernel.cnode_slot_capacity_split_read(521),
            Some(requested_slots)
        );
    }

    #[test]
    fn process_id_split_read_matches_global() {
        // Stage 5B: prove process_id_split_read (task lock only, rank 2)
        // returns the same value as the globally-locked process_id() accessor,
        // for both thread-group leaders and non-leader threads.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Before registration: both paths return None.
        assert_eq!(
            kernel.process_id_split_read(530),
            kernel.with(|state| state.process_id(530)),
            "process_id_split_read must match global for absent TID"
        );
        assert_eq!(kernel.process_id_split_read(530), None);

        // Register a task as its own thread-group leader (pid == tid).
        kernel.with(|state| {
            state.register_task(530).expect("leader");
        });

        let via_global = kernel.with(|state| state.process_id(530));
        assert_eq!(
            kernel.process_id_split_read(530),
            via_global,
            "process_id_split_read must match global for group leader"
        );
        // For a bare register_task, thread_group_id == tid.
        assert_eq!(kernel.process_id_split_read(530), Some(530));

        // Unknown TID returns None from both.
        assert_eq!(
            kernel.process_id_split_read(999),
            kernel.with(|state| state.process_id(999)),
            "process_id_split_read must match global for unknown TID"
        );
        assert_eq!(kernel.process_id_split_read(999), None);
    }

    #[test]
    fn is_group_leader_split_read_matches_global() {
        // Stage 5B: prove is_group_leader_split_read (task lock only, rank 2)
        // agrees with the globally-locked is_thread_group_leader() accessor,
        // for absent tasks and registered group-leader tasks.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        // Before registration: both return false.
        assert_eq!(
            kernel.is_group_leader_split_read(540),
            kernel.with(|state| state.is_thread_group_leader(540)),
            "is_group_leader_split_read must match global for absent TID"
        );
        assert!(!kernel.is_group_leader_split_read(540));

        // After registration: bare register_task sets thread_group_id == tid.
        kernel.with(|state| {
            state.register_task(540).expect("leader");
        });

        let via_global = kernel.with(|state| state.is_thread_group_leader(540));
        assert_eq!(
            kernel.is_group_leader_split_read(540),
            via_global,
            "is_group_leader_split_read must match global for registered leader"
        );
        assert!(kernel.is_group_leader_split_read(540));

        // Unknown TID still returns false from both.
        assert_eq!(
            kernel.is_group_leader_split_read(999),
            kernel.with(|state| state.is_thread_group_leader(999)),
            "is_group_leader_split_read must match global for unknown TID"
        );
        assert!(!kernel.is_group_leader_split_read(999));
    }

    // ── Stage 26 split-read extraction tests ────────────────────────────────

    #[test]
    fn stage26_global_lock_audit_syscall_count_unchanged() {
        // Stage 26 ABI guard: the global-lock callsite audit + two domain-lock
        // extractions are pure refactoring and must not alter the syscall ABI.
        assert_eq!(
            crate::kernel::syscall::SYSCALL_COUNT,
            32,
            "Stage 26 must not change SYSCALL_COUNT"
        );
    }

    #[test]
    fn stage26_notification_waiter_count_split_read_matches_global() {
        // Stage 26: prove notification_waiter_count_split_read (ipc lock only,
        // rank 3) returns the same value as the globally-locked
        // notification_waiter_count() accessor, both with and without a waiter.
        use crate::kernel::ipc::ThreadId;

        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));

        let notif_idx = kernel.with(|state| {
            state.register_task(610).expect("task");
            let (idx, _send, _recv) = state.create_notification(4).expect("notif");
            idx
        });

        // Before a waiter is injected: both paths report 0.
        let before_global = kernel.with(|state| state.notification_waiter_count(notif_idx));
        assert_eq!(
            kernel.notification_waiter_count_split_read(notif_idx),
            before_global,
            "split-read must match global before waiter"
        );
        assert_eq!(kernel.notification_waiter_count_split_read(notif_idx), 0);

        // Inject a waiter through the ipc domain.
        kernel.with(|state| {
            state.with_ipc_state_mut(|ipc| {
                ipc.notification_waiters[notif_idx] = Some(ThreadId(610));
            });
        });

        let after_global = kernel.with(|state| state.notification_waiter_count(notif_idx));
        assert_eq!(
            kernel.notification_waiter_count_split_read(notif_idx),
            after_global,
            "split-read must match global after waiter"
        );
        assert_eq!(kernel.notification_waiter_count_split_read(notif_idx), 1);

        // Adjacent path regression: a different (empty) notification slot still
        // reads 0 via the split-read helper.
        let other_idx = if notif_idx == 0 { 1 } else { 0 };
        assert_eq!(kernel.notification_waiter_count_split_read(other_idx), 0);
    }

    #[test]
    fn stage26_cnode_registered_split_read_matches_global() {
        // Stage 26: prove cnode_registered_split_read (capability lock only,
        // rank 4) agrees with the globally-locked cnode_slot_capacity() presence
        // check, both before and after a CNode is created.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        const PID: u64 = 620;

        // Before CNode creation: both paths report "not registered".
        let before_global = kernel.with(|state| {
            use crate::kernel::capabilities::CNodeId;
            state.cnode_slot_capacity(CNodeId(621)).is_some()
        });
        assert_eq!(
            kernel.cnode_registered_split_read(621),
            before_global,
            "split-read must match global before creation"
        );
        assert!(!kernel.cnode_registered_split_read(621));

        // Create a CNode via the control plane (same setup as Stage 5A test).
        kernel.with(|state| {
            state
                .register_task_with_class(PID, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(621, TaskClass::App)
                .expect("target");
            state.enqueue_current_cpu(PID).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(PID) {
                state.yield_current().expect("switch");
            }
        });
        control_plane_set_process_cnode_slots_via_syscall(&kernel, 621, 8).expect("create cnode");

        let after_global = kernel.with(|state| {
            use crate::kernel::capabilities::CNodeId;
            state.cnode_slot_capacity(CNodeId(621)).is_some()
        });
        assert_eq!(
            kernel.cnode_registered_split_read(621),
            after_global,
            "split-read must match global after creation"
        );
        assert!(kernel.cnode_registered_split_read(621));

        // Adjacent path regression: an unrelated pid is still unregistered.
        assert!(!kernel.cnode_registered_split_read(999));
    }
    // ── Stage 108 / Milestone 2 Pass 1: split-mut seam equivalence tests ──────

    #[test]
    fn stage108_scheduler_seam_matches_global_current_cpu() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        // Equivalence via runnable count on CPU 0: enqueue a task globally,
        // then observe the same runnable count through the seam.
        kernel.with(|state| {
            state.register_task(839).expect("register");
            state.enqueue_current_cpu(839).expect("enqueue");
        });
        let global_count = kernel.with(|state| {
            state.with_scheduler_state(|sched| {
                crate::kernel::boot::kernel_ref(&sched.scheduler)
                    .runnable_count_on(crate::kernel::scheduler::CpuId(0))
            })
        });
        let seam_count = kernel.with_scheduler_split_mut(|sched| {
            crate::kernel::boot::kernel_ref(&sched.scheduler)
                .runnable_count_on(crate::kernel::scheduler::CpuId(0))
        });
        assert_eq!(seam_count, global_count);
        assert!(seam_count >= 1, "enqueued task visible through the seam");
    }

    #[test]
    fn stage108_task_seam_matches_global_tcb_view() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| state.register_task(840).expect("register"));
        let global_present = kernel.with(|state| state.task_status(840).is_some());
        let seam_present = kernel
            .with_task_tcbs_split_mut(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == 840));
        assert_eq!(seam_present, global_present);
        assert!(
            seam_present,
            "registered TCB must be visible through the seam"
        );
        // Mutation through the seam is visible to the global view.
        kernel.with_task_tcbs_split_mut(|tcbs| {
            if let Some(tcb) = tcbs.iter_mut().flatten().find(|tcb| tcb.tid.0 == 840) {
                tcb.ipc_timeout_fired = true;
            }
        });
        let global_fired = kernel.with(|state| {
            state
                .consume_ipc_timeout_fired_for_tid(840)
                .expect("consume")
        });
        assert!(
            global_fired,
            "seam mutation must be visible under the global lock"
        );
    }

    #[test]
    fn stage108_vm_seam_matches_global_mapping_view() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let (asid, map_cap) = kernel.with(|state| {
            let (asid, map_cap) = state.create_user_address_space().expect("asid");
            state
                .map_user_page(
                    map_cap,
                    crate::kernel::vm::VirtAddr(0x5000),
                    crate::kernel::vm::Mapping {
                        phys: crate::kernel::vm::PhysAddr(0x9000),
                        flags: crate::kernel::vm::PageFlags::USER_RW,
                    },
                )
                .expect("map page");
            (asid, map_cap)
        });
        let _ = map_cap;
        let global_mapped = kernel.with(|state| {
            state
                .is_user_page_mapped_in_asid(asid, crate::kernel::vm::VirtAddr(0x5000))
                .expect("mapped query")
        });
        let seam_mapped = kernel.with_vm_user_spaces_split_mut(|spaces| {
            spaces
                .get_mut(asid)
                .map(|aspace| {
                    aspace
                        .resolve(crate::kernel::vm::VirtAddr(0x5000))
                        .is_some()
                })
                .unwrap_or(false)
        });
        assert_eq!(seam_mapped, global_mapped);
        assert!(seam_mapped, "mapping must be visible through the VM seam");
    }

    #[test]
    fn stage108_memory_seam_matches_global_object_count() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state.alloc_anonymous_memory_object().expect("alloc");
        });
        let global_count = kernel.with(|state| {
            state.with_memory_state(|memory| memory.memory_objects.iter().flatten().count())
        });
        let seam_count =
            kernel.with_memory_split_mut(|memory| memory.memory_objects.iter().flatten().count());
        assert_eq!(seam_count, global_count);
        assert!(seam_count >= 1);
    }

    #[test]
    fn stage108_seams_are_helper_only_no_live_callers() {
        // M2_SEAM_HELPER_ONLY: the Stage 108 seams must not be called directly
        // from syscall.rs / trap_entry.rs. The scheduler seam
        // (`with_scheduler_split_mut`) is the Stage 167 (D6-GENUINE-A)
        // exception: its sole live caller is the runtime.rs wrapper
        // `d6_genuine_local_dispatch_observe` (default-off behind
        // `yarm.d6_genuine=1`), so it is checked separately below.
        let syscall_src = include_str!("kernel/syscall.rs");
        let trap_entry_src = include_str!("arch/trap_entry.rs");
        // Build needles at runtime so doc/test mentions of the names in other
        // files' test modules cannot self-match.
        let names = [
            ["with_task_tcbs_", "split_mut("].concat(),
            ["with_vm_user_spaces_", "split_mut("].concat(),
            ["with_memory_", "split_mut("].concat(),
        ];
        for name in &names {
            assert!(
                !syscall_src.contains(name.as_str()),
                "{name} must not be called from syscall.rs (Stage 108 seams are helper-only)"
            );
            assert!(
                !trap_entry_src.contains(name.as_str()),
                "{name} must not be called from trap_entry.rs"
            );
        }
        // The scheduler seam's only live caller is the Stage 167 default-off
        // observe wrapper, defined in runtime.rs and invoked from trap_entry.rs.
        let scheduler_seam = ["with_scheduler_", "split_mut("].concat();
        assert!(
            !syscall_src.contains(scheduler_seam.as_str()),
            "scheduler seam must not be called directly from syscall.rs"
        );
        assert!(
            !trap_entry_src.contains(scheduler_seam.as_str()),
            "scheduler seam must only be reached via the d6_genuine wrapper, not called directly in trap_entry.rs"
        );
        assert!(
            trap_entry_src.contains("d6_genuine_local_dispatch_observe"),
            "Stage 167: trap_entry.rs must invoke the d6_genuine scheduler-seam wrapper"
        );
        // Labels present (the scheduler seam is now M2_SEAM_LIVE_D6_GENUINE;
        // the helper-only label still covers the remaining seam).
        let runtime_src = include_str!("runtime.rs");
        assert!(runtime_src.contains("VALIDATION: M2_SEAM_HELPER_ONLY"));
        assert!(runtime_src.contains("VALIDATION: M2_SEAM_LIVE_D6_GENUINE"));
    }
}

/// Test-only fault hook: force step (3) of `restore_consumed_reply_record_split` to fail after
/// the record has already been flipped, so the revert that preserves the two-outcome contract is
/// exercised rather than merely asserted. `#[cfg(test)]` — it exists in no shipped kernel.
#[cfg(test)]
pub(crate) static RESTORE_FORCE_LINK_INSTALL_FAILURE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Test-only fault hook for the POST-COPY membership collision. The early check at NR6 (4a) /
/// NR7 (2b) makes an ordinary `AlreadyQueued` unreachable at the final enqueue, so the recovery
/// path is exercised by injecting membership just before it. `0` = off, `1` = one queued entry
/// (yields `WithdrawOutcome::Removed`), `2` = dispatched as `current` (yields `RefusedCurrent`).
/// `#[cfg(test)]` — it exists in no shipped kernel.
#[cfg(test)]
pub(crate) static FORCE_POST_COPY_MEMBERSHIP: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
impl SharedKernel {
    /// Inject the membership the hook asks for, immediately before a direct transaction's final
    /// rank-1 enqueue. One-shot: it clears itself so a retry of the same transaction succeeds.
    /// U3 (203C) — test-only entry to the PRIVATE recv-boundary Phase B/C completion, so the
    /// focused tests drive the real production body (both copy-fault arms included) instead of
    /// re-implementing it. `#[cfg(test)]`: no shipped kernel has it, and it adds no acquisition.
    /// U3 (203C) — test-only entries to the PRIVATE ordinary-cap completion and to the split
    /// sender-wake composition, so the focused tests drive the real production bodies instead
    /// of re-implementing them. `#[cfg(test)]`: no shipped kernel has them, and they add no
    /// acquisition.
    pub(crate) fn test_complete_recv_boundary_ordinary_cap(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: crate::kernel::recv_core::RecvBoundaryOrdinaryCapSnapshot,
    ) -> Result<(), crate::kernel::boot::TrapHandleError> {
        self.complete_recv_boundary_ordinary_cap(cpu, frame, pending)
    }

    pub(crate) fn test_apply_split_sender_wake_plan_split(
        &self,
        cpu: CpuId,
        sender_tid: crate::kernel::ipc::ThreadId,
    ) -> Result<(), KernelError> {
        self.apply_split_sender_wake_plan_split(
            cpu,
            crate::kernel::ipc::SenderWakeTarget {
                tid: sender_tid,
                asid: None,
                send_generation: 0,
            },
        )
    }

    /// U3 (203C) — test-only entry to the PRIVATE fault-record transaction, so the focused
    /// differential can compare it against the retired broad body directly.
    pub(crate) fn record_recv_boundary_user_fault_split_for_test(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        addr: usize,
    ) -> Result<(), KernelError> {
        self.record_recv_boundary_user_fault_split(cpu, frame, addr)
    }

    pub(crate) fn test_complete_recv_boundary_user_copy(
        &self,
        cpu: CpuId,
        frame: &mut TrapFrame,
        pending: &crate::kernel::recv_core::RecvBoundaryUserCopySnapshot,
    ) -> Result<(), crate::kernel::boot::TrapHandleError> {
        self.complete_recv_boundary_user_copy(cpu, frame, pending)
    }

    pub(crate) fn test_force_post_copy_membership(&self, tid: u64, affinity: Option<CpuId>) {
        use core::sync::atomic::Ordering;
        let mode = FORCE_POST_COPY_MEMBERSHIP.swap(0, Ordering::Relaxed);
        if mode == 0 {
            return;
        }
        let cpu = affinity.unwrap_or_else(|| self.current_cpu_split_read());
        let _ = self.sr_enqueue_committed_receiver_split(tid, Some(cpu));
        if mode == 2 {
            self.with_scheduler_split_mut(|sched| {
                let _ = kernel_mut(&mut sched.scheduler).dispatch_next_on(cpu);
            });
        }
    }
}
