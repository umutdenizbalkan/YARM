// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{
    BootConfigSubsystem, ControlPlaneCnodePlan, FaultSubsystem, KernelCapacityProfile, KernelError,
    KernelState, KernelStorage, RuntimeCapacityConfig, SchedulerState, TelemetrySubsystem,
    TrapHandleError,
    kernel_mut, kernel_ref,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights};
use crate::kernel::ipc::Message;
use crate::kernel::task::{FaultPolicy, TaskClass};
#[cfg(any(debug_assertions, test))]
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use crate::kernel::lock::SpinLockGuard;
use crate::kernel::lock::{SpinLock, SpinLockIrq};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trap::{FaultInfo, Trap};
use crate::kernel::trapframe::TrapFrame;

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
    scheduler_state: *const SpinLockIrq<SchedulerState>,
    boot_config_state_lock: *const SpinLockIrq<()>,
    boot_config: *const KernelStorage<BootConfigSubsystem>,
}

impl SharedKernel {
    pub fn new(state: KernelState) -> Self {
        let scheduler_state = state.scheduler_state_lock_ptr();
        let (boot_config_state_lock, boot_config) = state.boot_config_split_read_ptrs();
        Self {
            state: SpinLock::new(state),
            scheduler_state,
            boot_config_state_lock,
            boot_config,
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
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
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
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
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
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).online_cpu_count()
    }

    /// # Validation status: UNIT_ONLY — staged read helper, not on the trap path.
    pub fn present_cpu_count_split_read(&self) -> usize {
        // Phase L7A split: read scheduler topology through scheduler_state only.
        // This is a read-only staged helper; it does not acquire the global
        // SharedKernel lock, mutate runqueues, or update current_cpu.
        // SAFETY: `scheduler_state` points at the scheduler lock embedded in the
        // same `KernelState` owned by `self.state`; the storage is stable for
        // the `SharedKernel` lifetime.
        let scheduler_state = unsafe { &*self.scheduler_state };
        let sched = scheduler_state.lock();
        kernel_ref(&sched.scheduler).present_cpu_count()
    }

    /// # Validation status: UNIT_ONLY — immutable boot-config read, not on the trap path.
    pub fn capacity_profile_split_read(&self) -> KernelCapacityProfile {
        // Phase L8B split: read immutable boot configuration under only the
        // boot_config lock domain. This intentionally avoids the global
        // SharedKernel lock and does not mutate boot config or runtime state.
        // SAFETY: these pointers refer to the boot_config lock and storage
        // embedded in the same `KernelState` owned by `self.state`; that storage
        // is stable for the `SharedKernel` lifetime.
        let boot_config_state_lock = unsafe { &*self.boot_config_state_lock };
        let _guard = boot_config_state_lock.lock();
        let boot_config = unsafe { &*self.boot_config };
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
    /// - HELPER_ONLY — Stage 31 queued-plain IPC recv fast-path attempt. NOT wired
    ///   into the live trap seam (`try_split_dispatch_into_frame`); exercised by unit
    ///   tests only. See `doc/KERNEL_LOCKING.md` §49 for the blocker.
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
        // Authoritative requester-TID read (binds current_cpu, then releases).
        // Mirrors the Stage 29A trap-seam discipline: never current_tid_split_read.
        let requester_tid = self.current_tid_authoritative(cpu)?;

        // Stage 32: resolve the endpoint receive cap via the phase-separated
        // split-read (task(2) read+release → capability(4) read+release), with
        // NO ipc lock and NO global lock held. A resolution failure is a real
        // error the old path returned — surface it (Some(Err)); the caller must
        // NOT fall back, since the global path produces the identical error.
        let recv_cap = CapId(frame.arg(crate::kernel::syscall::SYSCALL_ARG_CAP) as u64);
        let snapshot = match self.resolve_endpoint_recv_cap_split_read(requester_tid, recv_cap) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return Some(Err(TrapHandleError::Syscall(
                    crate::kernel::syscall::SyscallError::from(e),
                )));
            }
        };

        // Stage 32: the cap lock is RELEASED; only now acquire the IPC domain
        // (via the global `with` for this helper-only path) for the dequeue +
        // kernel-task writeback. The snapshot's endpoint object is revalidated
        // for liveness under ipc_state_lock inside the dequeue. The capability
        // lock and the IPC lock are NEVER held simultaneously.
        self.with(|state| {
            crate::kernel::syscall::try_split_recv_queued_plain_with_snapshot_locked(
                state, frame, &snapshot,
            )
        })
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
        unsafe {
            KernelState::task_asid_for_tid_from_raw(self.state.data_ptr() as *const _, tid)
        }
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
        FatalTrapReadSnapshot { current_tid, current_asid }
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
        unsafe {
            KernelState::cnode_slot_capacity_from_raw(self.state.data_ptr() as *const _, pid)
        }
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
        unsafe {
            KernelState::cnode_registered_from_raw(self.state.data_ptr() as *const _, pid)
        }
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
        let requester_class = unsafe {
            KernelState::task_class_from_raw(state as *const _, requester_tid)
        }
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

        let fault = FaultInfo { addr: VirtAddr(0xDEAD_0000), access: FaultAccess::Write };
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
            (state.tlb_shootdown_count(), state.tlb_shootdown_timeout_count())
        });

        // Initial values match.
        assert_eq!(kernel.tlb_shootdown_count_split_read(), count0);
        assert_eq!(kernel.tlb_shootdown_timeout_count_split_read(), timeout0);

        // After mutations via split_mut, split_read sees the updated values.
        kernel.increment_tlb_shootdown_count_split_mut();
        kernel.add_tlb_shootdown_timeout_count_split_mut(5);

        assert_eq!(kernel.tlb_shootdown_count_split_read(), count0.wrapping_add(1));
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
        assert!(!task_switched, "task_switched must be false for same-task return");
    }

    #[test]
    fn current_tid_split_read_offline_cpu_returns_none() {
        // Proves that current_tid_split_read for an offline CPU returns None —
        // same as the former with_cpu path (validate_online_cpu fail → unwrap_or(None)).
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(7);

        let split = kernel.current_tid_split_read(offline_cpu);
        let conservative = kernel.with_cpu(offline_cpu, |k| k.current_tid()).unwrap_or(None);
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
        assert!(!task_switched, "task_switched must be false for same-task return");
    }

    #[test]
    fn with_cpu_entering_tid_offline_cpu_returns_none() {
        // Proves that with_cpu for an offline CPU returns Err, making
        // unwrap_or(None) give None — the same sentinel as current_tid_split_read.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let offline_cpu = CpuId(7);

        let entering_tid = kernel.with_cpu(offline_cpu, |k| k.current_tid()).unwrap_or(None);
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
        let asid_via_global = kernel.with(|state| state.task_asid(74).map(|a| a.0 as u64).unwrap_or(0));

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
        assert_eq!(kernel.task_class_split_read(502), Some(TaskClass::SystemServer));

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
        let absent_via_global =
            kernel.with(|state| state.task_class(511)).is_some();
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

        let present_via_global =
            kernel.with(|state| state.task_class(511)).is_some();
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
        assert_eq!(kernel.cnode_slot_capacity_split_read(521), Some(requested_slots));
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
            30,
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
}
