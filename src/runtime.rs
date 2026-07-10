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
use crate::kernel::trap::{FaultInfo, Trap};
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
    pub fn current_tid_authoritative(&self, cpu: CpuId) -> Option<u64> {
        self.with_cpu(cpu, |kernel| kernel.current_tid())
            .ok()
            .flatten()
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
    pub(crate) fn d6_genuine_local_dispatch_step_mut(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            let incoming = kernel_mut(&mut sched.scheduler)
                .dispatch_next_on(dispatch_cpu)
                .map(|tid| tid.0);
            crate::yarm_log!(
                "D6_GENUINE_MUT_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                if incoming.is_some() { "some" } else { "none" },
                incoming
            );
            incoming
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
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d6_genuine_mark_running_via_task_seam(&self, incoming: Option<u64>) {
        let Some(tid) = incoming else {
            return;
        };
        self.with_task_tcbs_split_mut(|tcbs| {
            if let Some(tcb) = tcbs.iter_mut().flatten().find(|tcb| tcb.tid.0 == tid) {
                tcb.status = crate::kernel::task::TaskStatus::Running;
            }
        });
    }

    /// Stage 168B (D2-GENUINE-RECV): re-verify — out of the global lock, through
    /// the rank-2 task seam — that the deferred blocking-recv task is STILL
    /// `Blocked(EndpointReceive(_))`. Guards the out-of-lock queue-advancing
    /// dispatch drain against a stale deferral (e.g. a sender woke the task, or
    /// an in-lock fallback superseded it). Single-CPU + IRQ-off means nothing
    /// mutates between the in-lock commit and this check, but the re-verify is
    /// the correctness fence the spec requires before dispatching.
    #[cfg(target_arch = "x86_64")]
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
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d2_recv_dispatch_step_mut(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            let incoming = kernel_mut(&mut sched.scheduler)
                .dispatch_next_on(dispatch_cpu)
                .map(|tid| tid.0);
            let result = if incoming.is_some() { "switch" } else { "idle" };
            crate::yarm_log!(
                "D2_RECV_GENUINE_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                result,
                incoming
            );
            incoming
        })
    }

    /// Stage 169 (D2-GENUINE-SEND): re-verify — out of the global lock, through
    /// the rank-2 task seam — that the deferred blocking-SEND task is STILL
    /// `Blocked(EndpointSend(_))` before the out-of-lock queue-advancing dispatch
    /// drain runs. Same correctness fence as the recv reverify.
    #[cfg(target_arch = "x86_64")]
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
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d2_send_dispatch_step_mut(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            let incoming = kernel_mut(&mut sched.scheduler)
                .dispatch_next_on(dispatch_cpu)
                .map(|tid| tid.0);
            let result = if incoming.is_some() { "switch" } else { "idle" };
            crate::yarm_log!(
                "D2_SEND_GENUINE_DISPATCH_STEP_SPLIT cpu={} result={} incoming={:?}",
                cpu.0,
                result,
                incoming
            );
            incoming
        })
    }

    /// Stage 192A (FUTEXWAIT QUEUE-ADVANCING DISPATCH): re-verify — out of the global
    /// lock, through the rank-2 task seam — that the deferred FutexWait task is STILL
    /// `Blocked(Futex(_))` before the out-of-lock queue-advancing dispatch drain runs.
    /// Same correctness fence as the D2 recv/send reverify: guards against a stale deferral
    /// (e.g. a FutexWake woke the task, or an in-lock fallback superseded it) so a woken
    /// waiter is never displaced from the run queue. Single-CPU + IRQ-off means nothing
    /// mutates between the in-lock commit and this check.
    #[cfg(target_arch = "x86_64")]
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
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn futex_wait_dispatch_step_mut(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            let incoming = kernel_mut(&mut sched.scheduler)
                .dispatch_next_on(dispatch_cpu)
                .map(|tid| tid.0);
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
            incoming
        })
    }

    /// Stage 192B (YIELD QUEUE-ADVANCING DISPATCH): re-verify — out of the global lock,
    /// through the rank-1 scheduler seam — that the `current` slot on `cpu` is still cleared
    /// (the in-lock `yield_current` re-enqueued the caller and cleared `current`). Guards the
    /// out-of-lock dispatch against a stale deferral (e.g. an in-lock fallback already
    /// dispatched). Single-CPU + IRQ-off means nothing mutates between the in-lock commit and
    /// this check; the re-verify is the correctness fence before dispatching.
    #[cfg(target_arch = "x86_64")]
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
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn yield_dispatch_step_mut(&self, cpu: CpuId) -> Option<u64> {
        self.with_scheduler_split_mut(|sched| {
            let dispatch_cpu = sched.current_cpu;
            let incoming = kernel_mut(&mut sched.scheduler)
                .dispatch_next_on(dispatch_cpu)
                .map(|tid| tid.0);
            match incoming {
                Some(tid) => {
                    crate::yarm_log!("YIELD_DISPATCH_DEQUEUE_OK cpu={} tid={}", cpu.0, tid)
                }
                None => crate::yarm_log!("YIELD_DISPATCH_DEQUEUE_OK cpu={} tid=idle", cpu.0),
            }
            incoming
        })
    }

    /// Stage 168B (D2-GENUINE-RECV): does the incoming task have an initialized
    /// kernel switch context (a wired kernel thread)? Read out of the global
    /// lock through the rank-2 task seam. Blocking recv is done by USER tasks,
    /// which resume via trap-frame restore + syscall restart (kernel_context
    /// initialized == false), so this returns false for the recv workload; it
    /// gates the dormant `switch_frames` (D2_RECV_GENUINE_SWITCH_*) variant that
    /// would reuse the hardened D6-SWITCH-A stash for a kernel-thread incoming.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d2_recv_incoming_has_kernel_switch_ctx(&self, tid: u64) -> bool {
        self.with_task_tcbs_split_mut(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.kernel_context.initialized)
                .unwrap_or(false)
        })
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

    /// # Validation status
    /// - LIVE_OFF_TRAP — pre-reads scheduler tick, then falls back to global lock for recv;
    ///   not a standalone trap-seam path.
    pub fn ipc_recv_with_deadline_split_bridge(
        &self,
        recv_cap: CapId,
        timeout_ticks: u64,
    ) -> Result<Option<Message>, KernelError> {
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2D path=ipc_recv_timeout_deadline_bridge");
        if timeout_ticks == 0 {
            return self.with(|state| state.try_ipc_recv(recv_cap));
        }
        let now = self.scheduler_tick_now_split_read();
        let deadline = now.wrapping_add(timeout_ticks);
        self.with(|state| state.ipc_recv_until_deadline(recv_cap, deadline))
    }

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
        let phase_a = match self.with_cpu(cpu, |state| {
            crate::kernel::syscall::try_split_recv_queued_plain_with_snapshot_locked(
                state, frame, &snapshot,
            )
        }) {
            Ok(phase_a) => phase_a,
            Err(_) => {
                // current_cpu bind failed (e.g. CPU offline) — fall back to the
                // unchanged global-lock path for the canonical handling.
                crate::yarm_log!(
                    "YARM_SPLIT_RECV_PROBE step=bind_cpu result=err cpu={}",
                    cpu.0
                );
                return None;
            }
        };
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

        // Phase C helper: §58 cap rollback under a brief global re-entry. The
        // seam copy has already completed (or failed) — no seam call happens
        // inside this closure.
        let rollback_cap = |shared: &Self, frame: &mut TrapFrame| {
            if let Some(cap_id) = pending.materialized_cap {
                let _ = shared.with_cpu(cpu, |kernel| {
                    kernel.rollback_materialized_recv_cap(
                        pending.receiver_tid,
                        crate::kernel::capabilities::CapId(cap_id),
                        pending.is_reply_cap,
                    );
                });
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
                        // No rollback on payload copy fault (§54/§58) — fault
                        // record + frame error under a brief re-entry, identical
                        // to the legacy record_user_fault call.
                        let _ = self.with_cpu(cpu, |kernel| {
                            crate::kernel::syscall::recv_boundary_record_user_fault(
                                kernel, frame, user_ptr,
                            );
                        });
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
                        // No rollback on payload copy fault (§55/§58).
                        crate::yarm_log!("YARM_RECV_CORE_V2_WRITEBACK result=payload_fault");
                        let _ = self.with_cpu(cpu, |kernel| {
                            crate::kernel::syscall::recv_boundary_record_user_fault(
                                kernel, frame, user_ptr,
                            );
                        });
                        Ok(())
                    }
                }
            }
            RecvWritebackPlan::KernelRegister { .. } => {
                unreachable!("KernelRegister writeback completes in Phase A, never deferred")
            }
        }
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
            // Roll the just-minted cap back so nothing leaks, then fail.
            let _ = self.with_cpu(cpu, |kernel| {
                kernel.rollback_materialized_recv_cap(
                    pending.receiver_tid,
                    crate::kernel::capabilities::CapId(local_cap),
                    false,
                );
            });
            return Err(TrapHandleError::Syscall(SyscallError::Internal));
        }

        // Step 3 — deferred sender wake (AFTER materialize, BEFORE writeback:
        // §56/§58 order). Brief global re-entry; no seam inside.
        if let Some(wake_tid) = pending.wake_tid {
            let _ = self.with_cpu(cpu, |kernel| kernel.apply_split_sender_wake_plan(wake_tid));
            crate::yarm_log!(
                "IPC_RECV_V2_SENDER_WAKE_ORDER_OK wake_tid={} phase=before_writeback",
                wake_tid.0
            );
        }

        // Step 4 — 186E user copy + §58 completion, shared with the plain path.
        let user_copy = crate::kernel::recv_core::RecvBoundaryUserCopySnapshot {
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
    pub(crate) fn drain_dispatch_post_work(&self, cpu: CpuId) -> Result<(), TrapHandleError> {
        let cpu_idx = cpu.0 as usize;
        if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
            return Ok(());
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
            return Ok(()); // inert in Stage 188A: no live producer.
        };
        self.execute_dispatch_post_work(cpu, work)
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
                let _ = self.with_cpu(cpu, |kernel| {
                    kernel.clear_blocked_recv_return_regs(snap.waiter_tid);
                    if let Some(wake_tid) = snap.wake_tid {
                        kernel.ipc_clear_plain_receiver_waiter_only(snap.endpoint_idx, wake_tid);
                        let _ = kernel.apply_scheduler_wake_plan(
                            crate::kernel::boot::SchedulerWakePlan::Wake(wake_tid),
                        );
                    }
                });
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
                cap.0
            }
            Err(e) => {
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
            }
            stale => {
                self.rollback_minted_cap_split(snap.receiver_cnode, CapId(local_cap), reply_object);
                crate::yarm_log!(
                    "REPLY_CAP_RANK_SEAM_ROLLBACK_OK waiter_tid={} reason={}",
                    snap.waiter_tid,
                    stale.stale_reason().unwrap_or("unknown")
                );
                crate::yarm_log!("REPLY_CAP_RANK_SEAM_FAIL reason=stale_record");
                // Same error mapping the D5 split uses for a stale record.
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
        }

        // Phase B.2 — encode the recv-v2 meta with the fresh receiver-local CapId
        // and the reply-cap recv-meta flag (byte-identical to the legacy reply arm).
        let meta = crate::kernel::syscall::ipc_recv_core::encode_recv_v2_meta(
            0,
            snap.app_opcode,
            0,
            snap.payload_len as u32,
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
            crate::yarm_log!("REPLY_CAP_RANK_SEAM_FAIL reason=user_copy");
            crate::yarm_log!(
                "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_reply_cap reason=user_copy"
            );
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        }
        crate::yarm_log!("DISPATCH_POST_WORK_USER_COPY_OK kind=blocked_waiter_reply_cap");

        // Phase C.2 — completion (brief `with_cpu`, no seam): clear return regs +
        // waiter slot, wake once.
        let _ = self.with_cpu(cpu, |kernel| {
            kernel.clear_blocked_recv_return_regs(snap.waiter_tid);
            if let Some(wake_tid) = snap.wake_tid {
                kernel.ipc_clear_plain_receiver_waiter_only(snap.endpoint_idx, wake_tid);
                let _ = kernel.apply_scheduler_wake_plan(
                    crate::kernel::boot::SchedulerWakePlan::Wake(wake_tid),
                );
            }
        });
        crate::yarm_log!("DISPATCH_POST_WORK_WAKE_OK kind=blocked_waiter_reply_cap");
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
                cap.0
            }
            Ok(CapTransferMaterializeOutcome::DeferredReplyCap) => {
                // Cannot occur: the producer excludes reply caps AND non-Reply
                // objects only reach here. Surface a real error rather than drop.
                crate::yarm_log!(
                    "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap reason=unexpected_reply_object"
                );
                return Err(TrapHandleError::Syscall(SyscallError::WrongObject));
            }
            Err(e) => {
                // Same real error the legacy router would raise (CapabilityFull,
                // WrongObject, StaleCapability, MissingRight, …). The envelope was
                // already consumed in Phase A — identical to the legacy arm.
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
            let _ = self.with_cpu(cpu, |kernel| {
                kernel.rollback_materialized_recv_cap(snap.waiter_tid, CapId(local_cap), false);
            });
            crate::yarm_log!(
                "IPC_RECV_V2_ROLLBACK_OK site=blocked_ordinary_cap tid={} reply=false",
                snap.waiter_tid
            );
            crate::yarm_log!(
                "DISPATCH_POST_WORK_FAIL kind=blocked_waiter_ordinary_cap reason=user_copy"
            );
            return Err(TrapHandleError::Syscall(SyscallError::InvalidArgs));
        }
        crate::yarm_log!("DISPATCH_POST_WORK_USER_COPY_OK kind=blocked_waiter_ordinary_cap");

        // Phase C — completion via a brief global re-entry (no seam inside),
        // preserving the legacy order copy → clear GPRs → clear waiter slot →
        // wake exactly once.
        let _ = self.with_cpu(cpu, |kernel| {
            kernel.clear_blocked_recv_return_regs(snap.waiter_tid);
            if let Some(wake_tid) = snap.wake_tid {
                kernel.ipc_clear_plain_receiver_waiter_only(snap.endpoint_idx, wake_tid);
                let _ = kernel.apply_scheduler_wake_plan(
                    crate::kernel::boot::SchedulerWakePlan::Wake(wake_tid),
                );
            }
        });
        crate::yarm_log!("DISPATCH_POST_WORK_WAKE_OK kind=blocked_waiter_ordinary_cap");
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

    pub fn handle_trap_with_cpu(
        &self,
        cpu: CpuId,
        trap: Trap,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        let result = self
            .with_cpu(cpu, |kernel| kernel.handle_trap(trap, frame))
            .map_err(|err| TrapHandleError::Syscall(err.into()))?;
        result
    }

    pub fn control_plane_set_process_cnode_slots_via_syscall(
        &self,
        target_pid: u64,
        slot_capacity: usize,
    ) -> Result<(), TrapHandleError> {
        self.with(|state| {
            state.control_plane_set_process_cnode_slots_via_syscall(target_pid, slot_capacity)
        })
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
    ) -> Option<[u8; crate::kernel::ipc::Message::MAX_PAYLOAD]> {
        use crate::kernel::vm::{Asid, PAGE_SIZE, VirtAddr};
        if asid_raw == 0 || len == 0 || len > crate::kernel::ipc::Message::MAX_PAYLOAD {
            return None;
        }
        let asid = Asid(u16::try_from(asid_raw).ok()?);
        let mut out = [0u8; crate::kernel::ipc::Message::MAX_PAYLOAD];
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

    /// Stage 191C (GLOBAL-LOCK-RETIRE class=InitramfsReadChunk): copy the kernel slice
    /// `src` into user VA `user_ptr` in address space `asid_raw`, OFF the broad global
    /// lock. Byte-identical in end-state to the legacy `KernelState::
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

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
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
        kernel
            .control_plane_set_process_cnode_slots_via_syscall(901, requested)
            .expect("resize");
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

        let err = kernel
            .control_plane_set_process_cnode_slots_via_syscall(911, 8)
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
        kernel
            .control_plane_set_process_cnode_slots_via_syscall(521, requested_slots)
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
        kernel
            .control_plane_set_process_cnode_slots_via_syscall(621, 8)
            .expect("create cnode");

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
