// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::lock::SpinLock;

/// Maximum number of boot command-line bytes retained by the kernel.
///
/// The storage is policy-neutral and keeps arbitrary bytes; UTF-8 validation is
/// left to consumers that interpret a particular option.
pub const BOOT_COMMAND_LINE_MAX_BYTES: usize = 2048;
pub const YARM_BOOT_OPTION_MAX_KEY_BYTES: usize = 64;
pub const YARM_BOOT_OPTION_MAX_VALUE_BYTES: usize = 1024;
pub const YARM_MANIFEST_PATH_MAX_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootCommandLineStatus {
    Absent,
    Captured,
    Truncated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BootCommandLine {
    bytes: [u8; BOOT_COMMAND_LINE_MAX_BYTES],
    len: usize,
    status: BootCommandLineStatus,
}

impl core::fmt::Debug for BootCommandLine {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BootCommandLine")
            .field("bytes", &self.raw_cmdline())
            .field("status", &self.status)
            .finish()
    }
}

impl BootCommandLine {
    pub const fn absent() -> Self {
        Self {
            bytes: [0; BOOT_COMMAND_LINE_MAX_BYTES],
            len: 0,
            status: BootCommandLineStatus::Absent,
        }
    }

    /// Copies bytes through the first NUL, if present, into fixed-size storage.
    ///
    /// Empty input and an immediately terminating NUL are represented as absent.
    /// Inputs longer than the fixed buffer are truncated and marked accordingly.
    /// Bytes are stored losslessly; invalid UTF-8 is not rejected.
    pub fn set_raw_cmdline_from_bytes(&mut self, source: &[u8]) {
        self.bytes.fill(0);
        let nul = source.iter().position(|byte| *byte == 0);
        let source_len = nul.unwrap_or(source.len());
        self.len = core::cmp::min(source_len, BOOT_COMMAND_LINE_MAX_BYTES);
        self.bytes[..self.len].copy_from_slice(&source[..self.len]);
        self.status = if self.len == 0 {
            BootCommandLineStatus::Absent
        } else if source_len > BOOT_COMMAND_LINE_MAX_BYTES {
            BootCommandLineStatus::Truncated
        } else {
            BootCommandLineStatus::Captured
        };
    }

