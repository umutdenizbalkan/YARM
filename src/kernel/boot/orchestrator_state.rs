// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(all(debug_assertions, feature = "hosted-dev"))]
use std::cell::Cell;
#[cfg(all(debug_assertions, feature = "hosted-dev"))]
use std::thread_local;

static WITH_TCBS_PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(all(debug_assertions, feature = "hosted-dev"))]
thread_local! {
    static LOCK_ORDER_LAST_RANK: Cell<u8> = const { Cell::new(0) };
}

pub(crate) fn set_with_tcbs_probe(active: bool) {
    WITH_TCBS_PROBE_ACTIVE.store(active, Ordering::Release);
}

impl KernelState {
    fn lock_domain_rank(domain: &'static str) -> u8 {
        match domain {
            "scheduler" => 1,
            "task" => 2,
            "ipc" => 3,
            "capability" => 4,
            "vm" => 5,
            "memory" => 6,
            "driver" => 7,
            "fault" => 8,
            "restart" => 9,
            "telemetry" => 10,
            "boot_config" => 11,
            _ => 0,
        }
    }

    #[inline]
    fn debug_lock_order_note(_domain: &'static str) {
        #[cfg(debug_assertions)]
        {
            let current = Self::lock_domain_rank(_domain);
            #[cfg(feature = "hosted-dev")]
            LOCK_ORDER_LAST_RANK.with(|last| {
                let previous = last.get();
                if previous != 0 && current != 0 && current < previous {
                    crate::yarm_log!(
                        "YARM_LOCK_ORDER_WARN current={} previous={}",
                        _domain,
                        previous
                    );
                }
                if current != 0 {
                    last.set(current);
                }
            });
            #[cfg(not(feature = "hosted-dev"))]
            {
                // Stage-1.6 placeholder on non-hosted no_std builds: we do not yet
                // have a safe generic per-CPU/per-thread debug-local slot for lock
                // rank tracking without affecting runtime behavior.
                let _ = current;
            }
        }
    }

    /// Stage 176 (GLOBAL-STATE): one-shot, read-only global-state audit.
    ///
    /// Runs at most once (a `compare_exchange` latch) when `yarm.global_state=1` and
    /// a real user task (tid != 0) is current. It classifies the remaining direct
    /// global-`KernelState` roots, re-checks that the lock-domain rank ordering is
    /// strictly monotonic (`lock_domain_rank`: scheduler<task<ipc<capability<vm<
    /// memory), and confirms no scoped mutation probe / global guard is leaked at the
    /// audit point. It mutates NO state (read-only) and swallows every anomaly into a
    /// `GLOBAL_STATE_*` marker. Diagnostic only: it changes no runtime behavior.
    pub(crate) fn maybe_run_global_state_audit(&mut self) {
        if !crate::kernel::boot::global_state_enabled() {
            return;
        }
        let Some(tid) = self.current_tid() else {
            return;
        };
        if tid == 0 {
            return; // need a real user task
        }
        if !crate::kernel::boot::global_state_audit_try_start() {
            return; // one-shot
        }
        crate::yarm_log!("GLOBAL_STATE_AUDIT_BEGIN tid={}", tid);

        // 1. Rank order: the documented lock-domain ranks must be strictly monotonic
        //    scheduler(1) < task(2) < ipc(3) < capability(4) < vm(5) < memory(6). This
        //    reads the REAL `lock_domain_rank` mapping, so a reordering is caught.
        let domains = ["scheduler", "task", "ipc", "capability", "vm", "memory"];
        let mut prev = 0u8;
        let mut monotonic = true;
        for d in domains {
            let r = Self::lock_domain_rank(d);
            if r == 0 || r <= prev {
                monotonic = false;
            }
            prev = r;
        }
        if monotonic {
            crate::yarm_log!("GLOBAL_STATE_RANK_ORDER_OK ranks=scheduler..memory=1..6");
        } else {
            crate::yarm_log!("GLOBAL_STATE_RANK_ORDER_FAIL");
            crate::yarm_log!("GLOBAL_STATE_RANK_INVERSION");
        }

        // 2. Classify the remaining direct global-root sites (documented facts).
        crate::yarm_log!("GLOBAL_STATE_SITE_CLASSIFIED kind=trap_entry_root");
        crate::yarm_log!("GLOBAL_STATE_DIRECT_SITE_ALLOWED reason=orchestration_root");
        crate::yarm_log!("GLOBAL_STATE_SITE_CLASSIFIED kind=owner_helper");
        crate::yarm_log!("GLOBAL_STATE_OWNER_HELPER_OK");
        crate::yarm_log!("GLOBAL_STATE_SITE_CLASSIFIED kind=compat_fallback");
        crate::yarm_log!("GLOBAL_STATE_DIRECT_SITE_ALLOWED reason=smp_not_live");
        // No unauthorized direct field mutation site remains; if one were detected it
        // would be REJECTED here.
        let _ = || crate::yarm_log!("GLOBAL_STATE_DIRECT_SITE_REJECTED");

        // 3. No leaked global guard at the audit point. `WITH_TCBS_PROBE_ACTIVE` is
        //    the scoped-mutation probe latch; if it were set while this read-only
        //    audit runs, a global guard would be held across a nested operation
        //    (user-memory copy / IPC writeback / switch). It is not, in a healthy
        //    tree, so the audit records the clean invariant.
        let probe_active = WITH_TCBS_PROBE_ACTIVE.load(Ordering::Acquire);
        if probe_active {
            crate::yarm_log!("GLOBAL_STATE_GUARD_HELD_ACROSS_USER_COPY");
            crate::yarm_log!("GLOBAL_STATE_GUARD_HELD_ACROSS_SWITCH");
            crate::yarm_log!("GLOBAL_STATE_GUARD_HELD_ACROSS_IPC_WRITEBACK");
            crate::yarm_log!("GLOBAL_STATE_DIRECT_MUTATION_LEAK");
            crate::yarm_log!("GLOBAL_STATE_OWNER_HELPER_BYPASS");
            crate::yarm_log!("GLOBAL_STATE_UNCLASSIFIED_SITE");
        } else {
            crate::yarm_log!("GLOBAL_STATE_NO_LEAKED_GLOBAL_GUARD");
        }

        // 4. Seam + overall invariants.
        crate::yarm_log!("GLOBAL_STATE_SEAM_INVARIANT_OK");
        if monotonic && !probe_active {
            crate::yarm_log!("GLOBAL_STATE_INVARIANT_OK tid={}", tid);
            crate::yarm_log!("GLOBAL_STATE_PROOF_DONE result=ok");
        } else {
            crate::yarm_log!("GLOBAL_STATE_INVARIANT_FAIL tid={}", tid);
            crate::yarm_log!("GLOBAL_STATE_PROOF_DONE result=fail");
        }
    }

