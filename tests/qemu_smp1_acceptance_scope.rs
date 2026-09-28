// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP1-ACCEPTANCE — source guards for PLACEMENT and WIRING only.
//!
//! Behaviour is executed elsewhere: the FP/SIMD return decision by the hosted
//! `qemu_context1_user_fpu_ownership::acc_*` cases (production entry commit, scheduler selection,
//! restores and settlement), the transaction record by `smp_request_txn::tests`, and the two
//! witnesses live. These guards keep the executed code in the places those cases assume.

const TRAPFRAME: &str = include_str!("../src/kernel/trapframe.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const X86_DT: &str = include_str!("../src/arch/x86_64/descriptor_tables.rs");
const X86_TRAP: &str = include_str!("../src/arch/x86_64/trap.rs");
const A64_BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const A64_TRAP: &str = include_str!("../src/arch/aarch64/trap.rs");
const THREAD_STATE: &str = include_str!("../src/kernel/boot/thread_state.rs");
const TASK: &str = include_str!("../src/kernel/task.rs");
const WITNESS: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/context_witness.rs");
const CTX1_SH: &str = include_str!("../scripts/qemu-context1-witness-smoke.sh");
const TXN: &str = include_str!("../src/kernel/boot/smp_request_txn.rs");
const BOOT_MOD: &str = include_str!("../src/kernel/boot/mod.rs");
const DRAIN: &str = include_str!("../src/kernel/ipccall_direct_txn.rs");
const X86_SMP: &str = include_str!("../src/arch/x86_64/smp.rs");
const UC_SH: &str = include_str!("../scripts/qemu-x86_64-ap-cross-cpu-user-consume-smoke.sh");

fn code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fn_body<'a>(src: &'a str, head: &str) -> &'a str {
    let rest = src
        .split_once(head)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{head} missing"));
    rest.split("\n}\n").next().unwrap_or(rest)
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// §1: replacing the continuation clears its owner; every production site that applies a task's
/// context names the incarnation it applied, right after the apply.
#[test]
fn every_context_apply_binds_the_incarnation_it_restored() {
    let apply = code(fn_body(TRAPFRAME, "pub fn apply_user_context("));
    let first = apply.split_once('{').map(|(_, b)| b).expect("body");
    assert!(first.trim_start().starts_with("self.resume_owner = None;"));
    for (src, head, n) in [
        (
            X86_TRAP,
            "pub(crate) fn x86_post_lock_resume_marked_incoming(",
            1,
        ),
        (
            X86_TRAP,
            "pub(crate) fn x86_apply_owner_revalidation_restore(",
            1,
        ),
        (THREAD_STATE, "fn apply_current_thread_to_frame(", 1),
        (
            A64_TRAP,
            "pub(crate) fn direct_dispatch_resume_incoming_core(",
            1,
        ),
        (A64_TRAP, "pub(crate) fn apply_restored_thread_state(", 1),
    ] {
        let body = code(fn_body(src, head));
        let applied = pos(&body, ".apply_user_context(");
        let bound = pos(&body, ".bind_resume_owner(");
        assert!(applied < bound, "{head}: bind follows apply");
        assert_eq!(body.matches(".apply_user_context(").count(), n, "{head}");
    }
    // Every x86_64/AArch64 apply in these files is one of the five above: none escapes a bind.
    for src in [X86_TRAP, A64_TRAP, THREAD_STATE] {
        let c = code(src);
        assert_eq!(
            c.matches(".apply_user_context(").count(),
            c.matches(".bind_resume_owner(").count()
        );
    }
    let facts = code(fn_body(TASK, "pub(crate) fn take_thread_restore_facts("));
    assert!(facts.contains(".and_then(|t| t.asid);"));
}

/// §1: both bridges bind the entering owner before the dispatch and settle the returning
/// continuation through the one settlement; no register/root-equality settlement remains.
#[test]
fn both_bridges_settle_through_the_continuations_owner() {
    let body = code(fn_body(X86_DT, "fn x86_trap_dispatch_body("));
    let bind = pos(&body, "trap_frame.bind_resume_owner(owner);");
    let dispatch = pos(&body, "dispatch_trap_entry_with_shared_kernel(");
    assert!(bind < dispatch);
    assert!(body.contains("return trap_frame.resume_owner();"));
    for src in [X86_DT, A64_BOOT] {
        let c = code(src);
        assert_eq!(
            c.matches("shared.settle_user_fpu_return_split(cpu, resume_owner)")
                .count(),
            1
        );
        assert!(!c.contains("kept_captured"));
        assert!(!c.contains("load_user_fpu_current_split"));
    }
    let settle = code(fn_body(
        RUNTIME,
        "pub(crate) fn settle_user_fpu_return_split(",
    ));
    assert!(settle.contains("FpuHomeRefusal::UnownedContinuation"));
    assert!(settle.contains("FpuHomeExpectation::Owner(owner)"));
    assert!(!settle.contains("UserFpuState::initial"));
    let helper = code(fn_body(RUNTIME, "fn with_current_fpu_home_split<R>("));
    // The owner check precedes the access it protects.
    assert!(pos(&helper, "FpuHomeRefusal::IncarnationReplaced") < pos(&helper, "f(tcb)"));
}

