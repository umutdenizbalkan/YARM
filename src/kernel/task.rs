// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::capabilities::CapId;
use super::ipc::ThreadId;
use super::scheduler::CpuId;
use super::vm::{Asid, VirtAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RestartToken(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadGroupId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(CapId),
    EndpointSend(CapId),
    Futex(VirtAddr),
    Join(ThreadId),
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvAbiVariant {
    RecvV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedRecvState {
    pub recv_cap: CapId,
    pub payload_user_ptr: usize,
    pub payload_user_len: usize,
    pub meta_user_ptr: usize,
    pub meta_user_len: usize,
    pub recv_abi: RecvAbiVariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    App,
    Driver,
    SystemServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPolicy {
    KillTask,
    NotifyAndContinue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Runnable,
    /// Stage 199D-WA3B: a one-shot **spawn reservation**, not a live task.
    ///
    /// Bootstrap must own a task's CNode, class and kernel-side provisioning BEFORE the spawn
    /// runs, because the boot capability grants need a destination CNode. Historically that
    /// pre-registration produced an ordinary `Runnable` TCB, which is exactly what let
    /// `spawn_user_task_from_image` overwrite an arbitrary existing task.
    ///
    /// A `Reserved` task is deliberately none of the live states: it is not `Runnable`, so no
    /// dispatch transition accepts it; it is not `Blocked(_)`, so no wake, timeout or waiter
    /// path names it; and it is refused by `enqueue`, so it can never reach a run queue. It
    /// leaves this state exactly once, through the typed spawn commit.
    Reserved,
    /// Set only by `KernelState::dispatch_next_task()` / yield scheduling paths.
    /// Do not assign directly outside scheduler-mediated transitions.
    Running,
    Blocked(WaitReason),
    Faulted,
    Exited(u64),
    Dead,
}

/// Stage 199D-WA3B: which half of the one-shot spawn protocol a reservation is in.
///
/// `ReservedUnstarted` is the resting state a reservation is created in and restored to if a
/// spawn fails. `Spawning` is held only across a single `spawn_user_task_from_image` call, so a
/// second consume of the same token cannot find a `ReservedUnstarted` reservation to claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPhase {
    /// Reserved and idle: provisioned, not started, consumable exactly once.
    ReservedUnstarted,
    /// Claimed by an in-flight spawn. Not consumable.
    Spawning,
}

/// Stage 199D-WA3B: the authoritative reservation record carried by a `Reserved` TCB.
///
/// `generation` is drawn from a monotonic kernel counter, never from the TID, so a token minted
/// for an earlier occupant of numeric TID `T` cannot authorize a later occupant of `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnReservation {
    pub(crate) generation: u64,
    pub(crate) class: TaskClass,
    pub(crate) process_pid: u64,
    pub(crate) phase: SpawnPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadDetachState {
    Joinable,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserRegisterContext {
    pub instruction_ptr: VirtAddr,
    pub stack_ptr: VirtAddr,
    pub user_gprs: [usize; 32],
    pub arg0: usize,
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
    pub arg4: usize,
    pub arg5: usize,
}

impl Default for UserRegisterContext {
    fn default() -> Self {
        Self {
            instruction_ptr: VirtAddr(0),
            stack_ptr: VirtAddr(0),
            user_gprs: [0; 32],
            arg0: 0,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RobustFutexState {
    pub head: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestartState {
    pub token: Option<RestartToken>,
}

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchSwitchContext {
    words: [usize; 8],
    fxsave: [u8; 512],
}

impl Default for ArchSwitchContext {
    fn default() -> Self {
        Self {
            words: [0; 8],
            fxsave: [0; 512],
        }
    }
}

impl ArchSwitchContext {
    pub const WORDS: usize = 8;
    pub const FXSAVE_BYTES: usize = 512;
    const STACK_PTR_IDX: usize = 0;
    const INSTRUCTION_PTR_IDX: usize = 1;

    pub const fn stack_ptr(self) -> usize {
        self.words[Self::STACK_PTR_IDX]
    }

    pub fn set_stack_ptr(&mut self, value: usize) {
        self.words[Self::STACK_PTR_IDX] = value;
    }

    pub const fn instruction_ptr(self) -> usize {
        self.words[Self::INSTRUCTION_PTR_IDX]
    }

    pub fn set_instruction_ptr(&mut self, value: usize) {
        self.words[Self::INSTRUCTION_PTR_IDX] = value;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KernelExecutionContext {
    pub stack_base: Option<VirtAddr>,
    pub stack_top: Option<VirtAddr>,
    pub frame: ArchSwitchContext,
    pub initialized: bool,
    pub owns_stack: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: ThreadId,
    pub thread_group_id: ThreadGroupId,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub tls_ptr: Option<VirtAddr>,
    pub user_entry: Option<VirtAddr>,
    pub user_stack_top: Option<VirtAddr>,
    pub user_context: UserRegisterContext,
    pub detach_state: ThreadDetachState,
    /// `None` means fallback to kernel/class policy in `KernelState`.
    pub fault_policy_override: Option<FaultPolicy>,
    pub restart: RestartState,
    pub kernel_context: KernelExecutionContext,
    /// If set, scheduler enqueues this task only on the selected CPU.
    pub cpu_affinity: Option<CpuId>,
    /// Absolute scheduler tick at which an IPC wait should timeout.
    pub ipc_timeout_deadline: Option<u64>,
    /// Set when a blocked IPC wait is resumed due to timeout expiry.
    pub ipc_timeout_fired: bool,
    /// Saved userspace recv buffers for blocked recv-v2 completion.
    pub blocked_recv_state: Option<BlockedRecvState>,
    /// Stage 200C1 — the EXACT Stage 200B deadline-token handle for a reply-receive
    /// timeout. This REFERENCES the token in the single deadline-registration store
    /// from the existing per-TCB timeout "queue" (it is NOT a second queue, and NOT a
    /// terminal-result authority). `None` for ordinary (non-reply) recv timeouts,
    /// which keep their existing plain-deadline behavior unchanged. Dormant in
    /// production this stage (no live path registers it yet).
    pub reply_timeout_token: Option<crate::kernel::deadline_token::DeadlineTokenHandle>,
    /// Canonical 199E — the CLOCK DOMAIN the `reply_timeout_token` registration above was armed
    /// in. Written once by the single registration seam
    /// (`KernelState::register_reply_receive_deadline`) together with the token and the deadline,
    /// and never rewritten while that registration is live, so the timeout collector judges each
    /// record against its OWN clock instead of a selector-global one. Meaningless (and ignored)
    /// when `reply_timeout_token` is `None`: the ordinary-receive and blocking-send classes are
    /// scheduler-tick-only and never consult it.
    pub reply_timeout_clock: crate::kernel::deadline_token::ReplyDeadlineClock,
    /// Stage 200D — the BOUNDED, generation-bearing REVERSE link from this task (as the
    /// authorized replier) to the reply record it must answer.
    ///
    /// Teardown needs to find the records a dying server owed a reply to. Scanning every
    /// reply record on every task exit is O(MAX_REPLY_CAPS) of unrelated work and would
    /// make the death path's cost depend on unrelated IPC traffic, so the link is stored
    /// HERE — directly on the exact server TCB — and read by index at exit.
    ///
    /// Capacity is deliberately ONE. That is the honest current contract: this kernel
    /// does not support queued `IpcCall` or multiple simultaneous call/reply pairs, so a
    /// live server incarnation owes at most one outstanding reply. A second registration
    /// while one is live FAILS (it never silently overwrites), and the caller rolls the
    /// whole reply-record publication back before the request becomes externally visible.
    /// It is a fixed-size `Option`, so registration allocates nothing and is safe to run
    /// under the ranked lifecycle/IPC locks.
    pub server_reply_link: Option<ServerReplyLink>,
    /// Stage 200C1 — a monotonic per-task blocked-receive generation. Captured into
    /// the reply-timeout token identity at registration and revalidated at timeout
    /// completion, so a caller that unblocked and re-blocked (a new recv) advances
    /// this and a stale timeout completion is refused. Bumped when a fresh blocked
    /// recv is published.
    pub blocked_recv_generation: u64,
    /// U6 §2 — a monotonic per-task blocked-SEND generation, the send-side sibling of
    /// [`Self::blocked_recv_generation`] and deliberately a SEPARATE coordinate.
    ///
    /// Reusing `blocked_recv_generation` for sends would conflate two independent block
    /// cycles: a task that blocks in `ipc_send`, is woken, then blocks in `ipc_recv` would
    /// advance one shared counter, so a send completion published for the FIRST cycle could
    /// still match the receive cycle's identity (or be spuriously refused). The two classes
    /// therefore carry their own counters, and
    /// [`BlockedSyscallCompletion::blocked_generation`] is validated against the counter that
    /// belongs to its own [`BlockedSyscallClass`].
    ///
    /// Advanced by `checked_add` at the send-block commit — never `wrapping_add`. An exhausted
    /// counter REFUSES the commit rather than wrapping into a value that an ancient parked
    /// completion could match. At u64 width that is unreachable in practice; refusing is what
    /// makes the identity a proof rather than a probability.
    pub blocked_send_generation: u64,
    /// Stage 200C2C1B — a generation-bearing PENDING COMPLETION for a blocked syscall that
    /// was completed remotely (off-lock) while its caller was descheduled.
    ///
    /// On architectures whose blocked syscalls resume by SAVED-FRAME return (the AArch64
    /// port: the SVC's `ELR_EL1` already points past the instruction, so the handler is
    /// NEVER re-entered), a remote completion cannot deliver its result by "returning" from
    /// the handler — there is no second handler entry. It instead parks the outcome HERE,
    /// and the resume boundary consumes it exactly once while encoding the canonical
    /// syscall result into the resumed frame. One producer (the completion transaction),
    /// one consumer (the resume boundary); a stale generation or a replacement `{tid, asid}`
    /// incarnation is refused, so a NEW receive can never observe an OLD result and no
    /// completion is observed twice.
    pub pending_syscall_completion: Option<BlockedSyscallCompletion>,
    /// Canonical 199E-R1D — set exactly when this task's saved [`Self::user_context`] holds an
    /// ASYNCHRONOUSLY PREEMPTED register file rather than a syscall/startup lane convention.
    ///
    /// This is the explicit resume-state tag: the resume path reads it to decide whether to
    /// restore `a0..a7` verbatim or to install the startup/result lanes, and consumes it in the
    /// same step so one snapshot can never be applied twice. `None` means the ordinary lane
    /// conventions apply, exactly as before this stage. See [`AsyncPreemptedContext`].
    pub async_preempted: Option<AsyncPreemptedContext>,
    /// Canonical 199E-R1D — a monotonic per-task PREEMPTION generation, the async sibling of
    /// [`Self::blocked_recv_generation`] and [`Self::blocked_send_generation`] and deliberately
    /// a separate coordinate from both: a preemption is not a block, and conflating them would
    /// let a stale block completion match a preemption cycle or vice versa.
    ///
    /// Advanced by `checked_add` at each snapshot — never `wrapping_add`. An exhausted counter
    /// REFUSES the snapshot rather than wrapping into a value an ancient tag could match; at
    /// u64 width that is unreachable in practice, and refusing is what makes the identity a
    /// proof rather than a probability.
    pub async_preempt_generation: u64,
    /// Stage 199D-WA3B: the one-shot spawn reservation this TCB carries, if it is one.
    ///
    /// `Some` exactly when `status == TaskStatus::Reserved`. Cleared by the typed live commit,
    /// so a spawned task carries no residual reservation authority.
    pub(crate) spawn_reservation: Option<SpawnReservation>,
}

/// Stage 200D — a generation-bearing reverse link from an authorized replier to the reply
/// record it owes. Stored on the replier's own TCB (see `ThreadControlBlock::server_reply_link`).
///
/// Both the SERVER identity and the RECORD identity are generation-bearing, and both are
/// re-checked at use. A restarted task that reuses the numeric TID always carries a different
/// ASID, so it can neither inherit an old link's authority nor have its own link cancelled by a
/// stale numeric-TID sweep; a reply-record slot that was reclaimed and reused advances its
/// generation, so a link left behind by an earlier occupant refers to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerReplyLink {
    /// The authorized replier's numeric thread id. Never authorizes on its own.
    pub server_tid: u64,
    /// The replier INCARNATION. Together with `server_tid` this is the authority key.
    pub server_asid: crate::kernel::vm::Asid,
    /// Slot index in the single reply-record store.
    pub reply_record_index: usize,
    /// The slot's generation at registration — what makes a reused slot detectable.
    pub reply_record_generation: u64,
}

impl ServerReplyLink {
    /// `true` when this link still describes the given server incarnation.
    #[must_use]
    pub fn matches_server(&self, tid: u64, asid: crate::kernel::vm::Asid) -> bool {
        self.server_tid == tid && self.server_asid == asid
    }

    /// `true` when this link still describes the given record incarnation.
    #[must_use]
    pub fn matches_record(&self, index: usize, generation: u64) -> bool {
        self.reply_record_index == index && self.reply_record_generation == generation
    }
}

/// Stage 200C2C1B — which blocked syscall class a [`BlockedSyscallCompletion`] completes.
/// Arch-neutral: RISC-V (whose port shares the saved-frame resume shape) can consume the
/// same mechanism later; only AArch64 is wired in this stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedSyscallClass {
    /// A blocked `ipc_recv` / `ipc_recv_with_deadline` (recv-v2) receive.
    IpcRecv,
    /// U6 §2 — a blocked `ipc_send` / `ipc_send_with_deadline`.
    ///
    /// Unlike `IpcRecv`, this class is PRODUCTION-LIVE on every port and is not gated by any
    /// proof/oracle feature: U6 makes the blocking-send commit itself off-lock, and a blocked
    /// sender's saved frame carries `WouldBlock` from the producer, so without a published
    /// completion a woken sender would return `WouldBlock` for a message the receiver has
    /// already consumed — a duplicate delivery if the caller retried. The completion is the
    /// only thing that makes the woken sender's result true.
    IpcSend,
}

impl BlockedSyscallClass {
    /// Stable slug for markers/telemetry.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::IpcRecv => "ipc_recv",
            Self::IpcSend => "ipc_send",
        }
    }
}

/// Stage 200C2C1B — the exact, generation-bearing outcome of a remotely completed blocked
/// syscall. Carries full identity so consumption is unambiguous: the consumer must match
/// the EXACT `{tid, asid}` incarnation AND the `blocked_generation` captured when the
/// caller blocked. Purely internal — this is NOT a public ABI type and adds no syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedSyscallCompletion {
    /// Which blocked syscall class this completes.
    pub syscall_class: BlockedSyscallClass,
    /// The canonical syscall error code to encode on resume (e.g. `TimedOut`).
    pub result: u64,
    /// The exact caller thread id.
    pub tid: u64,
    /// The exact caller address-space id (a replacement incarnation differs).
    pub asid: Asid,
    /// The block generation captured when the caller blocked; a caller that unblocked and
    /// re-blocked advances this, so a stale completion is refused. Which counter it is
    /// compared against is decided by [`Self::syscall_class`] — see
    /// [`BlockedSyscallCompletion::matches_tcb`].
    pub blocked_generation: u64,
}

/// Canonical 199E-R1D — the TYPED resume state for a user context that was interrupted
/// ASYNCHRONOUSLY, i.e. by a timer interrupt at an arbitrary instruction boundary rather than
/// at a syscall.
///
/// # Why a third resume state exists
///
/// RISC-V previously had exactly two ways to resume a task, and both are *lane* conventions
/// rather than register-file restores:
///
///   * **Fresh/startup** — `a0..a5` carry the startup ABI, `a7` is zeroed. The task has never
///     run, so its `user_gprs` are all zero and mirroring them would clobber the arguments.
///   * **Syscall / blocked-syscall continuation** — `a0`/`a1` carry the syscall result lane,
///     published into both `user_gprs` and the `arg` mirror by `publish_riscv_user_return`.
///
/// Both install `a0..a5` from the argument mirror and force `a7 = 0`, which is correct for a
/// task resuming *from a syscall boundary*, where those registers are either the ABI arguments
/// or dead. It is corruption for a task preempted mid-computation: `a0..a7` are ordinary live
/// registers there, and `a7` in particular is an argument register the RISC-V ABI does not
/// preserve across anything.
///
/// Asynchronous preemption is therefore its OWN state, tagged explicitly. Nothing infers it
/// from zero registers, a PC value, a bare TID or incidental scheduler state — the tag is
/// published by the snapshot and consumed by the resume, exactly once each.
///
/// # Identity
///
/// Modelled on [`BlockedSyscallCompletion`]: a snapshot is only ever restored into the exact
/// `{tid, asid, generation}` incarnation it was taken from. A replacement task that reused the
/// numeric TID always carries a different ASID, and a task preempted twice advances its own
/// generation, so a stale tag can neither resurrect an older register file nor be consumed
/// twice. On any mismatch the resume FAILS CLOSED — it must never fall back to the startup
/// argument rewrite, because that would install a startup ABI over a running computation.
///
/// # Scope: integer registers only
///
/// This state carries the integer register file (`user_gprs`), `sepc` and `sp`, which is the
/// complete architecturally live user state **for every binary this port builds**. RISC-V user
/// targets declare the `lp64d` hard-float ABI and `sstatus.FS` is observed Dirty, so the
/// hardware permits user floating-point; no build in the tree contains a single FP instruction
/// or f-register reference, so no FP state can be live to lose. That is a real latent
/// dependency rather than a closed one — see `riscv64_async_preemption` guard
/// `no_user_floating_point_state_can_be_live`, which fails the moment it stops being true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPreemptedContext {
    /// The exact preempted thread id.
    pub tid: u64,
    /// The exact preempted address-space id (a replacement incarnation differs).
    pub asid: Asid,
    /// The preemption generation captured when the snapshot was published. A task preempted
    /// again advances this, so a stale tag cannot restore an older register file.
    pub preempt_generation: u64,
}

impl AsyncPreemptedContext {
    /// The SINGLE source of truth for "is this snapshot still exactly this task's, for the
    /// preemption it was published for?". All three coordinates must agree.
    #[must_use]
    pub fn matches_tcb(&self, tcb: &ThreadControlBlock) -> bool {
        self.tid == tcb.tid.0
            && tcb.asid == Some(self.asid)
            && self.preempt_generation == tcb.async_preempt_generation
    }
}

/// Canonical 199E-R2 — the write-back convention ONE resume boundary must use.
///
/// A resume boundary has exactly three conventions available and they are mutually exclusive.
/// Naming them as a type is what stops a boundary from silently defaulting to the startup lane
/// when it could not establish the async one — the defect this stage repairs, where an
/// asynchronously preempted task was resumed with its own `ecall` arguments reinstalled over a
/// live computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsyncResumeClass {
    /// No async tag names this incoming identity. The boundary's existing conventions apply:
    /// a syscall/D2 continuation gets its result lane, a fresh task gets the startup lane.
    Ordinary,
    /// Exactly one valid tag was consumed for this exact incarnation. The boundary MUST restore
    /// the full integer register file verbatim and MUST NOT touch the argument mirror.
    AsyncPreempted,
    /// A tag existed but could not be honoured. It has been cleared so it cannot be retried,
    /// and the boundary falls back to its ordinary conventions. The reason is a stable slug for
    /// the live marker.
    Refused(&'static str),
}

impl AsyncResumeClass {
    /// `true` only for the one class that authorizes a verbatim register-file restore.
    #[must_use]
    pub(crate) fn restores_verbatim(self) -> bool {
        matches!(self, Self::AsyncPreempted)
    }
}

/// Canonical 199E-R2 — THE incoming-identity consumer of an async-preemption tag.
///
/// This is the single implementation both RISC-V resume boundaries reach: the main trap bridge's
/// register write-back and the S-mode-idle timer dispatch. Each gets to it through its own
/// accessor (`SharedKernel::take_async_preempt_for_incoming_split` off the broad lock,
/// `KernelState::take_async_preempt_for_incoming` for the hosted probes), so the two boundaries
/// can never disagree about what `a0..a7` mean.
///
/// # Why the INCOMING identity, and nowhere else
///
/// The tag belongs to the task that was interrupted in U-mode, and it must stay attached to that
/// exact `{tid, asid, preempt_generation}` until that same task is selected as the task being
/// resumed. The previous design consumed it wherever `kernel.current_tid()` happened to be
/// readable — inside the in-lock restore, which on the post-lock dispatch route still names the
/// OUTGOING task. Measured live: 407 tags published, 407 consumed at that seam, and 0 of 187
/// switching write-backs ever saw one, because in every single case the consumed tag named the
/// outgoing task and never the resumed one. The preempted task's authorization was therefore
/// spent without effect and its next ordinary resume reinstalled the stale argument mirror over
/// a live computation.
///
/// So the identity is resolved HERE, at the boundary that actually performs the write-back, from
/// the resume TID and the ASID that boundary is about to activate.
///
/// # Fail-closed
///
/// Every refusal clears the tag and returns [`AsyncResumeClass::Refused`], never
/// [`AsyncResumeClass::Ordinary`] — a caller that cannot tell "there was nothing" from "there was
/// something I could not verify" would silently apply the startup rewrite to a preempted task,
/// which is the whole defect. Refusals are:
///
/// * `unresolved_incoming_asid` — the boundary could not name the incarnation it is resuming
///   into, so no tag can be validated against it;
/// * `identity_mismatch` — the tag, the TCB and the boundary's own resolved ASID do not all
///   agree (a replacement incarnation that reused the numeric TID);
/// * `stale_generation` — the tag names a superseded preemption cycle;
/// * `no_saved_context` — the task carries a tag but no saved instruction pointer, so the
///   context the tag promises does not exist;
/// * `continuation_coexists` — the task holds BOTH an async tag and a parked syscall
///   completion. Those states are mutually exclusive by construction (a tag is published only
///   for a RUNNING task, a completion only for a BLOCKED one), so observing both means the
///   record is ambiguous and neither may silently win.
pub(crate) fn classify_and_take_async_resume(
    tcbs: &mut [Option<ThreadControlBlock>],
    incoming_tid: u64,
    incoming_asid: Option<Asid>,
) -> AsyncResumeClass {
    let Some(tcb) = tcbs
        .iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == incoming_tid)
    else {
        return AsyncResumeClass::Ordinary;
    };
    // No tag is the ordinary, overwhelmingly common case, and it is decided BEFORE the identity
    // checks so a boundary that resumes an untagged task with an unresolved ASID is not told a
    // refusal happened when nothing was ever at stake.
    let Some(tag) = tcb.async_preempted else {
        return AsyncResumeClass::Ordinary;
    };
    let Some(expected) = incoming_asid else {
        tcb.async_preempted = None;
        return AsyncResumeClass::Refused("unresolved_incoming_asid");
    };
    // Three-way agreement: the boundary's own resolved incarnation, the TCB's, and the tag's.
    // Checking the boundary's separately is what makes this a resume-identity check rather than
    // a self-consistency check the TCB would always pass.
    if tcb.asid != Some(expected) || tag.asid != expected {
        tcb.async_preempted = None;
        return AsyncResumeClass::Refused("identity_mismatch");
    }
    if !tag.matches_tcb(tcb) {
        tcb.async_preempted = None;
        return AsyncResumeClass::Refused("stale_generation");
    }
    if tcb.user_context.instruction_ptr.0 == 0 {
        tcb.async_preempted = None;
        return AsyncResumeClass::Refused("no_saved_context");
    }
    if tcb.pending_syscall_completion.is_some() {
        tcb.async_preempted = None;
        return AsyncResumeClass::Refused("continuation_coexists");
    }
    tcb.async_preempted = None;
    AsyncResumeClass::AsyncPreempted
}

/// Canonical 199E-R2 — CANCEL a staged snapshot without consuming it as an authorization.
///
/// A snapshot is staged at trap entry, before the scheduler has decided anything. When the trap
/// then returns to the SAME task through the original hardware frame, no register write-back
/// happens at all and the snapshot describes an instant the task has already run past. Leaving
/// the tag in place would let a much later resume restore that stale register file, so the
/// boundary cancels it exactly there.
///
/// Returns `true` when a tag was actually cleared, so the caller can emit a bounded live marker
/// and so repeated no-switch ticks are observably not accumulating snapshots.
pub(crate) fn cancel_async_resume(tcbs: &mut [Option<ThreadControlBlock>], tid: u64) -> bool {
    tcbs.iter_mut()
        .flatten()
        .find(|tcb| tcb.tid.0 == tid)
        .is_some_and(|tcb| tcb.async_preempted.take().is_some())
}

impl BlockedSyscallCompletion {
    /// U6 §2 — the SINGLE source of truth for "is this parked completion still exactly this
    /// task's, for the cycle it was published for?".
    ///
    /// Three coordinates must all agree, and the third is CLASS-DISPATCHED: a completion
    /// published for a blocked send is validated against `blocked_send_generation`, one
    /// published for a blocked receive against `blocked_recv_generation`. Comparing a send
    /// completion against the receive counter (or vice versa) is exactly the conflation the
    /// separate counters exist to prevent, so the choice is made here rather than at each
    /// consumer.
    #[must_use]
    pub fn matches_tcb(&self, tcb: &ThreadControlBlock) -> bool {
        let live_generation = match self.syscall_class {
            BlockedSyscallClass::IpcRecv => tcb.blocked_recv_generation,
            BlockedSyscallClass::IpcSend => tcb.blocked_send_generation,
        };
        self.tid == tcb.tid.0
            && tcb.asid == Some(self.asid)
            && live_generation == self.blocked_generation
    }
}

impl ThreadControlBlock {
    /// Stage 200C2C2C-R2A — the CANONICAL publication of a completed blocked syscall's user-visible
    /// return registers into this task's saved continuation (RISC-V).
    ///
    /// ## Why two stores exist
    ///
    /// [`UserRegisterContext`] keeps two mirrors of the same userspace state: `user_gprs` (the raw
    /// register file) and `arg0..arg5` (the decoded syscall-argument lanes). They are populated
    /// together by `TrapFrame::capture_user_context` (frame → TCB) and restored together by
    /// `TrapFrame::apply_user_context` (TCB → frame), so on entry they agree. They are NOT
    /// redundant: the argument lanes are what the RISC-V syscall ABI import/restore path treats as
    /// authoritative for a resumed continuation, which is why publishing a completed result into
    /// only ONE of them lets the resume reconstruct `a0` from the other, stale mirror. That is
    /// exactly the proven defect: a timed-out receive published `a0 = 9` into the register mirror
    /// while the argument mirror still held the original endpoint capability, and userspace
    /// observed `a0 = 65540` — its own stale argument — instead of the result.
    ///
    /// This helper owns that mirror synchronization so no completion path has to know about it.
    /// It performs no user-memory copy, takes no lock, and must be called BEFORE the task is
    /// enqueued (the result must already be visible when the scheduler can dispatch it).
    ///
    /// `error` is the canonical `SyscallError` code (`0` = success). Only the RISC-V result lanes
    /// (`a0`/`a1` and their argument mirrors) are touched; every other saved register, `sepc`,
    /// `sstatus`, `satp`, `sp` and `tp` are left exactly as the continuation saved them.
    #[cfg(target_arch = "riscv64")]
    pub fn publish_riscv_user_return(&mut self, ret0: usize, ret1: usize, error: usize) {
        // RISC-V syscall ABI: a0 carries the error code when non-zero, otherwise ret0; a1 carries
        // ret1. `a0` is `user_gprs[10]`, `a1` is `user_gprs[11]`.
        let a0 = if error != 0 { error } else { ret0 };
        let a1 = if error != 0 { 0 } else { ret1 };
        self.user_context.user_gprs[10] = a0;
        self.user_context.user_gprs[11] = a1;
        // The argument-lane mirror MUST be published in the same operation, or the resume can
        // reconstruct a stale `a0` from it (the proven defect above).
        self.user_context.arg0 = a0;
        self.user_context.arg1 = a1;
    }

    /// Hosted mirror of [`Self::publish_riscv_user_return`] so the publication contract is
    /// testable on any host. Same lane semantics; compiled when not targeting RISC-V.
    #[cfg(not(target_arch = "riscv64"))]
    pub fn publish_riscv_user_return(&mut self, ret0: usize, ret1: usize, error: usize) {
        let a0 = if error != 0 { error } else { ret0 };
        let a1 = if error != 0 { 0 } else { ret1 };
        self.user_context.user_gprs[10] = a0;
        self.user_context.user_gprs[11] = a1;
        self.user_context.arg0 = a0;
        self.user_context.arg1 = a1;
    }

    pub fn new(tid: ThreadId, asid: Option<Asid>) -> Self {
        Self {
            tid,
            thread_group_id: ThreadGroupId(tid.0),
            status: TaskStatus::Runnable,
            asid,
            tls_ptr: None,
            user_entry: None,
            user_stack_top: None,
            user_context: UserRegisterContext::default(),
            detach_state: ThreadDetachState::Joinable,
            fault_policy_override: None,
            restart: RestartState::default(),
            kernel_context: KernelExecutionContext::default(),
            cpu_affinity: None,
            ipc_timeout_deadline: None,
            ipc_timeout_fired: false,
            blocked_recv_state: None,
            reply_timeout_token: None,
            reply_timeout_clock: crate::kernel::deadline_token::ReplyDeadlineClock::ProductionTick,
            server_reply_link: None,
            blocked_recv_generation: 0,
            blocked_send_generation: 0,
            pending_syscall_completion: None,
            async_preempted: None,
            async_preempt_generation: 0,
            spawn_reservation: None,
        }
    }

    /// Stage 199D-WA3B: a NON-LIVE spawn reservation.
    ///
    /// Deliberately a separate constructor from [`Self::new`]: ordinary registration must not
    /// silently acquire spawn-reservation semantics, and a reservation must not silently be an
    /// ordinary live task. The only difference is the status and the reservation record — every
    /// other field is the same default, so the pre-spawn provisioning bootstrap needs
    /// (CNode/process association, class, kernel stack and kernel context) works unchanged.
    pub fn reserved(tid: ThreadId, reservation: SpawnReservation) -> Self {
        let mut tcb = Self::new(tid, None);
        tcb.status = TaskStatus::Reserved;
        // Stage 199D-WA3B PROCESS IDENTITY: `task_cnode` resolves a task's CNode through
        // `thread_group_id`, so the reserved TCB must carry the OWNING PROCESS identity from the
        // moment it exists — otherwise the pre-spawn capability grants this whole protocol exists
        // to preserve would resolve to the wrong CNode. Storing `process_pid` in the reservation
        // record alone would leave the two inconsistent.
        tcb.thread_group_id = ThreadGroupId(reservation.process_pid);
        tcb.spawn_reservation = Some(reservation);
        tcb
    }

    /// Stage 199D-WA3B: is this TCB a non-live spawn reservation?
    ///
    /// The single predicate every scheduler / wake / waiter path uses, so "reserved" cannot come
    /// to mean two different things in two different places.
    pub fn is_spawn_reservation(&self) -> bool {
        matches!(self.status, TaskStatus::Reserved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_variants_construct() {
        let _ = TaskStatus::Runnable;
        let _ = TaskStatus::Running;
        let _ = TaskStatus::Blocked(WaitReason::Poll);
        let _ = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000)));
        let _ = TaskStatus::Blocked(WaitReason::Join(ThreadId(7)));
        let _ = TaskStatus::Faulted;
        let _ = TaskStatus::Exited(0);
        let _ = TaskStatus::Dead;
    }

    #[test]
    fn tcb_constructor_uses_typed_fields() {
        let mut tcb = ThreadControlBlock::new(ThreadId(7), Some(Asid(1)));
        tcb.tls_ptr = Some(VirtAddr(0xDEAD_BEEF));
        tcb.user_entry = Some(VirtAddr(0x4000));
        tcb.user_stack_top = Some(VirtAddr(0x8000));
        tcb.user_context = UserRegisterContext {
            instruction_ptr: VirtAddr(0x4000),
            stack_ptr: VirtAddr(0x8000),
            user_gprs: [0; 32],
            arg0: 1,
            arg1: 2,
            arg2: 3,
            arg3: 4,
            arg4: 5,
            arg5: 6,
        };
        tcb.fault_policy_override = Some(FaultPolicy::KillTask);
        tcb.restart = RestartState {
            token: Some(RestartToken(9)),
        };
        tcb.kernel_context.stack_base = Some(VirtAddr(0x9000));
        tcb.kernel_context.stack_top = Some(VirtAddr(0xA000));
        tcb.kernel_context.frame.set_stack_ptr(0x9FF0);
        tcb.kernel_context.frame.set_instruction_ptr(0x1234);
        tcb.kernel_context.initialized = true;
        tcb.kernel_context.owns_stack = true;

        assert_eq!(tcb.tid, ThreadId(7));
        assert_eq!(tcb.restart.token, Some(RestartToken(9)));
        assert_eq!(tcb.thread_group_id, ThreadGroupId(7));
        assert_eq!(tcb.tls_ptr, Some(VirtAddr(0xDEAD_BEEF)));
        assert_eq!(tcb.user_context.instruction_ptr, VirtAddr(0x4000));
        assert_eq!(tcb.detach_state, ThreadDetachState::Joinable);
        assert_eq!(tcb.status, TaskStatus::Runnable);
        assert_eq!(tcb.kernel_context.stack_top, Some(VirtAddr(0xA000)));
        assert_eq!(tcb.kernel_context.frame.stack_ptr(), 0x9FF0);
        assert_eq!(tcb.kernel_context.frame.instruction_ptr(), 0x1234);
        assert!(tcb.kernel_context.initialized);
        assert!(tcb.kernel_context.owns_stack);
    }

    #[test]
    fn tcb_constructor_preserves_large_tid_for_thread_group() {
        let tid = ThreadId(70_000);
        let tcb = ThreadControlBlock::new(tid, None);

        assert_eq!(tcb.thread_group_id, ThreadGroupId(70_000));
    }
}