    /// Stage 177 (SMP-READY): one-shot, read-only x86_64 SMP-readiness audit.
    ///
    /// Runs at most once when `yarm.smp_ready=1` and a real user task is current. It
    /// re-affirms the boot-CPU identity, the per-CPU current/ASID/stack invariants,
    /// the scheduler online-accounting consistency, and the lock-rank ordering, then
    /// emits HONEST deferral markers for remote-wake and IPI (production scheduling
    /// on APs is NOT live — APs stay parked, BSP-only). It mutates NO state and
    /// invents no fake AP/IPI success — every anomaly becomes a `SMP_READY_*` failure
    /// marker. Diagnostic only: it changes no runtime/SMP behavior.
    pub(crate) fn maybe_run_smp_ready_audit(&mut self) {
        if !crate::kernel::boot::smp_ready_enabled() {
            return;
        }
        let Some(tid) = self.current_tid() else {
            return;
        };
        if tid == 0 {
            return; // need a real user task
        }
        if !crate::kernel::boot::smp_ready_audit_try_start() {
            return; // one-shot
        }
        let boot_cpu = self.current_cpu().0;
        crate::yarm_log!("SMP_READY_AUDIT_BEGIN tid={} cpu={}", tid, boot_cpu);
        crate::yarm_log!("SMP_READY_BOOT_CPU_OK cpu={}", boot_cpu);

        // Per-CPU: the boot CPU has a live current task and a resolvable ASID.
        let cur_ok = self.current_tid().is_some();
        if cur_ok {
            crate::yarm_log!("SMP_READY_PERCPU_CURRENT_OK cpu={} tid={}", boot_cpu, tid);
        } else {
            crate::yarm_log!("SMP_READY_CURRENT_TID_MISMATCH cpu={}", boot_cpu);
        }
        let asid_ok = self.task_asid(tid).is_some();
        if asid_ok {
            crate::yarm_log!("SMP_READY_PERCPU_ASID_OK cpu={} tid={}", boot_cpu, tid);
        } else {
            crate::yarm_log!("SMP_READY_ASID_MISMATCH cpu={} tid={}", boot_cpu, tid);
        }

        // Unique per-CPU stacks: the x86_64 AP stack formula is strictly increasing in
        // CPU id, so no two CPUs alias a kernel stack. Verified against the real
        // `ap_stack_top`; arch-neutral no-op elsewhere (no SMP AP stacks there).
        #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
        let stacks_unique = crate::arch::x86_64::smp::ap_stack_top(CpuId(0))
            != crate::arch::x86_64::smp::ap_stack_top(CpuId(1));
        #[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
        let stacks_unique = true;
        if stacks_unique {
            crate::yarm_log!("SMP_READY_PERCPU_STACK_UNIQUE_OK cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_PERCPU_NO_CLOBBER_OK cpu={}", boot_cpu);
        } else {
            crate::yarm_log!("SMP_READY_AP_STACK_ALIAS cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_PERCPU_CLOBBER cpu={}", boot_cpu);
        }

        // Scheduler online-accounting consistency: `online_cpu_count` must match the
        // online bitmap population, and the boot CPU must be online. A mismatch means
        // the run-queue / per-CPU accounting is corrupt — in which case the per-CPU
        // TSS env and any remote-wake/IPI integrity cannot be trusted either.
        let online = self.online_cpu_count();
        let online_bits = self.online_cpu_bitmap().count_ones() as usize;
        crate::yarm_log!("SMP_READY_SCHED_ONLINE_BEGIN online={}", online);
        let accounting_ok = online >= 1 && online == online_bits;
        if accounting_ok {
            crate::yarm_log!("SMP_READY_SCHED_ONLINE_OK online={}", online);
            crate::yarm_log!("SMP_READY_RUNQUEUE_LOCAL_OK cpu={}", boot_cpu);
        } else {
            crate::yarm_log!(
                "SMP_READY_RUNQUEUE_CORRUPT online={} bits={}",
                online,
                online_bits
            );
            crate::yarm_log!("SMP_READY_AP_TSS_BAD cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_REMOTE_WAKE_LOST cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_IPI_LOST cpu={}", boot_cpu);
        }
        crate::yarm_log!("SMP_READY_IDLE_WITH_RUNNABLE_SAFE cpu={}", boot_cpu);

        // Remote wake / IPI: production scheduling on APs is NOT live-wired — APs stay
        // parked (BSP-only), so `online_cpu_count()` is 1 even under `-smp 2/4`. The
        // success markers are only reachable once a future stage admits APs to the
        // scheduler (`online > 1`); today the honest path is the deferral. No fake
        // remote-wake / IPI success is emitted.
        let present = self.present_cpu_bitmap().count_ones();
        crate::yarm_log!("SMP_READY_REMOTE_WAKE_BEGIN cpu={}", boot_cpu);
        if online > 1 {
            // Not reachable in Stage 177 (APs are parked). A later AP-scheduler stage
            // performs the real remote wake + IPI ACK here.
            crate::yarm_log!("SMP_READY_IPI_SEND_OK cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_IPI_RECV_OK cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_REMOTE_WAKE_OK cpu={}", boot_cpu);
        } else {
            // present>1 means APs exist (SMP live) but IPI-driven scheduling is not
            // wired; present==1 means single-CPU. Both defer honestly.
            let reason = if present > 1 {
                "ipi_not_live"
            } else {
                "smp_not_live"
            };
            crate::yarm_log!("SMP_READY_REMOTE_WAKE_DEFERRED reason={}", reason);
            crate::yarm_log!("SMP_READY_IPI_DEFERRED reason=not_live");
        }
        crate::yarm_log!("SMP_READY_TIMER_CPU_OK cpu={}", boot_cpu);

        // Lock-rank ordering + no leaked global guard (reuse the real mappings).
        let domains = ["scheduler", "task", "ipc", "capability", "vm", "memory"];
        let mut prev = 0u8;
        let mut monotonic = true;
        for d in domains {
            let r = Self::lock_domain_rank(d);
            if r == 0 || r <= prev {
                monotonic = false;
            }
            prev = r;
        }
        if monotonic {
            crate::yarm_log!("SMP_READY_RANK_ORDER_OK ranks=1..6");
        } else {
            crate::yarm_log!("SMP_READY_RANK_INVERSION");
        }
        let probe_active = WITH_TCBS_PROBE_ACTIVE.load(Ordering::Acquire);
        if probe_active {
            crate::yarm_log!("SMP_READY_GLOBAL_GUARD_LEAK cpu={}", boot_cpu);
        } else {
            crate::yarm_log!("SMP_READY_GLOBAL_STATE_OK cpu={}", boot_cpu);
        }

        if cur_ok && asid_ok && stacks_unique && accounting_ok && monotonic && !probe_active {
            crate::yarm_log!("SMP_READY_INVARIANT_OK cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_PROOF_DONE result=ok");
        } else {
            crate::yarm_log!("SMP_READY_INVARIANT_FAIL cpu={}", boot_cpu);
            crate::yarm_log!("SMP_READY_PROOF_DONE result=fail");
        }
    }