/// §3: A publishes the round generation only after its pattern is loaded, B stamps inside its own
/// patterned window, and both sides compare flag-neutrally.
#[test]
fn the_generation_is_published_inside_as_patterned_window() {
    let w = code(WITNESS);
    let a64 = w
        .split("pub unsafe fn spin_until_other_ran(")
        .nth(2)
        .expect("aarch64 spin");
    let load = pos(a64, "load_state!()");
    let publish = pos(a64, "\"str {g}, [{w}]\"");
    let compare = pos(a64, "\"eor {t}, {t}, {g}\"");
    assert!(load < publish && publish < compare);
    let x86 = w
        .split("pub unsafe fn spin_until_other_ran(")
        .nth(1)
        .expect("x86 spin");
    let restore = pos(x86, "\"fxrstor64 [{pat}]\"");
    let publish = pos(x86, "\"mov qword ptr [{w}], {g}\"");
    assert!(restore < publish && x86.contains("\"lea rcx, [rcx + {g} + 1]\""));
    assert!(!w.contains("RAN_NOT"));
    assert!(w.contains("let b_ran = b_seen == generation;"));
    for grade in [
        "B's stamp does not name this round's generation",
        "no timer preemption of A followed by B entered, B out and A resumed",
        "if t['out'] == B:",
        "ATTEMPTS = 6",
    ] {
        assert!(CTX1_SH.contains(grade), "{grade}");
    }
}

/// §4: each transaction step is recorded at its production owner, the seal is synchronous, the
/// drain's wake decision stays free of the oracle selector, and the grader reads the record.
#[test]
fn the_request_transaction_is_recorded_at_its_owners() {
    assert!(TXN.contains("crate::kernel::printk::printk_emit_sync(format_args!("));
    assert!(!code(TXN).contains("yarm_log!"));
    // QEMU-SMP1-SEAL: every successful publication attempts the report, and only there; no named
    // step reports on its own. Readiness precedes the one-time claim.
    let production = code(TXN.split("mod tests {").next().unwrap_or(TXN));
    assert_eq!(
        production.matches("attempt_seal()").count(),
        2,
        "definition + one call"
    );
    let rec = code(fn_body(TXN, "pub(crate) fn record("));
    assert!(
        pos(&rec, "if !publish(step, facts)") < pos(&rec, "if let Some(report) = attempt_seal()")
    );
    let observe = code(fn_body(TXN, "pub(crate) fn observe_user_marker("));
    assert!(!observe.contains("emit") && !observe.contains("attempt_seal"));
    let attempt = code(fn_body(TXN, "fn attempt_seal()"));
    assert!(pos(&attempt, "PUBLISHED.load(") < pos(&attempt, "SEALED.swap("));
    assert!(attempt.contains("steps[Step::Delivered as usize].1[5]"));
    for (src, needle) in [
        (BOOT_MOD, "smp_request_txn::Step::Blocked,"),
        (BOOT_MOD, "smp_request_txn::Step::Delivered,"),
        (BOOT_MOD, "smp_request_txn::Step::IpiSent,"),
        (
            X86_SMP,
            "crate::kernel::boot::smp_request_txn::Step::IpiObserved,",
        ),
        (
            X86_SMP,
            "crate::kernel::boot::smp_request_txn::Step::Resumed,",
        ),
        (TXN, "record(Step::Continued,"),
        (TXN, "record(Step::Validated,"),
    ] {
        assert!(src.contains(needle), "{needle}");
    }
    let drain = code(fn_body(
        DRAIN,
        "pub(crate) fn drain_direct_request_post_work(",
    ));
    let delivered = pos(&drain, "record_smp_request_delivery(");
    let ipi = pos(&drain, "send_reschedule_ipi_to(");
    let sent = pos(&drain, "record_smp_request_ipi_sent(");
    assert!(delivered < ipi && ipi < sent);
    assert!(!drain.contains("x86_ipccall_direct_smp_request_enabled"));
    // The grader requires the record, not the one-shot lines.
    for gone in [
        "count \"IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1\"",
        "count \"X86_AP_RESCHEDULE_IPI_RECEIVED cpu=1\"",
        "count \"X86_AP_RECV_V2_USER_VALIDATED cpu=1\"",
    ] {
        assert!(!UC_SH.contains(gone), "{gone}");
    }
    for grade in [
        "X86_SMP_REQUEST_TXN_SEAL",
        "causal order broken",
        "delivery is another transaction's",
        "no hardware 0xF1 arrival counted on cpu",
    ] {
        assert!(UC_SH.contains(grade), "{grade}");
    }
}
