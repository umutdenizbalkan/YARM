// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 — source guards for user execution-state preservation.
//!
//! These pin PLACEMENT, which no hosted test can execute: that each entry stub captures the user
//! FP/SIMD state before its first call into compiled code and restores it after its last one, that
//! the commit precedes every scheduling decision and the load follows every one, that every
//! ring-3/EL0 return installs the resuming continuation's own sanitized flags, and that the
//! witness hooks are feature-gated. Ownership BEHAVIOUR (spawn, thread, fork on both routes,
//! failed-spawn replay, slot reuse, commit/load identity) is executed by the hosted
//! `qemu_context1_user_fpu_ownership` tests; live preservation is graded by
//! `scripts/qemu-context1-witness-smoke.sh`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const CP_CARGO: &str = include_str!("../crates/yarm-control-plane-servers/Cargo.toml");
const DRIVER_CARGO: &str = include_str!("../crates/yarm-driver-servers/Cargo.toml");
const FS_CARGO: &str = include_str!("../crates/yarm-fs-servers/Cargo.toml");
const X86_DT: &str = include_str!("../src/arch/x86_64/descriptor_tables.rs");
const X86_SMP: &str = include_str!("../src/arch/x86_64/smp.rs");
const X86_WITNESS: &str = include_str!("../src/arch/x86_64/context_witness.rs");
const A64_BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const A64_TRAP: &str = include_str!("../src/arch/aarch64/trap.rs");
const A64_WITNESS: &str = include_str!("../src/arch/aarch64/context_witness.rs");
const USER_FPU: &str = include_str!("../src/kernel/user_fpu.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const FORK_OWNERS: &str = include_str!("../src/kernel/boot/fork_owners.rs");
const EXEC_STATE: &str = include_str!("../src/kernel/boot/exec_state.rs");
const THREAD_CORE: &str = include_str!("../src/kernel/boot/spawn_thread_core.rs");
const RESERVATION: &str = include_str!("../src/kernel/spawn_reservation.rs");
const TASK: &str = include_str!("../src/kernel/task.rs");
const TRAPFRAME: &str = include_str!("../src/kernel/trapframe.rs");
const USER_WITNESS: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/context_witness.rs");
const GRADER: &str = include_str!("../scripts/qemu-context1-witness-smoke.sh");

fn code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn after<'a>(src: &'a str, head: &str) -> &'a str {
    src.split_once(head)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{head} missing"))
}

fn fn_body<'a>(src: &'a str, head: &str) -> &'a str {
    let rest = after(src, head);
    rest.split("\n}\n").next().unwrap_or(rest)
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