    pub fn raw_cmdline(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub const fn status(&self) -> BootCommandLineStatus {
        self.status
    }

    pub const fn cmdline_was_truncated(&self) -> bool {
        matches!(self.status, BootCommandLineStatus::Truncated)
    }

    /// Monotonic capture: copies `source` UNLESS doing so would replace an
    /// already-stored non-empty command line with an empty one, in which case
    /// the existing command line is preserved untouched.
    ///
    /// This protects the firmware-provided command line on architectures
    /// whose early-boot entry can be re-reached (e.g. RISC-V, where a
    /// pre-kernel fault could restart the payload entry): a later capture that
    /// no longer has a valid DTB pointer must never clear a command line that
    /// was already captured from a valid DTB. Returns `true` if the stored
    /// command line was (re)written, `false` if the existing value was kept.
    pub fn set_raw_cmdline_from_bytes_monotonic(&mut self, source: &[u8]) -> bool {
        let incoming_len = source
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(source.len());
        if incoming_len == 0 && self.len > 0 {
            return false;
        }
        self.set_raw_cmdline_from_bytes(source);
        true
    }
}

static BOOT_COMMAND_LINE: SpinLock<BootCommandLine> = SpinLock::new(BootCommandLine::absent());

/// Applies the YARM boot-option knobs carried by a captured command line.
///
/// Stage 108/109/120: `yarm.loglevel=` sets the console loglevel,
/// `yarm.x86_ap_rust=` flips the x86_64 AP Rust-entry gate, and
/// `yarm.d6_switch_proof=` gates the x86_64-only controlled one-shot
/// switch proof harness. Knobs are applied
/// ONLY when present and valid; otherwise production defaults are kept. The
/// `BootCommandLine` storage itself stays policy-neutral. This is the single
/// chokepoint every arch boot path routes through, so no arch boot file needs
/// to change.
fn apply_boot_option_knobs(captured: &BootCommandLine) {
    let parsed = parse_yarm_boot_options(captured.raw_cmdline());
    if let Some(level) = parsed.console_loglevel {
        crate::kernel::printk::set_console_loglevel(
            crate::kernel::printk::LogLevel::from_u8_public(level),
        );
        crate::yarm_log!("YARM_LOGLEVEL_SET level={}", level);
    }
    if let Some(enabled) = parsed.x86_ap_rust {
        #[cfg(target_arch = "x86_64")]
        {
            crate::arch::x86_64::smp::set_ap_rust_entry_enabled(enabled);
            crate::yarm_log!("YARM_X86_AP_RUST_SET enabled={}", enabled);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = enabled;
    }
    if let Some(enabled) = parsed.d6_switch_proof {
        #[cfg(target_arch = "x86_64")]
        crate::kernel::boot::set_d6_controlled_switch_proof_enabled(enabled);
        #[cfg(not(target_arch = "x86_64"))]
        let _ = enabled;
    }
    if let Some(enabled) = parsed.d6_switch_a {
        // Stage 166 (D6-SWITCH-A): x86_64-only gate that opts a real production
        // switch into the unlocked path. Default-off; no-op on other arches.
        #[cfg(target_arch = "x86_64")]
        {
            crate::kernel::boot::set_d6_switch_a_enabled(enabled);
            if enabled {
                crate::yarm_log!("D6_SWITCH_A_ENABLED");
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = enabled;
    }
    // Stage 182 (REMOVE-FALLBACKS): the per-seam graduation SELECTOR knobs
    // (`yarm.d6_genuine`, `yarm.d2_recv_genuine`, `yarm.d2_send_genuine`) are DELETED.
    // The graduated out-of-lock dispatch seams are the only x86_64 `-smp 1` production
    // path (a compile-time gate now — see `boot::d6_genuine_enabled`), so these knobs
    // no longer select any execution path. They are still recognized ONLY to report
    // that they are obsolete + ignored (proving a stale boot line cannot re-enable the
    // old in-lock production fallback). They never toggle behavior.
    if parsed.d6_genuine.is_some() {
        crate::yarm_log!("UNLOCK_FALLBACK_KNOB_OBSOLETE knob=yarm.d6_genuine action=ignored");
    }
    if parsed.d2_recv_genuine.is_some() {
        crate::yarm_log!("UNLOCK_FALLBACK_KNOB_OBSOLETE knob=yarm.d2_recv_genuine action=ignored");
    }
    if parsed.d2_send_genuine.is_some() {
        crate::yarm_log!("UNLOCK_FALLBACK_KNOB_OBSOLETE knob=yarm.d2_send_genuine action=ignored");
    }
    if let Some(enabled) = parsed.sched_timeout {
        // Stage 171 (SCHED-TIMEOUT): arch-neutral, default-off DIAGNOSTIC gate for
        // the scheduler timeout/deadline hardening markers. Changes no scheduling
        // behavior or ABI — only emits SCHED_TIMEOUT_* / SCHED_IDLE_* markers.
        crate::kernel::boot::set_sched_timeout_enabled(enabled);
        if enabled {
            crate::yarm_log!("SCHED_TIMEOUT_ENABLED");
        }
    }
    if let Some(enabled) = parsed.vm_cow {
        // Stage 172 (VM-COW): arch-neutral, default-off DIAGNOSTIC gate for the
        // VM/COW/page-table/fork phase-boundary markers. Changes no VM behavior or
        // ABI — only emits VM_COW_* / VM_MAP_* / VM_UNMAP_* / VM_TLB_* markers.
        crate::kernel::boot::set_vm_cow_enabled(enabled);
        if enabled {
            crate::yarm_log!("VM_COW_ENABLED");
        }
    }
    if let Some(enabled) = parsed.cap_cnode {
        // Stage 173 (CAP-CNODE): arch-neutral, default-off DIAGNOSTIC gate for the
        // capability/CNode phase-boundary markers + one-shot proof. Changes no
        // cap/CNode behavior or ABI — only emits CAP_CNODE_* markers.
        crate::kernel::boot::set_cap_cnode_enabled(enabled);
        if enabled {
            crate::yarm_log!("CAP_CNODE_ENABLED");
        }
    }
    if let Some(enabled) = parsed.fault_delivery {
        // Stage 174 (FAULT-DELIVERY): arch-neutral, default-off DIAGNOSTIC gate for
        // the kernel-fault → supervisor delivery / fault-channel lifecycle markers +
        // one-shot proof. Changes no fault/IPC behavior or ABI — only emits
        // FAULT_DELIVERY_* markers.
        crate::kernel::boot::set_fault_delivery_enabled(enabled);
        if enabled {
            crate::yarm_log!("FAULT_DELIVERY_ENABLED");
        }
    }
    if let Some(enabled) = parsed.spawn_lifecycle {
        // Stage 175 (SPAWN-LIFECYCLE): arch-neutral, default-off DIAGNOSTIC gate for
        // the spawn / image-loading / lifecycle-metadata markers + one-shot proof.
        // Changes no spawn/PM behavior or ABI — only emits SPAWN_LIFECYCLE_* markers.
        crate::kernel::boot::set_spawn_lifecycle_enabled(enabled);
        if enabled {
            crate::yarm_log!("SPAWN_LIFECYCLE_ENABLED");
        }
    }
    if let Some(enabled) = parsed.global_state {
        // Stage 176 (GLOBAL-STATE): arch-neutral, default-off DIAGNOSTIC gate for the
        // remaining direct global-KernelState mutation audit + lock-rank markers +
        // one-shot audit. Changes no state/ABI behavior — only emits GLOBAL_STATE_*.
        crate::kernel::boot::set_global_state_enabled(enabled);
        if enabled {
            crate::yarm_log!("GLOBAL_STATE_ENABLED");
        }
    }
    if let Some(enabled) = parsed.smp_ready {
        // Stage 177 (SMP-READY): arch-neutral, default-off DIAGNOSTIC gate for the
        // x86_64 SMP-readiness audit markers + one-shot audit. Changes no state/ABI/
        // SMP behavior (APs stay out of the production scheduler) — only emits
        // SMP_READY_* markers.
        crate::kernel::boot::set_smp_ready_enabled(enabled);
        if enabled {
            crate::yarm_log!("SMP_READY_ENABLED");
        }
    }
    if let Some(enabled) = parsed.cross_arch_d6 {
        // Stage 178 (CROSS-ARCH-D6): arch-neutral, default-off DIAGNOSTIC gate for the
        // AArch64/RISC-V D6 restore-path audit markers + one-shot audit. Changes no
        // state/ABI/dispatch behavior and live-wires no cross-arch D6 restore — only
        // emits CROSS_ARCH_D6_* markers.
        crate::kernel::boot::set_cross_arch_d6_enabled(enabled);
        if enabled {
            crate::yarm_log!("CROSS_ARCH_D6_ENABLED");
        }
    }
    if let Some(enabled) = parsed.d3_full {
        // Stage 179 (D3-FULL): arch-neutral, default-off gate for the D3 VM
        // anon-map/unmap two-phase markers + one-shot self-contained proof. Changes
        // no production VM ABI and claims no real SMP shootdown — only emits D3_*.
        crate::kernel::boot::set_d3_full_enabled(enabled);
        if enabled {
            crate::yarm_log!("D3_FULL_ENABLED");
        }
    }
    // Stage 182 (REMOVE-FALLBACKS): the `yarm.unlock_graduated` umbrella knob is DELETED,
    // including its `=0` EMERGENCY OPT-OUT that forced the old global-lock production
    // path. Graduation is no longer a runtime toggle — `boot::d6_genuine_enabled()` is a
    // compile-time constant (graduated on x86_64 unless a D6-switch diagnostic owns the
    // path; in-lock only on other arches / SMP>1 via the eligibility guard). The knob is
    // recognized ONLY to report it is obsolete + ignored; it can never re-enable the
    // fallback. (No `set_*` calls: the seam gates have no setter anymore.)
    if parsed.unlock_graduated.is_some() {
        crate::yarm_log!("UNLOCK_FALLBACK_KNOB_OBSOLETE knob=yarm.unlock_graduated action=ignored");
    }
    if let Some(enabled) = parsed.ipc_recv_proof {
        // Arch-neutral: the exercise drives the same recv-v2 delivery markers on
        // every arch (the AArch64 queued-split gap is the motivating case).
        crate::kernel::boot::set_ipc_recv_oracle_proof_enabled(enabled);
        crate::yarm_log!("YARM_IPC_RECV_PROOF_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_recv_proof_sender_wake {
        // Stage 163 sub-knob: only meaningful in combination with the base proof
        // knob above; gates the sender-wake coordination hook + workload.
        crate::kernel::boot::set_ipc_recv_proof_sender_wake_enabled(enabled);
        crate::yarm_log!("YARM_IPC_RECV_PROOF_SENDER_WAKE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_send_plain_oracle {
        // Stage 193B sub-knob: only meaningful with the base proof knob above;
        // gates the receiver-blocked coordination hook + IpcSend-plain live oracle.
        crate::kernel::boot::set_ipc_send_plain_oracle_enabled(enabled);
        crate::yarm_log!("YARM_IPC_SEND_PLAIN_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_send_cap_oracle {
        // Stage 193C sub-knob: only meaningful with the base proof knob above;
        // gates the receiver-blocked coordination hook + IpcSend ordinary-cap oracle.
        crate::kernel::boot::set_ipc_send_cap_oracle_enabled(enabled);
        crate::yarm_log!("YARM_IPC_SEND_CAP_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_send_reply_cap_oracle {
        // Stage 193D sub-knob: only meaningful with the base proof knob above;
        // gates the receiver-blocked coordination hook + IpcSend reply-cap oracle.
        crate::kernel::boot::set_ipc_send_reply_cap_oracle_enabled(enabled);
        crate::yarm_log!("YARM_IPC_SEND_REPLY_CAP_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_send_enqueue_oracle {
        // Stage 193E sub-knob: only meaningful with the base proof knob above;
        // gates the IpcSend plain no-waiter enqueue live oracle workload.
        crate::kernel::boot::set_ipc_send_enqueue_oracle_enabled(enabled);
        crate::yarm_log!("YARM_IPC_SEND_ENQUEUE_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ipc_send_cap_enqueue_oracle {
        // Stage 193F sub-knob: only meaningful with the base proof knob above;
        // gates the IpcSend ordinary-cap no-waiter enqueue live oracle workload.
        crate::kernel::boot::set_ipc_send_cap_enqueue_oracle_enabled(enabled);
        crate::yarm_log!("YARM_IPC_SEND_CAP_ENQUEUE_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.aarch64_futex_wake_oracle {
        // Stage 195C: default-off AArch64 FutexWake live oracle knob (independent of the
        // IPC proof knob). Signals init to run the parent/child FutexWake oracle.
        crate::kernel::boot::set_aarch64_futex_wake_oracle_enabled(enabled);
        crate::yarm_log!("YARM_AARCH64_FUTEX_WAKE_ORACLE_SET enabled={}", enabled);
    }
    if let Some(enabled) = parsed.ap_user_dispatch {
        // Stage 189C6 (LIVE-AP-DISPATCH): x86_64-only, default-off gate arming the
        // first live AP user dispatch. No-op on other arches; when OFF the AP
        // idle-loop hook is inert and the SMP baseline is preserved.
        crate::kernel::boot::set_ap_user_dispatch_enabled(enabled);
        if enabled {
            crate::yarm_log!("AP_USER_DISPATCH_ENABLED");
        }
    }
}

pub fn set_raw_cmdline_from_bytes(source: &[u8]) -> BootCommandLine {
    let captured = {
        let mut command_line = BOOT_COMMAND_LINE.lock();
        command_line.set_raw_cmdline_from_bytes(source);
        *command_line
    };
    apply_boot_option_knobs(&captured);
    captured
}

/// Monotonic variant of [`set_raw_cmdline_from_bytes`]: captures `source` and
/// applies the boot-option knobs, but never replaces an already-captured
/// non-empty command line with an empty one. Used by early-boot entries that
/// can be re-reached so a missing-DTB re-capture cannot clobber a valid
/// command line. Returns the resulting (possibly preserved) command line.
pub fn set_raw_cmdline_from_bytes_monotonic(source: &[u8]) -> BootCommandLine {
    let (captured, wrote) = {
        let mut command_line = BOOT_COMMAND_LINE.lock();
        let wrote = command_line.set_raw_cmdline_from_bytes_monotonic(source);
        (*command_line, wrote)
    };
    // Apply knobs only when this call actually (re)captured; a preserved
    // command line keeps the knob state established by its original capture.
    if wrote {
        apply_boot_option_knobs(&captured);
    }
    captured
}

pub fn boot_command_line() -> BootCommandLine {
    *BOOT_COMMAND_LINE.lock()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlatformOption {
    #[default]
    Auto,
    QemuVirt,
    Rpi5,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BootPhase {
    Entry,
    Uart,
    Dtb,
    Mmu,
    #[default]
    Kernel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct YarmBootOptions<'a> {
    pub manifest_path: Option<&'a [u8]>,
    pub platform: PlatformOption,
    pub boot_phase: BootPhase,
    pub max_cpus: Option<usize>,
    /// Stage 108 / Milestone 2 Pass 1: `yarm.loglevel=` observability knob.
    ///
    /// Accepts a printk level as a digit `0`–`7` or a name
    /// (`emerg|alert|crit|err|warn|notice|info|debug`). `None` when the key
    /// is absent or the value is invalid — the console loglevel is then left
    /// at its production default (Info). The knob can only be applied at
    /// boot-cmdline capture time; it never changes the default.
    pub console_loglevel: Option<u8>,
    /// Stage 109 / Milestone 2 Pass 2: `yarm.x86_ap_rust=1` knob. Sets the
    /// `set_ap_rust_entry_enabled` gate in `arch::x86_64::smp` at capture
    /// time. Today the trampoline asm is unchanged and ignores the gate;
    /// the knob exists as Pass 2 scaffolding so the cmdline plumbing for
    /// Pass 3's live Rust-entry wiring is already in place and testable.
    pub x86_ap_rust: Option<bool>,
    /// Stage 120: `yarm.d6_switch_proof=1` gates the x86_64-only,
    /// single-CPU-only, one-shot unlocked `switch_frames` proof harness.
    /// Non-x86_64 builds parse but ignore the knob so AArch64/RISC-V
    /// behavior remains unchanged.
    pub d6_switch_proof: Option<bool>,
    /// Stage 166 (D6-SWITCH-A): `yarm.d6_switch_a=1` gates the x86_64-only,
    /// single-CPU-only first narrow production Outcome A — a real production
    /// `switch_frames` that drops the global lock. Default-off; non-x86_64
    /// builds parse but ignore it so AArch64/RISC-V behavior is unchanged.
    pub d6_switch_a: Option<bool>,
    /// Stage 167 (D6-GENUINE-A): `yarm.d6_genuine=1` gates the x86_64-only,
    /// single-CPU-only, default-off live wire that makes the rank-1 scheduler
    /// split seam (`with_scheduler_split_mut`) its first production caller,
    /// running one `local_dispatch_step_split` observation outside the global
    /// lock per eligible trap. Default-off; non-x86_64 builds parse but ignore
    /// it so AArch64/RISC-V behavior is unchanged.
    pub d6_genuine: Option<bool>,
    /// Stage 168 (D2-GENUINE-RECV): `yarm.d2_recv_genuine=1` gates the x86_64-only,
    /// default-off blocking-recv rank-clean phase live-wire (scheduler/task/IPC
    /// phase markers + Stage 168 out-of-global-lock dispatch seam where eligible).
    /// Default-off; non-x86_64 builds parse but ignore it so AArch64/RISC-V
    /// behavior is unchanged.
    pub d2_recv_genuine: Option<bool>,
    /// Stage 169 (D2-GENUINE-SEND): `yarm.d2_send_genuine=1` gates the x86_64-only,
    /// default-off blocking-send rank-clean phase live-wire (scheduler/task/IPC
    /// phase markers + out-of-global-lock dispatch seam). Default-off; non-x86_64
    /// builds parse but ignore it so AArch64/RISC-V behavior is unchanged.
    pub d2_send_genuine: Option<bool>,
    /// Stage 171 (SCHED-TIMEOUT): `yarm.sched_timeout=1` gates the arch-neutral,
    /// default-off scheduler timeout/deadline DIAGNOSTIC markers (no behavior/ABI
    /// change; the chunked-scan hardening is always on).
    pub sched_timeout: Option<bool>,
    /// Stage 172 (VM-COW): `yarm.vm_cow=1` gates the arch-neutral, default-off
    /// VM/COW/page-table/fork phase-boundary DIAGNOSTIC markers (no behavior/ABI
    /// change).
    pub vm_cow: Option<bool>,
    /// Stage 173 (CAP-CNODE): `yarm.cap_cnode=1` gates the arch-neutral, default-off
    /// capability/CNode phase-boundary DIAGNOSTIC markers + one-shot proof (no
    /// behavior/ABI change).
    pub cap_cnode: Option<bool>,
    /// Stage 174 (FAULT-DELIVERY): `yarm.fault_delivery=1` gates the arch-neutral,
    /// default-off kernel-fault → supervisor delivery / fault-channel lifecycle
    /// DIAGNOSTIC markers + one-shot fault-delivery proof (no behavior/ABI change).
    pub fault_delivery: Option<bool>,
    /// Stage 175 (SPAWN-LIFECYCLE): `yarm.spawn_lifecycle=1` gates the arch-neutral,
    /// default-off spawn / image-loading / lifecycle-metadata DIAGNOSTIC markers +
    /// one-shot rollback proof (no behavior/ABI/PM-policy change).
    pub spawn_lifecycle: Option<bool>,
    /// Stage 176 (GLOBAL-STATE): `yarm.global_state=1` gates the arch-neutral,
    /// default-off remaining direct global-`KernelState` mutation audit + lock-rank
    /// discipline DIAGNOSTIC markers + one-shot audit (no behavior/ABI change).
    pub global_state: Option<bool>,
    /// Stage 177 (SMP-READY): `yarm.smp_ready=1` gates the arch-neutral, default-off
    /// x86_64 SMP-readiness audit (AP bring-up / per-CPU / remote-wake + IPI
    /// readiness) DIAGNOSTIC markers + one-shot audit (no behavior/ABI/SMP change).
    pub smp_ready: Option<bool>,
    /// Stage 178 (CROSS-ARCH-D6): `yarm.cross_arch_d6=1` gates the arch-neutral,
    /// default-off AArch64/RISC-V D6 restore-path audit (trapframe / exception-return
    /// / dispatch / lock-drop readiness) DIAGNOSTIC markers + one-shot audit (no
    /// behavior/ABI/dispatch change; no cross-arch D6 live-wire).
    pub cross_arch_d6: Option<bool>,
    /// Stage 179 (D3-FULL): `yarm.d3_full=1` gates the arch-neutral, default-off D3 VM
    /// anonymous map/unmap two-phase diagnostic markers + one-shot self-contained D3
    /// proof (local TLB flush live, remote shootdown prepped/deferred; no VM ABI change).
    pub d3_full: Option<bool>,
    /// Stage 181 (GRADUATE-KNOBS): `yarm.unlock_graduated=0|1` umbrella. Absent ⇒
    /// default (graduated on x86_64 single-CPU); `1` explicitly graduates; `0` is the
    /// emergency opt-out that forces the conservative per-stage-off path. This is a
    /// REAL production-behavior gate, not a diagnostic knob.
    pub unlock_graduated: Option<bool>,
    /// Stage 159: `yarm.ipc_recv_proof=1` gates the default-off, arch-neutral
    /// userspace IPC recv-v2 oracle exercise client. When set, the control-plane
    /// bootstrap provisions a loopback endpoint into the exercise workload, which
    /// deterministically drives the queued-split, sender-wake, and rollback
    /// recv-v2 markers. Default-off: nothing is provisioned or run otherwise.
    pub ipc_recv_proof: Option<bool>,
    /// Stage 163: `yarm.ipc_recv_proof_sender_wake=1` SUB-knob. Default-off and
    /// only meaningful with `ipc_recv_proof`; gates the deterministic sender-wake
    /// coordination hook + workload, isolating it from the green queued-split +
    /// rollback proof boots.
    pub ipc_recv_proof_sender_wake: Option<bool>,
    /// Stage 193B: `yarm.ipc_send_plain_oracle=1` SUB-knob. Default-off and only
    /// meaningful with `ipc_recv_proof`; gates the deterministic IpcSend-plain
    /// live oracle (a forked child blocks on recv-v2, init plain-sends to it) that
    /// fires the 193A `class=IpcSendPlain` boundary split in QEMU. Independent of
    /// the sender-wake sub-knob (mutually exclusive coordination-slot pattern).
    pub ipc_send_plain_oracle: Option<bool>,
    /// Stage 193C: `yarm.ipc_send_cap_oracle=1` SUB-knob. Default-off and only
    /// meaningful with `ipc_recv_proof`; gates the deterministic IpcSend ordinary
    /// cap-transfer live oracle (a forked child blocks on recv-v2, init sends it a
    /// message carrying one ordinary cap) that fires the 193C
    /// `class=IpcSendOrdinaryCap` boundary split in QEMU. Independent of the plain
    /// and sender-wake sub-knobs (mutually exclusive coordination-slot pattern).
    pub ipc_send_cap_oracle: Option<bool>,
    /// Stage 193D: `yarm.ipc_send_reply_cap_oracle=1` SUB-knob. Default-off and only
    /// meaningful with `ipc_recv_proof`; gates the deterministic IpcSend reply-cap
    /// transfer live oracle (a forked child blocks on recv-v2, init transfers it a
    /// kernel-provisioned one-shot reply cap) that fires the 193D
    /// `class=IpcSendReplyCap` boundary split in QEMU. Independent of the plain,
    /// ordinary-cap, and sender-wake sub-knobs.
    pub ipc_send_reply_cap_oracle: Option<bool>,
    /// Stage 193E: `yarm.ipc_send_enqueue_oracle=1` SUB-knob. Default-off and only
    /// meaningful with `ipc_recv_proof`; gates the IpcSend plain no-waiter enqueue live
    /// oracle (init plain-sends to the loopback with no blocked receiver → the message
    /// enqueues) that fires the 193E `class=IpcSendPlainEnqueue` boundary split in QEMU.
    pub ipc_send_enqueue_oracle: Option<bool>,
    /// Stage 193F: `yarm.ipc_send_cap_enqueue_oracle=1` SUB-knob. Default-off and only
    /// meaningful with `ipc_recv_proof`; gates the IpcSend ordinary-cap no-waiter enqueue
    /// live oracle (init sends a cap-transfer to the loopback with no blocked receiver → the
    /// message enqueues; a later recv materializes a fresh receiver-local cap) that fires the
    /// 193F `class=IpcSendOrdinaryCapEnqueue` boundary split in QEMU.
    pub ipc_send_cap_enqueue_oracle: Option<bool>,
    /// Stage 195C: `yarm.aarch64_futex_wake_oracle=1` DEFAULT-OFF knob (independent of the
    /// IPC proof knob). Signals AArch64 init to run the parent/child FutexWake live oracle
    /// that fires the `class=FutexWake` split retirement + wake-count proof in QEMU.
    pub aarch64_futex_wake_oracle: Option<bool>,
    /// Stage 189C6: `yarm.ap_user_dispatch=1` DEFAULT-OFF gate that arms the first
    /// live x86_64 AP user dispatch (build probe task → wake AP → ring3 entry +
    /// probe syscall re-entry). Off ⇒ the accepted smp2/smp4 baseline is preserved.
    pub ap_user_dispatch: Option<bool>,
}

/// Parse a `yarm.loglevel=` value: digit 0–7 or a level name.
fn parse_loglevel_value(value: &[u8]) -> Option<u8> {
    match value {
        [d @ b'0'..=b'7'] => Some(d - b'0'),
        b"emerg" => Some(0),
        b"alert" => Some(1),
        b"crit" => Some(2),
        b"err" => Some(3),
        b"warn" => Some(4),
        b"notice" => Some(5),
        b"info" => Some(6),
        b"debug" => Some(7),
        _ => None,
    }
}

/// Parses YARM-owned `key=value` tokens without applying any boot policy.
///
/// Tokens are ASCII-whitespace separated. Tokens without `=` and keys outside
/// the `yarm.` namespace are ignored. Duplicate recognized keys use last-wins
/// semantics. Unknown `yarm.*` keys are ignored. The returned manifest path is
/// borrowed from `raw` and is never read from CPIO or acted upon here.
pub fn parse_yarm_boot_options(raw: &[u8]) -> YarmBootOptions<'_> {
    let mut options = YarmBootOptions::default();
    for token in raw.split(|byte| byte.is_ascii_whitespace()) {
        if token.is_empty() {
            continue;
        }
        let Some(separator) = token.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = &token[..separator];
        let value = &token[separator + 1..];
        if key.len() > YARM_BOOT_OPTION_MAX_KEY_BYTES
            || value.len() > YARM_BOOT_OPTION_MAX_VALUE_BYTES
            || !key.starts_with(b"yarm.")
        {
            continue;
        }
        match key {
            b"yarm.manifest" => {
                options.manifest_path = valid_manifest_path(value).then_some(value);
            }
            b"yarm.platform" => {
                if let Some(platform) = parse_platform_option(value) {
                    options.platform = platform;
                }
            }
            b"yarm.boot_phase" => {
                if let Some(phase) = parse_boot_phase(value) {
                    options.boot_phase = phase;
                }
            }
            b"yarm.max_cpus" => {
                if let Some(max_cpus) = parse_positive_usize(value) {
                    options.max_cpus = Some(max_cpus);
                }
            }
            _ => {}
        }
        if key == b"yarm.loglevel" {
            // Invalid values leave the option unset (last-wins only among
            // valid values would deviate from the manifest key's last-wins
            // semantics, so mirror those exactly: last token wins, and an
            // invalid last token clears back to None).
            options.console_loglevel = parse_loglevel_value(value);
        }
        if key == b"yarm.x86_ap_rust" {
            options.x86_ap_rust = parse_bool_knob(value);
        }
        if key == b"yarm.d6_switch_proof" {
            options.d6_switch_proof = parse_bool_knob(value);
        }
        if key == b"yarm.d6_switch_a" {
            options.d6_switch_a = parse_bool_knob(value);
        }
        if key == b"yarm.d6_genuine" {
            options.d6_genuine = parse_bool_knob(value);
        }
        if key == b"yarm.d2_recv_genuine" {
            options.d2_recv_genuine = parse_bool_knob(value);
        }
        if key == b"yarm.d2_send_genuine" {
            options.d2_send_genuine = parse_bool_knob(value);
        }
        if key == b"yarm.sched_timeout" {
            options.sched_timeout = parse_bool_knob(value);
        }
        if key == b"yarm.vm_cow" {
            options.vm_cow = parse_bool_knob(value);
        }
        if key == b"yarm.cap_cnode" {
            options.cap_cnode = parse_bool_knob(value);
        }
        if key == b"yarm.fault_delivery" {
            options.fault_delivery = parse_bool_knob(value);
        }
        if key == b"yarm.spawn_lifecycle" {
            options.spawn_lifecycle = parse_bool_knob(value);
        }
        if key == b"yarm.global_state" {
            options.global_state = parse_bool_knob(value);
        }
        if key == b"yarm.smp_ready" {
            options.smp_ready = parse_bool_knob(value);
        }
        if key == b"yarm.cross_arch_d6" {
            options.cross_arch_d6 = parse_bool_knob(value);
        }
        if key == b"yarm.d3_full" {
            options.d3_full = parse_bool_knob(value);
        }
        if key == b"yarm.unlock_graduated" {
            options.unlock_graduated = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_recv_proof" {
            options.ipc_recv_proof = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_recv_proof_sender_wake" {
            options.ipc_recv_proof_sender_wake = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_send_plain_oracle" {
            options.ipc_send_plain_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_send_cap_oracle" {
            options.ipc_send_cap_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_send_reply_cap_oracle" {
            options.ipc_send_reply_cap_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_send_enqueue_oracle" {
            options.ipc_send_enqueue_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.ipc_send_cap_enqueue_oracle" {
            options.ipc_send_cap_enqueue_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.aarch64_futex_wake_oracle" {
            options.aarch64_futex_wake_oracle = parse_bool_knob(value);
        }
        if key == b"yarm.ap_user_dispatch" {
            options.ap_user_dispatch = parse_bool_knob(value);
        }
    }
    options
}

fn parse_bool_knob(value: &[u8]) -> Option<bool> {
    match value {
        b"1" | b"true" | b"yes" | b"on" => Some(true),
        b"0" | b"false" | b"no" | b"off" => Some(false),
        _ => None,
    }
}

fn parse_platform_option(value: &[u8]) -> Option<PlatformOption> {
    match value {
        b"auto" => Some(PlatformOption::Auto),
        b"qemu-virt" => Some(PlatformOption::QemuVirt),
        b"rpi5" => Some(PlatformOption::Rpi5),
        _ => None,
    }
}

fn parse_boot_phase(value: &[u8]) -> Option<BootPhase> {
    match value {
        b"entry" => Some(BootPhase::Entry),
        b"uart" => Some(BootPhase::Uart),
        b"dtb" => Some(BootPhase::Dtb),
        b"mmu" => Some(BootPhase::Mmu),
        b"kernel" => Some(BootPhase::Kernel),
        _ => None,
    }
}

fn parse_positive_usize(value: &[u8]) -> Option<usize> {
    if value.is_empty() {
        return None;
    }
    let mut parsed = 0usize;
    for byte in value {
        if !byte.is_ascii_digit() {
            return None;
        }
        parsed = parsed
            .checked_mul(10)?
            .checked_add((byte - b'0') as usize)?;
    }
    (parsed > 0).then_some(parsed)
}

fn valid_manifest_path(path: &[u8]) -> bool {
    !path.is_empty()
        && path.len() <= YARM_MANIFEST_PATH_MAX_BYTES
        && path[0] == b'/'
        && path
            .iter()
            .all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_defaults_to_absent() {
        let command_line = BootCommandLine::absent();
        assert_eq!(command_line.raw_cmdline(), b"");
        assert_eq!(command_line.status(), BootCommandLineStatus::Absent);
    }

    #[test]
    fn riscv_monotonic_valid_cmdline_then_missing_dtb_is_preserved() {
        // RISC-V early boot: a valid command line is captured from a valid
        // DTB, then a later re-capture with a missing DTB (empty source) must
        // NOT clear it.
        let mut command_line = BootCommandLine::absent();
        let wrote =
            command_line.set_raw_cmdline_from_bytes_monotonic(b"console=ttyS0 rdinit=/init");
        assert!(wrote, "first valid capture must be written");
        assert_eq!(command_line.raw_cmdline(), b"console=ttyS0 rdinit=/init");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);

        // Missing-DTB re-capture: empty source must be ignored, value kept.
        let wrote_again = command_line.set_raw_cmdline_from_bytes_monotonic(b"");
        assert!(
            !wrote_again,
            "empty re-capture must not overwrite valid cmdline"
        );
        assert_eq!(
            command_line.raw_cmdline(),
            b"console=ttyS0 rdinit=/init",
            "valid cmdline must survive a missing-DTB re-capture"
        );
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);

        // Even repeated empty re-captures keep the original (no overwrite loop).
        for _ in 0..8 {
            assert!(!command_line.set_raw_cmdline_from_bytes_monotonic(b""));
        }
        assert_eq!(command_line.raw_cmdline(), b"console=ttyS0 rdinit=/init");
    }

    #[test]
    fn riscv_monotonic_missing_dtb_first_records_empty_once_and_is_idempotent() {
        // If the very first capture has a missing DTB (empty source), it is
        // recorded as absent exactly once; subsequent empty captures remain a
        // stable no-op (no spam, no flapping), and a later valid capture can
        // still populate it.
        let mut command_line = BootCommandLine::absent();
        let wrote = command_line.set_raw_cmdline_from_bytes_monotonic(b"");
        assert!(
            wrote,
            "first empty capture records the empty/absent state once"
        );
        assert_eq!(command_line.raw_cmdline(), b"");
        assert_eq!(command_line.status(), BootCommandLineStatus::Absent);

        // Repeated empty captures stay a stable no-op.
        let wrote_again = command_line.set_raw_cmdline_from_bytes_monotonic(b"");
        assert!(
            wrote_again,
            "empty-over-empty still records empty (nothing to preserve)"
        );
        assert_eq!(command_line.raw_cmdline(), b"");
        assert_eq!(command_line.status(), BootCommandLineStatus::Absent);

        // A subsequent valid capture is still allowed to populate it.
        assert!(command_line.set_raw_cmdline_from_bytes_monotonic(b"console=ttyS0"));
        assert_eq!(command_line.raw_cmdline(), b"console=ttyS0");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);
    }

    #[test]
    fn riscv_monotonic_stops_at_nul_when_deciding_emptiness() {
        // A source that is NUL-first is treated as empty for preservation
        // purposes (mirrors set_raw_cmdline_from_bytes NUL handling).
        let mut command_line = BootCommandLine::absent();
        assert!(command_line.set_raw_cmdline_from_bytes_monotonic(b"yarm.manifest=/boot/a"));
        assert_eq!(command_line.raw_cmdline(), b"yarm.manifest=/boot/a");
        // Leading NUL => incoming length 0 => preserve existing.
        assert!(!command_line.set_raw_cmdline_from_bytes_monotonic(b"\0ignored"));
        assert_eq!(command_line.raw_cmdline(), b"yarm.manifest=/boot/a");
    }

    #[test]
    fn command_line_copies_normal_and_invalid_bytes_losslessly() {
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(b"console=ttyS0 \xff");
        assert_eq!(command_line.raw_cmdline(), b"console=ttyS0 \xff");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);
    }

    #[test]
    fn command_line_accepts_exact_maximum() {
        let source = [b'x'; BOOT_COMMAND_LINE_MAX_BYTES];
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(&source);
        assert_eq!(command_line.raw_cmdline(), source);
        assert!(!command_line.cmdline_was_truncated());
    }

    #[test]
    fn command_line_truncates_overlong_input() {
        let source = [b'x'; BOOT_COMMAND_LINE_MAX_BYTES + 1];
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(&source);
        assert_eq!(
            command_line.raw_cmdline().len(),
            BOOT_COMMAND_LINE_MAX_BYTES
        );
        assert!(command_line.cmdline_was_truncated());
    }

    #[test]
    fn command_line_stops_at_nul_before_applying_limit() {
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(b"yarm.manifest=/boot/a\0ignored");
        assert_eq!(command_line.raw_cmdline(), b"yarm.manifest=/boot/a");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);
    }

    #[test]
    fn parser_extracts_manifest_and_ignores_linux_options() {
        let parsed = parse_yarm_boot_options(
            b"console=ttyS0 rdinit=/init other=value yarm.manifest=/boot/services-core.txt",
        );
        assert_eq!(
            parsed.manifest_path,
            Some(b"/boot/services-core.txt".as_slice())
        );
    }

    #[test]
    fn parser_uses_last_manifest_value() {
        let parsed = parse_yarm_boot_options(
            b"yarm.manifest=/boot/first.txt yarm.unknown=x yarm.manifest=/boot/last.txt",
        );
        assert_eq!(parsed.manifest_path, Some(b"/boot/last.txt".as_slice()));
    }

    #[test]
    fn stage108_loglevel_parses_digits_and_names() {
        for (raw, expected) in [
            (b"yarm.loglevel=0".as_slice(), Some(0u8)),
            (b"yarm.loglevel=7".as_slice(), Some(7)),
            (b"yarm.loglevel=debug".as_slice(), Some(7)),
            (b"yarm.loglevel=info".as_slice(), Some(6)),
            (b"yarm.loglevel=warn".as_slice(), Some(4)),
            (b"yarm.loglevel=emerg".as_slice(), Some(0)),
        ] {
            assert_eq!(
                parse_yarm_boot_options(raw).console_loglevel,
                expected,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn stage108_loglevel_rejects_invalid_values_keeping_default() {
        for raw in [
            b"yarm.loglevel=8".as_slice(),
            b"yarm.loglevel=99".as_slice(),
            b"yarm.loglevel=".as_slice(),
            b"yarm.loglevel=verbose".as_slice(),
            b"yarm.loglevel=-1".as_slice(),
            b"loglevel=7".as_slice(), // non-yarm namespace ignored
            b"console=ttyS0 rdinit=/init".as_slice(), // absent
        ] {
            assert_eq!(
                parse_yarm_boot_options(raw).console_loglevel,
                None,
                "{raw:?} must leave the production default untouched"
            );
        }
    }

    #[test]
    fn stage108_loglevel_last_token_wins_including_invalid() {
        // Mirrors yarm.manifest semantics exactly: last token wins, and an
        // invalid last token clears back to None (production default).
        let parsed = parse_yarm_boot_options(b"yarm.loglevel=debug yarm.loglevel=3");
        assert_eq!(parsed.console_loglevel, Some(3));
        let parsed = parse_yarm_boot_options(b"yarm.loglevel=3 yarm.loglevel=bogus");
        assert_eq!(parsed.console_loglevel, None);
    }

    #[test]
    fn stage108_loglevel_does_not_disturb_manifest_parsing() {
        // RPi5 Stage1 / existing cmdline-semantics preservation: the new key
        // must not interfere with yarm.manifest or any non-yarm token.
        let parsed = parse_yarm_boot_options(
            b"console=ttyAMA0 yarm.loglevel=debug yarm.manifest=/boot/services-core.txt",
        );
        assert_eq!(
            parsed.manifest_path,
            Some(b"/boot/services-core.txt".as_slice())
        );
        assert_eq!(parsed.console_loglevel, Some(7));
    }

    #[test]
    fn stage108_capture_applies_loglevel_then_restores_default_capture_does_not() {
        use crate::kernel::printk::{LogLevel, console_loglevel, set_console_loglevel};
        // Capture WITH the knob: console loglevel changes.
        let _ = set_raw_cmdline_from_bytes(b"console=ttyS0 yarm.loglevel=debug");
        assert_eq!(console_loglevel(), LogLevel::Debug);
        // Restore default, then capture WITHOUT the knob: default untouched.
        set_console_loglevel(LogLevel::Info);
        let _ = set_raw_cmdline_from_bytes(b"console=ttyS0 rdinit=/init");
        assert_eq!(
            console_loglevel(),
            LogLevel::Info,
            "absent knob must leave the production default unchanged"
        );
    }

    #[test]
    fn parser_rejects_invalid_manifest_paths() {
        for raw in [
            b"yarm.manifest=relative".as_slice(),
            b"yarm.manifest=".as_slice(),
            b"yarm.manifest=/boot/control\x01path".as_slice(),
            b"yarm.manifest=\"/boot/has space\"".as_slice(),
        ] {
            assert_eq!(parse_yarm_boot_options(raw).manifest_path, None, "{raw:?}");
        }
    }

    #[test]
    fn platform_phase_and_cpu_options_parse_with_qemu_preserving_defaults() {
        let defaults = parse_yarm_boot_options(b"");
        assert_eq!(defaults.platform, PlatformOption::Auto);
        assert_eq!(defaults.boot_phase, BootPhase::Kernel);
        assert_eq!(defaults.max_cpus, None);

        let parsed =
            parse_yarm_boot_options(b"yarm.platform=rpi5 yarm.boot_phase=uart yarm.max_cpus=1");
        assert_eq!(parsed.platform, PlatformOption::Rpi5);
        assert_eq!(parsed.boot_phase, BootPhase::Uart);
        assert_eq!(parsed.max_cpus, Some(1));
    }

    #[test]
    fn recognized_boot_options_are_last_wins_and_invalid_values_are_ignored() {
        let parsed = parse_yarm_boot_options(
            b"yarm.platform=qemu-virt yarm.platform=rpi5 yarm.boot_phase=dtb \
              yarm.boot_phase=bogus yarm.max_cpus=4 yarm.max_cpus=0",
        );
        assert_eq!(parsed.platform, PlatformOption::Rpi5);
        assert_eq!(parsed.boot_phase, BootPhase::Dtb);
        assert_eq!(parsed.max_cpus, Some(4));
    }

    // Stage 159: the userspace IPC recv-v2 oracle exercise gate parses as a
    // standard bool knob, defaults to None (off), and is independent of the
    // other yarm.* knobs.
    #[test]
    fn ipc_recv_proof_knob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").ipc_recv_proof, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=1").ipc_recv_proof,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=0").ipc_recv_proof,
            Some(false)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=bogus").ipc_recv_proof,
            None
        );
        // Does not collide with the other knobs.
        let parsed = parse_yarm_boot_options(
            b"yarm.loglevel=info yarm.d6_switch_proof=1 yarm.ipc_recv_proof=true",
        );
        assert_eq!(parsed.ipc_recv_proof, Some(true));
        assert_eq!(parsed.d6_switch_proof, Some(true));
    }

    // Stage 163: the sender-wake SUB-knob parses as a standard bool knob, defaults
    // to None (off), and is independent of the base ipc_recv_proof knob — the two
    // keys must not alias (one is a prefix of the other).
    #[test]
    fn ipc_recv_proof_sender_wake_subknob_parses_and_defaults_off() {
        assert_eq!(
            parse_yarm_boot_options(b"").ipc_recv_proof_sender_wake,
            None
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_recv_proof_sender_wake=1")
                .ipc_recv_proof_sender_wake,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_recv_proof_sender_wake=0")
                .ipc_recv_proof_sender_wake,
            Some(false)
        );
        // The base knob alone must NOT set the sub-knob (no prefix aliasing).
        let base_only = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1");
        assert_eq!(base_only.ipc_recv_proof, Some(true));
        assert_eq!(base_only.ipc_recv_proof_sender_wake, None);
        // Both together parse independently.
        let both =
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_recv_proof_sender_wake=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_recv_proof_sender_wake, Some(true));
    }

    // Stage 195C: the AArch64 FutexWake live-oracle knob parses as a standard bool knob,
    // defaults to None (off), and is fully independent of every IPC-proof knob (no aliasing).
    #[test]
    fn aarch64_futex_wake_oracle_knob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").aarch64_futex_wake_oracle, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.aarch64_futex_wake_oracle=1").aarch64_futex_wake_oracle,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.aarch64_futex_wake_oracle=0").aarch64_futex_wake_oracle,
            Some(false)
        );
        // Independent of the IPC-proof knobs (no prefix aliasing in either direction).
        let ipc_only = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1");
        assert_eq!(ipc_only.aarch64_futex_wake_oracle, None);
        let oracle_only = parse_yarm_boot_options(b"yarm.aarch64_futex_wake_oracle=1");
        assert_eq!(oracle_only.ipc_recv_proof, None);
        assert_eq!(oracle_only.ipc_recv_proof_sender_wake, None);
        // Both together parse independently.
        let both =
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.aarch64_futex_wake_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.aarch64_futex_wake_oracle, Some(true));
    }

    // Stage 193B: the send-plain-oracle SUB-knob parses as a standard bool knob,
    // defaults to None (off), is independent of the base ipc_recv_proof knob, and
    // does NOT alias the sender-wake sub-knob.
    #[test]
    fn ipc_send_plain_oracle_subknob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").ipc_send_plain_oracle, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_plain_oracle=1").ipc_send_plain_oracle,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_plain_oracle=0").ipc_send_plain_oracle,
            Some(false)
        );
        // The base knob alone must NOT set the sub-knob.
        let base_only = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1");
        assert_eq!(base_only.ipc_send_plain_oracle, None);
        // The two sub-knobs are independent (no aliasing).
        let sw_only = parse_yarm_boot_options(b"yarm.ipc_recv_proof_sender_wake=1");
        assert_eq!(sw_only.ipc_send_plain_oracle, None);
        let both = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_send_plain_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_send_plain_oracle, Some(true));
        assert_eq!(both.ipc_recv_proof_sender_wake, None);
    }

    // Stage 193C: the cap-oracle SUB-knob parses as a standard bool knob, defaults
    // to None (off), and does NOT alias the plain oracle or sender-wake sub-knobs.
    #[test]
    fn ipc_send_cap_oracle_subknob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").ipc_send_cap_oracle, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_cap_oracle=1").ipc_send_cap_oracle,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_cap_oracle=0").ipc_send_cap_oracle,
            Some(false)
        );
        // No aliasing with the plain oracle sub-knob.
        let plain_only = parse_yarm_boot_options(b"yarm.ipc_send_plain_oracle=1");
        assert_eq!(plain_only.ipc_send_cap_oracle, None);
        let cap_only = parse_yarm_boot_options(b"yarm.ipc_send_cap_oracle=1");
        assert_eq!(cap_only.ipc_send_plain_oracle, None);
        let both = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_send_cap_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_send_cap_oracle, Some(true));
    }

    // Stage 193D: the reply-cap-oracle SUB-knob parses as a standard bool knob,
    // defaults to None (off), and does NOT alias the plain/cap/sender-wake sub-knobs.
    #[test]
    fn ipc_send_reply_cap_oracle_subknob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").ipc_send_reply_cap_oracle, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_reply_cap_oracle=1").ipc_send_reply_cap_oracle,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_reply_cap_oracle=0").ipc_send_reply_cap_oracle,
            Some(false)
        );
        // No aliasing with the cap oracle sub-knob (one key is not a prefix of the other).
        let cap_only = parse_yarm_boot_options(b"yarm.ipc_send_cap_oracle=1");
        assert_eq!(cap_only.ipc_send_reply_cap_oracle, None);
        let reply_only = parse_yarm_boot_options(b"yarm.ipc_send_reply_cap_oracle=1");
        assert_eq!(reply_only.ipc_send_cap_oracle, None);
        let both =
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_send_reply_cap_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_send_reply_cap_oracle, Some(true));
    }

    // Stage 193E: the enqueue-oracle SUB-knob parses as a standard bool knob, defaults
    // to None (off), and does NOT alias the reply-cap sub-knob.
    #[test]
    fn ipc_send_enqueue_oracle_subknob_parses_and_defaults_off() {
        assert_eq!(parse_yarm_boot_options(b"").ipc_send_enqueue_oracle, None);
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_enqueue_oracle=1").ipc_send_enqueue_oracle,
            Some(true)
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_enqueue_oracle=0").ipc_send_enqueue_oracle,
            Some(false)
        );
        let reply_only = parse_yarm_boot_options(b"yarm.ipc_send_reply_cap_oracle=1");
        assert_eq!(reply_only.ipc_send_enqueue_oracle, None);
        let both = parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_send_enqueue_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_send_enqueue_oracle, Some(true));
    }

    // Stage 193F: the cap-enqueue-oracle SUB-knob parses as a standard bool knob, defaults
    // to None (off), and does NOT alias the plain enqueue sub-knob (one key is a prefix).
    #[test]
    fn ipc_send_cap_enqueue_oracle_subknob_parses_and_defaults_off() {
        assert_eq!(
            parse_yarm_boot_options(b"").ipc_send_cap_enqueue_oracle,
            None
        );
        assert_eq!(
            parse_yarm_boot_options(b"yarm.ipc_send_cap_enqueue_oracle=1")
                .ipc_send_cap_enqueue_oracle,
            Some(true)
        );
        // The plain enqueue knob must NOT set the cap-enqueue knob (no prefix aliasing).
        let plain_only = parse_yarm_boot_options(b"yarm.ipc_send_enqueue_oracle=1");
        assert_eq!(plain_only.ipc_send_cap_enqueue_oracle, None);
        let cap_only = parse_yarm_boot_options(b"yarm.ipc_send_cap_enqueue_oracle=1");
        assert_eq!(cap_only.ipc_send_enqueue_oracle, None);
        let both =
            parse_yarm_boot_options(b"yarm.ipc_recv_proof=1 yarm.ipc_send_cap_enqueue_oracle=1");
        assert_eq!(both.ipc_recv_proof, Some(true));
        assert_eq!(both.ipc_send_cap_enqueue_oracle, Some(true));
    }
}