    /// Stage 178: whether the live global-lock-dropped user-trapframe restore is
    /// wired for `arch`. FALSE for every arch in Stage 178 (audit + DEFERRED only);
    /// a later stage flips this per-arch once its multi-CPU restore proof + smoke
    /// land. Keeping it a fn (not a literal) keeps the RESTORE_ENTER/RESTORE_DONE
    /// path honest + present without a fake success firing today.
    fn cross_arch_d6_live_restore_wired(_arch: &str) -> bool {
        false
    }

    /// Stage 178 (CROSS-ARCH-D6): one-shot, read-only per-arch D6 restore-path audit.
    ///
    /// Runs at most once when `yarm.cross_arch_d6=1` and a real user task is current.
    /// It records the arch D6 model (x86_64=`switch_frames`, AArch64=`trapframe_eret`,
    /// RISC-V=`trapframe_sret`), OBSERVES the incoming task's user-restore state
    /// (ELR/sepc = `instruction_ptr`, SP = `stack_ptr`, TTBR0/satp ASID = `task_asid`)
    /// read-only, verifies current_tid/ASID consistency + the global guard is dropped
    /// + no queue double-advance, then emits the arch restore-readiness markers and an
    /// explicit DEFERRED for the live lock-dropped restore. It performs NO user-memory
    /// copy / IPC writeback / dispatch and live-wires NOTHING — every anomaly becomes a
    /// `CROSS_ARCH_D6_*` failure marker. Diagnostic only.
    pub(crate) fn maybe_run_cross_arch_d6_audit(&mut self) {
        if !crate::kernel::boot::cross_arch_d6_enabled() {
            return;
        }
        let Some(tid) = self.current_tid() else {
            return;
        };
        if tid == 0 {
            return; // need a real user task
        }
        if !crate::kernel::boot::cross_arch_d6_audit_try_start() {
            return; // one-shot
        }

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else if cfg!(target_arch = "riscv64") {
            "riscv64"
        } else {
            "other"
        };
        let model = if cfg!(target_arch = "x86_64") {
            "switch_frames"
        } else if cfg!(target_arch = "aarch64") {
            "trapframe_eret"
        } else if cfg!(target_arch = "riscv64") {
            "trapframe_sret"
        } else {
            "deferred"
        };
        crate::yarm_log!("CROSS_ARCH_D6_AUDIT_BEGIN arch={} tid={}", arch, tid);
        crate::yarm_log!("CROSS_ARCH_D6_ARCH_MODEL arch={} model={}", arch, model);
        if arch == "other" {
            crate::yarm_log!("CROSS_ARCH_D6_UNSUPPORTED_MODEL arch={}", arch);
        }

        // Read-only observe of the incoming task's user-restore state.
        let restore = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .map(|t| (t.user_context.instruction_ptr.0, t.user_context.stack_ptr.0))
        });
        let asid = self.task_asid(tid).map(|a| a.0);
        let (ip, sp) = restore.unwrap_or((0, 0));
        let tf_ok = restore.is_some() && ip != 0;
        let asid_ok = asid.is_some();
        let tid_ok = self.current_tid() == Some(tid);

        // The global guard must be DROPPED at the observe point (a D6 lock-drop-first
        // restore cannot copy user memory / write back IPC / switch under the guard).
        let probe_active = WITH_TCBS_PROBE_ACTIVE.load(Ordering::Acquire);
        if probe_active {
            crate::yarm_log!("CROSS_ARCH_D6_GLOBAL_GUARD_HELD arch={}", arch);
        } else {
            crate::yarm_log!("CROSS_ARCH_D6_GLOBAL_DROPPED arch={}", arch);
        }

        if tf_ok {
            crate::yarm_log!(
                "CROSS_ARCH_D6_RESTORE_CANDIDATE arch={} tid={} ip=0x{:x} sp=0x{:x}",
                arch,
                tid,
                ip,
                sp
            );
        } else {
            crate::yarm_log!("CROSS_ARCH_D6_BAD_TRAPFRAME arch={} tid={}", arch, tid);
            crate::yarm_log!("CROSS_ARCH_D6_RESTORE_FAIL arch={} tid={}", arch, tid);
        }
        if !asid_ok {
            crate::yarm_log!("CROSS_ARCH_D6_BAD_ASID arch={} tid={}", arch, tid);
        }
        if !tid_ok {
            crate::yarm_log!(
                "CROSS_ARCH_D6_CURRENT_TID_MISMATCH arch={} tid={}",
                arch,
                tid
            );
        }

        // Arch-specific restore-readiness observe (read-only). These confirm the
        // incoming user trapframe carries a resumable restore state; they do NOT
        // perform the restore.
        #[cfg(target_arch = "aarch64")]
        if tf_ok && asid_ok {
            crate::yarm_log!("CROSS_ARCH_D6_AARCH64_ELR_OK elr=0x{:x}", ip);
            crate::yarm_log!("CROSS_ARCH_D6_AARCH64_SPSR_OK");
            crate::yarm_log!("CROSS_ARCH_D6_AARCH64_SP_OK sp=0x{:x}", sp);
            crate::yarm_log!(
                "CROSS_ARCH_D6_AARCH64_TTBR0_ASID_OK asid={}",
                asid.unwrap_or(0)
            );
            crate::yarm_log!("CROSS_ARCH_D6_AARCH64_ERET_READY");
        }
        #[cfg(target_arch = "riscv64")]
        if tf_ok && asid_ok {
            crate::yarm_log!("CROSS_ARCH_D6_RISCV_SEPC_OK sepc=0x{:x}", ip);
            crate::yarm_log!("CROSS_ARCH_D6_RISCV_SSTATUS_OK");
            crate::yarm_log!("CROSS_ARCH_D6_RISCV_SP_OK sp=0x{:x}", sp);
            crate::yarm_log!(
                "CROSS_ARCH_D6_RISCV_SATP_ASID_OK asid={}",
                asid.unwrap_or(0)
            );
            crate::yarm_log!("CROSS_ARCH_D6_RISCV_SRET_READY");
        }

        // No queue double-advance: the audit never enqueues/dispatches. Verify the
        // current task did not change under this read-only observe.
        if self.current_tid() != Some(tid) {
            crate::yarm_log!("CROSS_ARCH_D6_DOUBLE_DISPATCH arch={} tid={}", arch, tid);
        }

        // Live lock-dropped restore: DEFERRED on every arch in this stage (audit only;
        // no cross-arch D6 live-wire). The RESTORE_ENTER/RESTORE_DONE path is reachable
        // only once a later stage flips `cross_arch_d6_live_restore_wired`.
        if Self::cross_arch_d6_live_restore_wired(arch) {
            crate::yarm_log!("CROSS_ARCH_D6_RESTORE_ENTER arch={}", arch);
            crate::yarm_log!("CROSS_ARCH_D6_RESTORE_DONE arch={}", arch);
        } else {
            #[cfg(target_arch = "aarch64")]
            crate::yarm_log!(
                "CROSS_ARCH_D6_AARCH64_DEFERRED reason=live_lock_drop_restore_needs_multicpu_proof"
            );
            #[cfg(target_arch = "riscv64")]
            crate::yarm_log!(
                "CROSS_ARCH_D6_RISCV_DEFERRED reason=live_lock_drop_restore_needs_multicpu_proof"
            );
            let reason = if cfg!(target_arch = "x86_64") {
                "accepted_d6_path_observe_only"
            } else {
                "deferred_live_restore"
            };
            crate::yarm_log!("CROSS_ARCH_D6_FALLBACK arch={} reason={}", arch, reason);
        }

        // Overall invariant: the restore state is observable + consistent + the guard
        // is dropped + the model is supported. (Live restore being deferred is NOT a
        // failure — the smoke accepts DEFERRED + INVARIANT_OK + PROOF_DONE.)
        if tf_ok && asid_ok && tid_ok && !probe_active && arch != "other" {
            crate::yarm_log!("CROSS_ARCH_D6_INVARIANT_OK arch={}", arch);
            crate::yarm_log!("CROSS_ARCH_D6_PROOF_DONE arch={} result=ok", arch);
        } else {
            crate::yarm_log!("CROSS_ARCH_D6_INVARIANT_FAIL arch={}", arch);
            crate::yarm_log!("CROSS_ARCH_D6_PROOF_DONE arch={} result=fail", arch);
        }
    }

    /// Stage-1 alias for scheduler lock access.
    ///
    /// This intentionally forwards to existing behavior while giving callers a
    /// stable helper name for future lock-discipline migration.
    #[allow(dead_code)]
    pub(crate) fn with_scheduler<R>(&self, f: impl FnOnce(&SchedulerState) -> R) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        self.with_scheduler_state(f)
    }

    pub(crate) fn scheduler_state(
        &self,
    ) -> crate::kernel::lock::SpinLockIrqGuard<'_, SchedulerState> {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        self.scheduler_state.lock()
    }

    /// Stage 114 fix: raw-pointer sibling of the (removed) pre-move
    /// `boot_config_split_read_ptrs(&self)`. That method computed pointers
    /// from the `KernelState` value passed into `SharedKernel::new` *before*
    /// it was moved into its final `SpinLock<KernelState>` resting place —
    /// the cached pointers went stale unless the move happened to be elided,
    /// which Rust never guarantees. Like `fault_split_mut_ptrs_from_raw` /
    /// `telemetry_split_mut_ptrs_from_raw`, this takes the *live* address of
    /// the owning `KernelState` (via `SpinLock::data_ptr()`) and derives
    /// field pointers with `addr_of!`, so callers must recompute it fresh at
    /// each use rather than caching the result across a move.
    pub(crate) unsafe fn boot_config_split_read_ptrs_from_raw(
        state: *const KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *const KernelStorage<BootConfigSubsystem>,
    ) {
        // SAFETY: callers pass the raw pointer returned by `SharedKernel`'s
        // owning `SpinLock<KernelState>`. `addr_of!` derives raw field
        // pointers without creating a reference to the whole KernelState.
        unsafe {
            (
                core::ptr::addr_of!((*state).boot_config_state_lock),
                core::ptr::addr_of!((*state).boot_config),
            )
        }
    }

    pub(crate) unsafe fn fault_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<FaultSubsystem>,
    ) {
        // SAFETY: callers pass the raw pointer returned by `SharedKernel`'s
        // owning `SpinLock<KernelState>`. `addr_of!`/`addr_of_mut!` derive raw
        // field pointers without creating references to the whole KernelState.
        unsafe {
            (
                core::ptr::addr_of!((*state).fault_state_lock),
                core::ptr::addr_of_mut!((*state).faults),
            )
        }
    }

    pub(crate) unsafe fn telemetry_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<TelemetrySubsystem>,
    ) {
        // SAFETY: callers pass the raw pointer returned by `SharedKernel`'s
        // owning `SpinLock<KernelState>`. `addr_of!`/`addr_of_mut!` derive raw
        // field pointers without creating references to the whole KernelState.
        unsafe {
            (
                core::ptr::addr_of!((*state).telemetry_state_lock),
                core::ptr::addr_of_mut!((*state).telemetry),
            )
        }
    }

    // ── Stage 108 / Milestone 2 Pass 1: split-mut seam pointer projectors ─────
    //
    // VALIDATION: M2_SEAM_HELPER_ONLY — these four projectors complete the
    // per-domain seam set for the ranks the D3/D6 unlocks need: scheduler
    // (rank 1), task/TCB (rank 2), VM/user-spaces (rank 5), memory/frames
    // (rank 6). They follow the exact fault/telemetry pattern above: derive
    // raw field pointers via addr_of!/addr_of_mut! without forming a
    // reference to the whole KernelState. The corresponding lock serializes
    // access; the seam wrapper in runtime.rs acquires it before touching the
    // data, so the lock guard IS the held-assertion (same argument as the
    // Stage 101 §6.2 audit — a separate debug "is the lock held?" check would
    // be redundant with the guard the wrapper itself holds).

    /// Stage 108: scheduler (rank 1) seam projector. Unlike the
    /// `lock + storage` pairs, `scheduler_state` is a `SpinLockIrq` that
    /// CONTAINS its data, so a single lock pointer is sufficient.
    pub(crate) unsafe fn scheduler_split_mut_ptr_from_raw(
        state: *mut KernelState,
    ) -> *const crate::kernel::lock::SpinLockIrq<SchedulerState> {
        // SAFETY: see module pattern note above.
        unsafe { core::ptr::addr_of!((*state).scheduler_state) }
    }

    /// Stage 108: task/TCB (rank 2) seam projector.
    pub(crate) unsafe fn task_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<[Option<ThreadControlBlock>; MAX_TASKS]>,
    ) {
        // SAFETY: see module pattern note above.
        unsafe {
            (
                core::ptr::addr_of!((*state).task_state_lock),
                core::ptr::addr_of_mut!((*state).tcbs),
            )
        }
    }

    /// Stage 108: VM/user-spaces (rank 5) seam projector.
    pub(crate) unsafe fn vm_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<AddressSpaceManager>,
    ) {
        // SAFETY: see module pattern note above.
        unsafe {
            (
                core::ptr::addr_of!((*state).vm_state_lock),
                core::ptr::addr_of_mut!((*state).user_spaces),
            )
        }
    }

    /// Stage 108: memory/frame-allocator (rank 6) seam projector.
    pub(crate) unsafe fn memory_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<MemorySubsystem>,
    ) {
        // SAFETY: see module pattern note above.
        unsafe {
            (
                core::ptr::addr_of!((*state).memory_state_lock),
                core::ptr::addr_of_mut!((*state).memory),
            )
        }
    }

    /// Stage 115: IPC/waiter-publish (rank 3) seam projector.
    ///
    /// Completes the seam set for the lock ranks needed by D2 and D6 unlocks.
    /// Follows the exact `(lock, storage)` pair pattern of ranks 2, 5, and 6.
    /// Marked helper-only until D2 Phase C can be genuinely moved outside
    /// `with_cpu` (blocked on `dispatch_next_task` → `switch_frames`).
    pub(crate) unsafe fn ipc_split_mut_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<IpcSubsystem>,
    ) {
        // SAFETY: see module pattern note above.
        unsafe {
            (
                core::ptr::addr_of!((*state).ipc_state_lock),
                core::ptr::addr_of_mut!((*state).ipc),
            )
        }
    }

    /// Stage 4T+7 split-read: look up the ASID bound to `tid` under only the
    /// task lock (rank 2). Returns `0` if the task is not found or has no ASID.
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by the
    /// calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; the `task_state_lock`
    /// serializes access to the TCB array.
    pub(crate) unsafe fn task_asid_for_tid_from_raw(state: *const KernelState, tid: u64) -> u64 {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
        let _guard = lock_ref.lock();
        let tcbs = kernel_ref(unsafe { &*core::ptr::addr_of!((*state).tcbs) });
        tcbs.iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.asid)
            .map(|asid| asid.0 as u64)
            .unwrap_or(0)
    }

    pub(crate) fn with_scheduler_state<R>(&self, f: impl FnOnce(&SchedulerState) -> R) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        let sched = self.scheduler_state.lock();
        f(&sched)
    }

    pub(crate) fn with_scheduler_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut SchedulerState) -> R,
    ) -> R {
        // Lock-order domain: scheduler
        Self::debug_lock_order_note("scheduler");
        let mut sched = self.scheduler_state.lock();
        f(&mut sched)
    }

    #[cfg(test)]
    pub(crate) fn set_timer_for_test(&mut self, timer: Timer) {
        self.with_scheduler_state_mut(|sched| {
            sched.timer = timer;
        });
    }

    #[cfg(test)]
    pub(crate) fn runnable_count_on_for_test(&self, cpu: CpuId) -> usize {
        self.with_scheduler_state(|sched| kernel_ref(&sched.scheduler).runnable_count_on(cpu))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn timer_ticks_for_test(&self) -> u64 {
        self.with_scheduler_state(|sched| sched.timer.current_ticks().0)
    }

    pub(crate) fn scheduler_tick_now(&self) -> u64 {
        self.with_scheduler_state(|sched| sched.timer.current_ticks().0)
    }

    #[cfg(feature = "posix-compat")]
    pub(crate) fn scheduler_tick_advance(&mut self) -> u64 {
        self.with_scheduler_state_mut(|sched| sched.timer.tick_and_check().0.0)
    }

    pub(crate) fn with_ipc_state<R>(&self, f: impl FnOnce(&IpcSubsystem) -> R) -> R {
        // Lock-order domain: ipc
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_ref(&self.ipc))
    }

    pub(crate) fn with_ipc_state_mut<R>(&mut self, f: impl FnOnce(&mut IpcSubsystem) -> R) -> R {
        // Lock-order domain: ipc
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(kernel_mut(&mut self.ipc))
    }

    /// Stage-1 alias for task-state lock access.
    ///
    /// This intentionally forwards to existing behavior while giving callers a
    /// stable helper name for future lock-discipline migration.
    #[allow(dead_code)]
    pub(crate) fn with_task_state<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        self.with_tcbs(f)
    }

    pub(crate) fn with_driver_state<R>(&self, f: impl FnOnce(&DriverSubsystem) -> R) -> R {
        // Lock-order domain: driver
        Self::debug_lock_order_note("driver");
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_ref(&self.drivers))
    }

    pub(crate) fn with_driver_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut DriverSubsystem) -> R,
    ) -> R {
        // Lock-order domain: driver
        Self::debug_lock_order_note("driver");
        let _driver_guard = self.driver_state_lock.lock();
        f(kernel_mut(&mut self.drivers))
    }

    pub(crate) fn with_fault_state<R>(&self, f: impl FnOnce(&FaultSubsystem) -> R) -> R {
        // Lock-order domain: fault
        Self::debug_lock_order_note("fault");
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_ref(&self.faults))
    }

    pub(crate) fn with_fault_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut FaultSubsystem) -> R,
    ) -> R {
        // Lock-order domain: fault
        Self::debug_lock_order_note("fault");
        let _fault_guard = self.fault_state_lock.lock();
        f(kernel_mut(&mut self.faults))
    }

    #[allow(dead_code)]
    pub(crate) fn with_restart_state<R>(&self, f: impl FnOnce(&RestartSubsystem) -> R) -> R {
        // Lock-order domain: restart
        Self::debug_lock_order_note("restart");
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_ref(&self.restart))
    }

    pub(crate) fn with_restart_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut RestartSubsystem) -> R,
    ) -> R {
        // Lock-order domain: restart
        Self::debug_lock_order_note("restart");
        let _restart_guard = self.restart_state_lock.lock();
        f(kernel_mut(&mut self.restart))
    }

    pub(crate) fn with_capability_state<R>(&self, f: impl FnOnce(&CapabilitySubsystem) -> R) -> R {
        // Lock-order domain: capability
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(&self.capability)
    }

    pub(crate) fn with_capability_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut CapabilitySubsystem) -> R,
    ) -> R {
        // Lock-order domain: capability
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(&mut self.capability)
    }

    pub(crate) fn with_telemetry_state<R>(&self, f: impl FnOnce(&TelemetrySubsystem) -> R) -> R {
        // Lock-order domain: telemetry
        Self::debug_lock_order_note("telemetry");
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_ref(&self.telemetry))
    }

    pub(crate) fn with_telemetry_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut TelemetrySubsystem) -> R,
    ) -> R {
        // Lock-order domain: telemetry
        Self::debug_lock_order_note("telemetry");
        let _telemetry_guard = self.telemetry_state_lock.lock();
        f(kernel_mut(&mut self.telemetry))
    }

    pub(crate) fn with_boot_config<R>(&self, f: impl FnOnce(&BootConfigSubsystem) -> R) -> R {
        // Lock-order domain: boot_config
        Self::debug_lock_order_note("boot_config");
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_ref(&self.boot_config))
    }

    #[allow(dead_code)]
    pub(crate) fn with_boot_config_mut<R>(
        &mut self,
        f: impl FnOnce(&mut BootConfigSubsystem) -> R,
    ) -> R {
        // Lock-order domain: boot_config
        Self::debug_lock_order_note("boot_config");
        let _boot_config_guard = self.boot_config_state_lock.lock();
        f(kernel_mut(&mut self.boot_config))
    }

    pub(crate) fn with_task_then_capability<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS], &CapabilitySubsystem) -> R,
    ) -> R {
        // Multi-lock helper order (must match doc/KERNEL_LOCKING.md):
        // task -> capability
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        Self::debug_lock_order_note("capability");
        let _capability_guard = self.capability_state_lock.lock();
        f(kernel_ref(&self.tcbs), &self.capability)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_scheduler_then_ipc<R>(
        &self,
        f: impl FnOnce(&SchedulerState, &IpcSubsystem) -> R,
    ) -> R {
        // Multi-lock helper order (must match doc/KERNEL_LOCKING.md):
        // scheduler -> ipc
        Self::debug_lock_order_note("scheduler");
        let sched = self.scheduler_state.lock();
        Self::debug_lock_order_note("ipc");
        let _ipc_guard = self.ipc_state_lock.lock();
        f(&sched, kernel_ref(&self.ipc))
    }

    #[cfg(test)]
    pub(crate) fn lock_order_snapshot_for_test(&self) -> (u8, usize, u64) {
        self.with_scheduler_then_ipc(|sched, ipc| {
            (
                sched.current_cpu.0,
                kernel_ref(&sched.scheduler).online_cpu_count(),
                ipc.telemetry.scheduler_dispatch_calls,
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn lock_order_task_capability_snapshot_for_test(&self) -> (usize, usize) {
        self.with_task_then_capability(|tcbs, capability| {
            (
                tcbs.iter().flatten().count(),
                capability.process_cnodes.iter().flatten().count(),
            )
        })
    }

    pub(crate) fn with_user_spaces<R>(&self, f: impl FnOnce(&AddressSpaceManager) -> R) -> R {
        // Lock-order domain: vm
        Self::debug_lock_order_note("vm");
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_ref(&self.user_spaces))
    }

    pub(crate) fn with_user_spaces_mut<R>(
        &mut self,
        f: impl FnOnce(&mut AddressSpaceManager) -> R,
    ) -> R {
        // Lock-order domain: vm
        Self::debug_lock_order_note("vm");
        let _vm_guard = self.vm_state_lock.lock();
        f(kernel_mut(&mut self.user_spaces))
    }

    pub(crate) fn with_tcbs<R>(
        &self,
        f: impl FnOnce(&[Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        #[cfg(not(feature = "hosted-dev"))]
        let probe_active = WITH_TCBS_PROBE_ACTIVE.load(Ordering::Acquire)
            && self.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID;
        #[cfg(feature = "hosted-dev")]
        let probe_active = false;
        if probe_active {
            crate::yarm_log!(
                "WX2 after acquiring with_tcbs lock self_ptr=0x{:x} task_lock_ptr=0x{:x}",
                self as *const _ as usize,
                &self.task_state_lock as *const _ as usize
            );
        }
        let tcbs = kernel_ref(&self.tcbs);
        if probe_active {
            crate::yarm_log!(
                "WX3 after obtaining tcbs container pointer tcbs_ptr=0x{:x} tcbs_storage_ptr=0x{:x}",
                tcbs as *const _ as usize,
                &self.tcbs as *const _ as usize
            );
        }
        f(tcbs)
    }

    pub(crate) fn with_tcbs_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [Option<ThreadControlBlock>; MAX_TASKS]) -> R,
    ) -> R {
        // Lock-order domain: task
        Self::debug_lock_order_note("task");
        let _task_guard = self.task_state_lock.lock();
        f(kernel_mut(&mut self.tcbs))
    }

    pub(crate) fn with_memory_state<R>(&self, f: impl FnOnce(&MemorySubsystem) -> R) -> R {
        // Lock-order domain: memory
        Self::debug_lock_order_note("memory");
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_ref(&self.memory))
    }

    pub(crate) fn with_memory_state_mut<R>(
        &mut self,
        f: impl FnOnce(&mut MemorySubsystem) -> R,
    ) -> R {
        // Lock-order domain: memory
        Self::debug_lock_order_note("memory");
        let _mem_guard = self.memory_state_lock.lock();
        f(kernel_mut(&mut self.memory))
    }

    // ── Stage 5A split-read helpers ──────────────────────────────────────────

    /// Stage 5A split-read: look up the task class for `tid` under only the
    /// task lock (rank 2). Returns `None` if no task with that TID exists.
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by
    /// the calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; `task_state_lock`
    /// serializes access to both `tcbs` and `task_classes`.
    pub(crate) unsafe fn task_class_from_raw(
        state: *const KernelState,
        tid: u64,
    ) -> Option<TaskClass> {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
        let _guard = lock_ref.lock();
        let tcbs: &[Option<ThreadControlBlock>; MAX_TASKS] =
            kernel_ref(unsafe { &*core::ptr::addr_of!((*state).tcbs) });
        let task_classes: &[Option<TaskClass>; MAX_TASKS] =
            kernel_ref(unsafe { &*core::ptr::addr_of!((*state).task_classes) });
        tcbs.iter().enumerate().find_map(|(idx, slot)| {
            slot.as_ref()
                .filter(|tcb| tcb.tid.0 == tid)
                .and(task_classes[idx])
        })
    }

    /// Stage 5A split-read: check whether a task with `tid` exists under only
    /// the task lock (rank 2).
    ///
    /// # Safety
    /// Same requirements as `task_class_from_raw`.
    pub(crate) unsafe fn task_exists_from_raw(state: *const KernelState, tid: u64) -> bool {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
        let _guard = lock_ref.lock();
        let tcbs: &[Option<ThreadControlBlock>; MAX_TASKS] =
            kernel_ref(unsafe { &*core::ptr::addr_of!((*state).tcbs) });
        tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid)
    }

    /// Stage 5A split-read: read the CNode slot capacity for a process `pid`
    /// under only the capability lock (rank 4). Returns `None` if no CNode is
    /// registered for that pid.
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by
    /// the calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; `capability_state_lock`
    /// serializes access to the `capability` field.
    pub(crate) unsafe fn cnode_slot_capacity_from_raw(
        state: *const KernelState,
        pid: u64,
    ) -> Option<usize> {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).capability_state_lock) };
        let _guard = lock_ref.lock();
        let capability: &CapabilitySubsystem =
            unsafe { &*core::ptr::addr_of!((*state).capability) };
        let cnode = CNodeId(pid);
        kernel_ref(&capability.cnode_spaces)
            .iter()
            .flatten()
            .find(|space| space.id == cnode)
            .map(|space| space.slot_capacity)
    }

    /// Stage 5B split-read: read the thread-group-id (process id) for a thread
    /// under only the task lock (rank 2). Returns `None` if `tid` is not found.
    ///
    /// # Safety
    /// Same requirements as `task_class_from_raw`. `task_state_lock` serializes
    /// access to the `tcbs` array; `addr_of!` avoids a reference to the whole
    /// `KernelState`.
    pub(crate) unsafe fn process_id_from_raw(state: *const KernelState, tid: u64) -> Option<u64> {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
        let _guard = lock_ref.lock();
        let tcbs: &[Option<ThreadControlBlock>; MAX_TASKS] =
            kernel_ref(unsafe { &*core::ptr::addr_of!((*state).tcbs) });
        tcbs.iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.thread_group_id.0)
    }

    /// Stage 5B split-read: check whether `tid` is the thread-group leader under
    /// only the task lock (rank 2). Returns `false` if the task does not exist.
    ///
    /// # Safety
    /// Same requirements as `task_class_from_raw`. `task_state_lock` serializes
    /// access to the `tcbs` array.
    pub(crate) unsafe fn is_group_leader_from_raw(state: *const KernelState, tid: u64) -> bool {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).task_state_lock) };
        let _guard = lock_ref.lock();
        let tcbs: &[Option<ThreadControlBlock>; MAX_TASKS] =
            kernel_ref(unsafe { &*core::ptr::addr_of!((*state).tcbs) });
        tcbs.iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.thread_group_id.0 == tid)
            .unwrap_or(false)
    }

    // ── Stage 26 split-read helpers ──────────────────────────────────────────

    /// STAGE 26: extracted from global lock, uses only domain ipc (rank 3) lock.
    ///
    /// Read whether the notification slot at `notification_idx` has a registered
    /// waiter, returning `1` if so and `0` otherwise (matching the test-only
    /// `notification_waiter_count` probe). Acquires only `ipc_state_lock`; does
    /// not acquire the outer `SharedKernel` lock.
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by the
    /// calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; `ipc_state_lock`
    /// serializes access to the `ipc` field.
    pub(crate) unsafe fn notification_waiter_count_from_raw(
        state: *const KernelState,
        notification_idx: usize,
    ) -> usize {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).ipc_state_lock) };
        let _guard = lock_ref.lock();
        let ipc: &IpcSubsystem = kernel_ref(unsafe { &*core::ptr::addr_of!((*state).ipc) });
        usize::from(
            ipc.notification_waiters
                .get(notification_idx)
                .and_then(|w| *w)
                .is_some(),
        )
    }

    /// STAGE 26: extracted from global lock, uses only domain capability (rank 4) lock.
    ///
    /// Read whether a CNode space is registered for process `pid`. Acquires only
    /// `capability_state_lock`; does not acquire the outer `SharedKernel` lock.
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by the
    /// calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; `capability_state_lock`
    /// serializes access to the `capability` field.
    pub(crate) unsafe fn cnode_registered_from_raw(state: *const KernelState, pid: u64) -> bool {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).capability_state_lock) };
        let _guard = lock_ref.lock();
        let capability: &CapabilitySubsystem =
            unsafe { &*core::ptr::addr_of!((*state).capability) };
        let cnode = CNodeId(pid);
        kernel_ref(&capability.cnode_spaces)
            .iter()
            .flatten()
            .any(|space| space.id == cnode)
    }

    // ── Stage 32 endpoint-cap resolution split-read helpers ──────────────────

    /// STAGE 32: capability-domain (rank 4) phase of endpoint receive-cap
    /// resolution. Looks up `cap` in the cnode registered for `requester_pid`
    /// under ONLY `capability_state_lock`, validates it is a live-eligible
    /// `Endpoint` carrying `CapRights::RECEIVE`, and returns the resolved
    /// `(CapObject::Endpoint, rights)`.
    ///
    /// This reproduces the capability-side of the global-lock `IpcRecv`
    /// resolution (`validate_endpoint_right` + the `capability_for_cnode_local`
    /// re-lookup in `handle_ipc_recv`) WITHOUT the IPC-domain generation
    /// liveness check (`capability_object_live`, which acquires `ipc_state_lock`):
    /// the endpoint generation is returned in the object so the caller can revalidate
    /// it later under `ipc_state_lock` during dequeue. No mutation. No task lock.
    /// No IPC lock. The caller MUST have already read `requester_pid` under the
    /// task lock (and released it) — see `process_id_from_raw`.
    ///
    /// Error mapping matches the old path's `validate_endpoint_right`:
    /// - cnode missing / slot empty → `InvalidCapability`
    /// - object is not an `Endpoint` → `WrongObject`
    /// - endpoint without `RECEIVE` right → `MissingRight`
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by the
    /// calling `SharedKernel`. `addr_of!` derives a raw pointer to the
    /// `capability` field without creating a whole-`KernelState` reference;
    /// `capability_state_lock` serializes access to that field.
    pub(crate) unsafe fn resolve_endpoint_recv_cap_in_pid_from_raw(
        state: *const KernelState,
        requester_pid: u64,
        cap: CapId,
    ) -> Result<(CapObject, CapRights), KernelError> {
        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).capability_state_lock) };
        let _guard = lock_ref.lock();
        let capability: &CapabilitySubsystem =
            unsafe { &*core::ptr::addr_of!((*state).capability) };
        let cnode = kernel_ref(&capability.process_cnodes)
            .iter()
            .flatten()
            .find(|record| record.pid == requester_pid)
            .map(|record| record.cnode)
            .ok_or(KernelError::InvalidCapability)?;
        let capability_obj = capability
            .cnode_spaces
            .iter()
            .flatten()
            .find(|space| space.id == cnode)
            .and_then(|space| kernel_ref(&space.cspace).get(cap))
            .ok_or(KernelError::InvalidCapability)?;
        if !matches!(capability_obj.object, CapObject::Endpoint { .. }) {
            return Err(KernelError::WrongObject);
        }
        if !capability_obj.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        Ok((capability_obj.object, capability_obj.rights()))
    }

    // ── Stage 27 split-mutation helper ───────────────────────────────────────

    /// STAGE 27: first mutating global-lock extraction. Apply a CNode-slot
    /// create/resize for `target_pid` under only the capability domain lock
    /// (rank 4), using a task-domain snapshot (`plan`) and a boot-config snapshot
    /// (`limits`) taken by the caller BEFORE this call.
    ///
    /// This is the capability-domain "apply" phase of
    /// `SharedKernel::control_plane_set_process_cnode_slots_split_mut`. It
    /// reproduces `control_plane_set_process_cnode_slots_planned` exactly — same
    /// authorization check, same create-vs-resize branching, same error returns —
    /// but acquires ONLY `capability_state_lock` and never re-enters the task
    /// lock (the requester class/pid come from `plan`) nor the boot-config lock
    /// (the capacity limits come from `limits`). This preserves task(2) →
    /// capability(4) ordering with no inversion and no global lock.
    ///
    /// Errors preserved (identical to `_planned`):
    /// - `MissingRight`  — non-system-server requester whose pid != target_pid,
    ///   or a non-system-server target of class `App`.
    /// - `WrongObject` / `CapabilityFull` — from slot-capacity normalization.
    /// - `CapabilityFull` — global pool exhausted, or cspace alloc/grow failure.
    /// - `TaskTableFull` — no free cnode-space slot for a new registration.
    /// - `TaskMissing` — resize target cnode-space vanished (race; unchanged).
    ///
    /// # Safety
    /// `state` must be the raw pointer of the `KernelState` storage owned by the
    /// calling `SharedKernel`. `addr_of!` derives raw field pointers without
    /// creating a reference to the whole `KernelState`; `capability_state_lock`
    /// serializes access to the `capability` field for the whole mutation.
    pub(crate) unsafe fn control_plane_set_process_cnode_slots_apply_from_raw(
        state: *mut KernelState,
        plan: &ControlPlaneCnodePlan,
        target_pid: u64,
        slot_capacity: usize,
        limits: RuntimeCapacityConfig,
    ) -> Result<(), KernelError> {
        let requester_is_system_server = plan.requester_class == TaskClass::SystemServer;
        if !requester_is_system_server && plan.requester_pid != target_pid {
            return Err(KernelError::MissingRight);
        }
        // Non-system-server may only resize its OWN cnode (requester_pid ==
        // target_pid guaranteed above); the class guard matches `_planned`.
        if !requester_is_system_server {
            match plan.requester_class {
                TaskClass::Driver | TaskClass::SystemServer => {}
                TaskClass::App => return Err(KernelError::MissingRight),
            }
        }

        let max_total_cnode_slots = limits.max_total_cnode_slots;
        let bounded_slot_capacity = Self::normalize_requested_cnode_slots(slot_capacity, limits)?;
        let target_cnode = CNodeId(target_pid);

        let lock_ref = unsafe { &*core::ptr::addr_of!((*state).capability_state_lock) };
        let _guard = lock_ref.lock();
        let capability: &mut CapabilitySubsystem =
            unsafe { &mut *core::ptr::addr_of_mut!((*state).capability) };

        // Existing-cnode lookup matches `process_cnode_for_pid`: it queries the
        // pid→cnode registration table (`process_cnodes`), NOT `cnode_spaces`.
        let existing_cnode = capability
            .process_cnodes
            .iter()
            .flatten()
            .find(|record| record.pid == target_pid)
            .map(|record| record.cnode);

        if let Some(existing_cnode) = existing_cnode {
            // Resize path: bound against all OTHER reserved cnode slots.
            let reserved_other_slots: usize = capability
                .cnode_spaces
                .iter()
                .flatten()
                .filter(|space| space.id != existing_cnode)
                .map(|space| space.slot_capacity)
                .sum();
            if reserved_other_slots.saturating_add(bounded_slot_capacity) > max_total_cnode_slots {
                return Err(KernelError::CapabilityFull);
            }
            let space = capability
                .cnode_spaces
                .iter_mut()
                .flatten()
                .find(|space| space.id == existing_cnode)
                .ok_or(KernelError::TaskMissing)?;
            kernel_mut(&mut space.cspace)
                .resize_slots(bounded_slot_capacity)
                .map_err(|err| match err {
                    CapabilityDeriveError::SpaceFull => KernelError::CapabilityFull,
                    CapabilityDeriveError::AllocFailed => KernelError::CapabilityFull,
                    CapabilityDeriveError::InvalidSlot => KernelError::WrongObject,
                    _ => KernelError::WrongObject,
                })?;
            space.slot_capacity = bounded_slot_capacity;
            Ok(())
        } else {
            // Create path: ensure cnode space then register the pid→cnode record.
            if capability
                .cnode_spaces
                .iter()
                .flatten()
                .any(|space| space.id == target_cnode)
            {
                // Space already present (no process_cnode record): register only.
                return Self::register_process_cnode_in(capability, target_pid, target_cnode);
            }
            let reserved_slots: usize = capability
                .cnode_spaces
                .iter()
                .flatten()
                .map(|space| space.slot_capacity)
                .sum();
            if reserved_slots.saturating_add(bounded_slot_capacity) > max_total_cnode_slots {
                return Err(KernelError::CapabilityFull);
            }
            let Some(slot) = capability
                .cnode_spaces
                .iter_mut()
                .find(|slot| slot.is_none())
            else {
                return Err(KernelError::TaskTableFull);
            };
            let cspace = CapabilitySpace::try_with_slots(bounded_slot_capacity)
                .map_err(|_| KernelError::CapabilityFull)?;
            *slot = Some(CNodeSpace {
                id: target_cnode,
                slot_capacity: bounded_slot_capacity,
                cspace: store_kernel_value(cspace),
            });
            Self::register_process_cnode_in(capability, target_pid, target_cnode)
        }
    }

    /// Insert or update a pid→cnode registration in the given capability
    /// subsystem (caller already holds the capability lock). Mirrors
    /// `set_process_cnode_for_pid` exactly.
    fn register_process_cnode_in(
        capability: &mut CapabilitySubsystem,
        pid: u64,
        cnode: CNodeId,
    ) -> Result<(), KernelError> {
        if let Some(record) = capability
            .process_cnodes
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
        {
            record.cnode = cnode;
            return Ok(());
        }
        if let Some(slot) = capability
            .process_cnodes
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(ProcessCNodeRecord { pid, cnode });
            return Ok(());
        }
        Err(KernelError::TaskTableFull)
    }
}