fn rpos(src: &str, needle: &str) -> usize {
    src.rfind(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// One asm routine: from its label to the next `.global`/end of the template.
fn asm_routine<'a>(src: &'a str, label: &str) -> &'a str {
    let rest = after(src, &format!("\n{label}:\n"));
    let end = [rest.find("\n    .global"), rest.find("\"#")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_witness_features_are_declared_and_not_default() {
    for cargo in [ROOT_CARGO, CP_CARGO, DRIVER_CARGO, FS_CARGO] {
        assert!(cargo.contains("\ncontext1-witness = []\n"));
        assert!(cargo.contains("\ncontext1-clobber = [\"context1-witness\"]\n"));
    }
    let default = ROOT_CARGO
        .split("\ndefault = ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("default features");
    assert!(!default.contains("context1"));
}

/// x86_64: both entry stubs capture before the first call and restore after it, and give the
/// kernel its own control environment in between.
#[test]
fn x86_entry_stubs_capture_first_and_restore_last() {
    for label in ["yarm_x86_common_trap_entry", "yarm_x86_lstar_entry"] {
        let r = code(asm_routine(X86_DT, label));
        let save = pos(&r, "fxsave64 [rsp]");
        let call = pos(&r, "call yarm_x86_dispatch_trap_from_stub");
        let calls = r
            .lines()
            .filter(|l| l.trim_start().starts_with("call "))
            .count();
        assert_eq!(calls, 1, "{label}: one call into compiled code");
        assert!(save < call, "{label}: capture precedes the call");
        for env in [
            "fninit",
            "ldmxcsr [rip + YARM_X86_KERNEL_MXCSR]",
            "cld",
            "mov r8, rsp",
        ] {
            let at = pos(&r, env);
            assert!(
                save < at && at < call,
                "{label}: {env} sits between capture and call"
            );
        }
        let restore = pos(&r, "fxrstor64 [rsp]");
        assert!(call < restore, "{label}: restore follows the call");
        assert!(
            restore < pos(&r, "mov rsp, r12"),
            "{label}: restore uses the area in place"
        );
        assert!(pos(&r, "sub rsp, 512") < save);
    }
    assert!(
        X86_DT.contains(
            "static YARM_X86_KERNEL_MXCSR: u32 = crate::kernel::user_fpu::X86_MXCSR_INIT;"
        )
    );
}

/// x86_64: the wrapper commits before the dispatch body and loads after it; the witness clobber
/// is gated, runs last, and neither saves nor restores user state.
#[test]
fn x86_commit_precedes_and_load_follows_the_dispatch() {
    let w = code(fn_body(
        X86_DT,
        "#[cfg(all(not(feature = \"hosted-dev\"), target_arch = \"x86_64\"))]\n#[unsafe(no_mangle)]\nextern \"C\" fn yarm_x86_dispatch_trap_from_stub(",
    ));
    let commit = pos(&w, "user_fpu_commit_on_entry(fpu_area);");
    let body = pos(
        &w,
        "x86_trap_dispatch_body(vector, error_code, regs, interrupt_frame);",
    );
    let load = pos(&w, "user_fpu_load_for_return(fpu_area);");
    assert!(commit < body && body < load);
    let clobber = pos(&w, "clobber_user_visible_state();");
    assert!(load < clobber);
    assert!(w[..clobber].contains("#[cfg(feature = \"context1-clobber\")]"));
    let c = code(fn_body(X86_WITNESS, "pub fn clobber_user_visible_state()"));
    assert!(!c.contains("fxsave") && !c.contains("user_fpu") && !c.contains("commit"));
}

/// x86_64: every other way into ring 3 installs a defined state — the initial image at first
/// entry, the resuming task's home on the saved-frame (D6/AP) resumes.
#[test]
fn x86_first_entry_and_saved_frame_resumes_install_a_home() {
    assert!(
        code(asm_routine(X86_DT, "yarm_x86_enter_ring3"))
            .trim_start()
            .starts_with("fxrstor64 [rip + YARM_X86_INITIAL_FXSAVE]")
    );
    assert!(
        code(asm_routine(X86_DT, "yarm_x86_resume_ring3"))
            .trim_start()
            .starts_with("fxrstor64 [rsi]")
    );
    assert_eq!(
        X86_SMP
            .matches(".unwrap_or_else(crate::kernel::user_fpu::UserFpuState::initial);")
            .count(),
        2
    );
    assert_eq!(X86_SMP.matches(".load_user_fpu_split(").count(), 2);
    assert_eq!(
        X86_SMP
            .matches("resume_user_mode_iret(&frame, &fpu)")
            .count(),
        2
    );
}

/// x86_64: RFLAGS on every ring-3 return is the resuming continuation's own sanitized word.
#[test]
fn x86_ring3_returns_install_the_resuming_tasks_flags() {
    let flush = code(fn_body(
        X86_DT,
        "unsafe fn flush_trap_context_to_iret_frame(",
    ));
    assert_eq!(
        flush
            .matches("crate::kernel::user_fpu::sanitize_user_rflags(trap_frame.user_status as u64)")
            .count(),
        2,
        "the ring-3 tail and the idle-boundary conversion"
    );
    assert!(!flush.contains("0x202"));
    assert_eq!(
        X86_SMP
            .matches("rflags: crate::kernel::user_fpu::sanitize_user_rflags(user_status),")
            .count(),
        2
    );
    assert!(X86_DT.contains("trap.user_status = frame.rflags as usize;"));
}

/// AArch64: the vector dispatch saves q0..q31/FPCR/FPSR/TPIDR_EL0 before its first `bl` and
/// restores them after its last one.
#[test]
fn aarch64_vector_dispatch_captures_first_and_restores_last() {
    let r = code(asm_routine(A64_BOOT, "yarm_aarch64_vector_dispatch"));
    assert!(r.contains("sub sp, sp, #832") && r.contains("add sp, sp, #848"));
    let first_bl = pos(&r, "bl ");
    let last_bl = rpos(&r, "bl ");
    for save in [
        "stp q0, q1, [sp, #288]",
        "stp q30, q31, [sp, #768]",
        "mrs x9, fpcr",
        "mrs x9, fpsr",
        "mrs x9, tpidr_el0",
        "msr fpcr, xzr",
    ] {
        assert!(pos(&r, save) < first_bl, "{save} precedes the first bl");
    }
    for restore in [
        "ldp q0, q1, [sp, #288]",
        "ldp q30, q31, [sp, #768]",
        "msr fpcr, x9",
        "msr fpsr, x9",
        "msr tpidr_el0, x9",
    ] {
        assert!(pos(&r, restore) > last_bl, "{restore} follows the last bl");
        assert!(pos(&r, restore) < pos(&r, "eret"));
    }
    assert!(
        A64_BOOT
            .contains("const _: () = assert!(core::mem::size_of::<Aarch64VectorFrame>() == 832);")
    );
}

/// AArch64: commit precedes the shared dispatch; load follows the take-point SPSR rewrite, the
/// last decision about where this exception returns; the clobber is gated and sits between.
#[test]
fn aarch64_commit_precedes_and_load_follows_the_dispatch() {
    let e = code(fn_body(
        A64_BOOT,
        "extern \"C\" fn yarm_aarch64_vector_entry(",
    ));
    let commit = pos(&e, "user_fpu_commit_on_entry(frame, trap_cpu);");
    let dispatch = pos(&e, "dispatch_trap_entry_with_shared_kernel(");
    let take = pos(&e, "aarch64_take_point_return_spsr(");
    let load = pos(&e, "user_fpu_load_for_return(frame, trap_cpu);");
    assert!(commit < dispatch && dispatch < take && take < load);
    let clobber = pos(&e, "clobber_user_visible_state();");
    assert!(dispatch < clobber && clobber < load);
    let c = code(fn_body(A64_WITNESS, "pub fn clobber_user_visible_state()"));
    assert!(!c.contains("user_fpu") && !c.contains("frame") && !c.contains("commit"));
}

/// AArch64: an EL0 return hands back NZCV from the resuming continuation and nothing else.
#[test]
fn aarch64_el0_returns_install_the_resuming_tasks_nzcv() {
    let wb = code(fn_body(
        A64_BOOT,
        "fn write_trapframe_back_to_vector_frame(",
    ));
    assert_eq!(
        wb.matches("crate::kernel::user_fpu::sanitize_user_spsr(trap_frame.user_status as u64)")
            .count(),
        2,
        "the ordinary EL0t write-back and the idle-boundary conversion"
    );
    assert_eq!(
        code(A64_BOOT)
            .matches("trap_frame.user_status = frame.spsr_el1 as usize;")
            .count(),
        2
    );
}

/// AArch64: the post-lock resume core mirrors argument lanes under the same predicate as the
/// in-lock owner — never over an asynchronously interrupted task's live registers.
#[test]
fn aarch64_post_lock_argument_mirror_is_gated() {
    let core = code(fn_body(
        A64_TRAP,
        "pub(crate) fn direct_dispatch_resume_incoming_core(",
    ));
    let gate = pos(
        &core,
        "if context.argument_lanes_are_authoritative(completion_encoded) {",
    );
    let mirror = pos(&core, "frame.set_user_gpr(REG_X0, frame.arg(0));");
    assert!(gate < mirror);
    assert_eq!(core.matches("completion_encoded = true;").count(), 2);
    let inlock = code(fn_body(
        A64_TRAP,
        "pub(crate) fn apply_restored_thread_state(",
    ));
    assert!(inlock.contains(".argument_lanes_are_authoritative(completion_encoded)"));
}

/// Ownership sites: every TCB starts initial; threads get their own TLS; spawned images reset;
/// fork copies on BOTH publication owners; a failed spawn replays the home.
#[test]
fn every_home_writer_follows_the_ownership_rules() {
    assert!(TASK.contains("user_fpu: crate::kernel::user_fpu::UserFpuState::initial(),"));
    assert!(THREAD_CORE.contains("tcb.user_fpu = crate::kernel::user_fpu::UserFpuState::initial_for_thread(args.tls_base as u64);")
        || THREAD_CORE.contains("UserFpuState::initial_for_thread(args.tls_base as u64)"));
    assert!(
        EXEC_STATE.contains("tcb.user_fpu = crate::kernel::user_fpu::UserFpuState::initial();")
    );
    assert!(FORK_OWNERS.contains("tcb.user_fpu = publication.user_fpu;"));
    let split = fn_body(RUNTIME, "pub(crate) fn publish_forked_child_split(");
    assert!(split.contains("tcb.user_fpu = publication.user_fpu;"));
    assert!(
        RESERVATION.contains("user_fpu: tcb.user_fpu,")
            && RESERVATION.contains("tcb.user_fpu = self.user_fpu;")
    );
    // The flags word travels with the GPRs through the one capture/apply pair.
    assert!(TRAPFRAME.contains("user_status: self.user_status,"));
    assert!(TRAPFRAME.contains("self.user_status = context.user_status;"));
    // No lazy scheme: nothing traps FP use to decide ownership later.
    for lazy in ["CR0.TS", "clts", "CPACR_EL1.FPEN = 0", "fpu_owner"] {
        assert!(!code(USER_FPU).contains(lazy));
    }
}

/// The witness is private to init, uses no new syscall, and the grader demands identities.
#[test]
fn the_witness_uses_existing_syscalls_and_the_grader_checks_identities() {
    let w = code(USER_WITNESS);
    assert!(w.contains("const NR_IPC_RECV_TIMEOUT: usize = 5;"));
    assert!(w.contains("spawn_thread(") && w.contains("exit_current_task()"));
    for cell in [
        "cell=fresh",
        "cell=block",
        "cell=preempt",
        "cell=same",
        "cell=preempt_b",
    ] {
        assert!(w.contains(cell));
        assert!(GRADER.contains(cell.trim_start_matches("cell=")));
    }
    assert!(GRADER.contains("t['in'] == A and t['out'] == B and t['timer'] == '1'"));
    assert!(GRADER.contains("{'spin': 'timer', 'block': 'syscall'}"));
    assert!(GRADER.contains("SCHED_ENTER_IDLE_HLT"));
    assert!(GRADER.contains("CTX1_KERNEL_ENV_BAD"));
}
