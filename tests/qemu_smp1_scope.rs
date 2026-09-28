// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP1 — source guards for x86_64 cross-CPU interrupts, TLB acknowledgement, context
//! identity and reply progress.
//!
//! These pin PLACEMENT and SHAPE, which no hosted test can execute: that the BSP takes 0xF1
//! through the same pure-asm stub as the APs, that the ACK wait answers its own CPU's mailbox in
//! the stub's order, that every FP/SIMD-home access is authenticated against the incarnation
//! current on the CPU, that the retired oracle BSP resume stays retired, and that the witness is
//! feature-gated and observes rather than performs. Identity BEHAVIOUR is executed by the hosted
//! `qemu_context1_user_fpu_ownership` and `u3_ap_saved_context_snapshot` tests; live behaviour
//! is graded by `scripts/qemu-x86_64-smp1-witness-smoke.sh` and
//! `scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const X86_DT: &str = include_str!("../src/arch/x86_64/descriptor_tables.rs");
const X86_SMP: &str = include_str!("../src/arch/x86_64/smp.rs");
const X86_PERCPU: &str = include_str!("../src/arch/x86_64/percpu.rs");
const X86_MOD: &str = include_str!("../src/arch/x86_64/mod.rs");
const WITNESS_RS: &str = include_str!("../src/arch/x86_64/smp1_witness.rs");
const WITNESS_S: &str = include_str!("../src/arch/x86_64/smp1_witness.S");
const A64_BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const VM_TXN: &str = include_str!("../src/kernel/syscall/vm_txn.rs");
const EXEC_STATE: &str = include_str!("../src/kernel/boot/exec_state.rs");
const BOOT_MOD: &str = include_str!("../src/kernel/boot/mod.rs");
const REPLY_SH: &str = include_str!("../scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh");
const WITNESS_SH: &str = include_str!("../scripts/qemu-x86_64-smp1-witness-smoke.sh");

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

/// §1: the BSP's 0xF1 gate is the pure-asm stub, installed after the generic loop, and the
/// compiled dispatch has no 0xF1 branch left.
#[test]
fn the_bsp_takes_0xf1_through_the_asm_stub() {
    let populate = code(fn_body(X86_DT, "fn populate_boot_idt_from_stubs("));
    let generic = pos(&populate, "while i < IDT_ENTRIES {");
    let gate = pos(&populate, "idt_ptr.add(AP_REMOTE_WAKE_VECTOR as usize)");
    assert!(generic < gate, "the stub overrides the generic entry");
    assert!(populate[gate..].contains("yarm_ap_remote_wake_stub"));
    assert!(!X86_DT.contains("vector as usize == AP_REMOTE_WAKE_VECTOR as usize"));
    assert!(!X86_SMP.contains("c2c_bsp_handle_reschedule_ipi"));
}

/// §1: the stub counts each arrival by the privilege level it interrupted, before the mailbox,
/// and still ACKs last.
#[test]
fn the_stub_counts_arrivals_by_origin_and_acks_last() {
    let stub = X86_DT
        .split("\nyarm_ap_remote_wake_stub:\n")
        .nth(1)
        .expect("stub");
    let stub = &stub[..stub.find("\n    iretq").expect("iretq")];
    let kernel = pos(stub, "add dword ptr gs:[{wake_kernel_off}], 1");
    let user = pos(stub, "add dword ptr gs:[{wake_user_off}], 1");
    let req = pos(stub, "mov ecx, dword ptr gs:[{tlb_req_gen_off}]");
    let ack = pos(stub, "mov dword ptr gs:[{tlb_ack_gen_off}], ecx");
    assert!(kernel < req && user < req && req < ack);
    assert!(!stub.contains("call ") && !stub.contains("fxsave") && !stub.contains("fxrstor"));
    assert!(X86_PERCPU.contains("pub const WAKE_ORIGIN_KERNEL_COUNT_OFFSET: usize = 176;"));
    assert!(X86_PERCPU.contains("pub const WAKE_ORIGIN_USER_COUNT_OFFSET: usize = 180;"));
}

/// §5: the ACK wait answers this CPU's own mailbox, and the answer mirrors the stub's order:
/// generation, then VA, invalidate, origin, ACK last.
#[test]
fn the_ack_wait_answers_its_own_mailbox_in_the_stub_order() {
    let shoot = code(fn_body(X86_SMP, "pub fn smp_tlb_shootdown_cpus("));
    let me = pos(
        &shoot,
        "let me = super::descriptor_tables::current_cpu_id();",
    );
    let poll = pos(&shoot, "if super::percpu::tlb_ack_gen(cpu) == want {");
    let own = pos(&shoot, "super::percpu::service_own_tlb_request(me)");
    let relax = shoot[own..]
        .find("cpu_relax();")
        .map(|r| own + r)
        .expect("relax");
    assert!(me < poll && poll < own && own < relax);
    let body = code(fn_body(X86_PERCPU, "pub fn service_own_tlb_request("));
    let gen_read = pos(&body, "addr_of!((*base).tlb_req_gen)");
    let va_read = pos(&body, "addr_of!((*base).tlb_req_va)");
    let pending = pos(&body, "addr_of!((*base).tlb_ack_gen)) {");
    let inval = pos(&body, "invlpg [{va}]");
    let origin = pos(&body, "TLB_ACK_ORIGIN_KERNEL");
    let ack = pos(&body, "addr_of_mut!((*base).tlb_ack_gen), generation)");
    assert!(gen_read < va_read && va_read < pending && pending < inval);
    assert!(inval < origin && origin < ack);
    assert!(
        body.contains("mov cr3, {t}"),
        "VA 0 is the full-flush convention"
    );
}

/// §2: every FP/SIMD-home access is one authenticated transaction; no numeric-TID accessor
/// and no initial-image substitution remain.
#[test]
fn fp_homes_are_authenticated_against_the_current_incarnation() {
    let helper = code(fn_body(RUNTIME, "fn with_current_fpu_home_split<R>("));
    let rank1 = pos(&helper, "self.with_scheduler_split_mut(");
    let rank2 = pos(&helper, "self.with_task_tcbs_split_mut(");
    assert!(rank1 < rank2);
    for check in [
        "current_tid_on(cpu)",
        "CurrentElsewhere",
        "tcb.asid",
        "tcb.status",
    ] {
        assert!(helper.contains(check), "{check}");
    }
    assert!(!RUNTIME.contains("fn commit_user_fpu_split("));
    assert!(!RUNTIME.contains("fn load_user_fpu_split("));
    assert!(!RUNTIME.contains("fn ap_saved_resume_context_split("));
    for (src, port) in [(X86_DT, "x86_64"), (A64_BOOT, "aarch64")] {
        let c = code(src);
        assert_eq!(
            c.matches(".commit_user_fpu_current_split(").count(),
            1,
            "{port}"
        );
        assert_eq!(
            c.matches(".load_user_fpu_current_split(").count(),
            1,
            "{port}"
        );
        assert!(
            c.contains("action=kept_captured") && c.contains("action=fatal"),
            "{port}"
        );
    }
    let resume = code(fn_body(X86_SMP, "fn ap_saved_frame_resume("));
    assert_eq!(
        resume
            .matches("shared.ap_saved_resume_current_split(cpu, tid)")
            .count(),
        1
    );
    assert!(resume.contains("let fpu = authenticated.user_fpu;"));
    assert!(!X86_SMP.contains("UserFpuState::initial"));
}

/// §4: the oracle's second selection owner stays retired, and so do the probe parks.
#[test]
fn the_reply_profile_has_one_selection_owner_and_no_probe_park() {
    for gone in [
        "c2c_bsp_saved_frame_resume",
        "C2C_BSP_RESCHEDULE_PENDING",
        "clear_trap_dispatch_depth",
    ] {
        assert!(!X86_SMP.contains(gone) && !X86_DT.contains(gone), "{gone}");
    }
    assert!(EXEC_STATE.contains("const RECV_V2_SERVER_STUB_C2C: [u8; 362]"));
    assert!(
        EXEC_STATE.contains("X86_AP_DUPLICATE_REPLY_REFUSED_OBSERVED cpu=1 err=InvalidCapability")
    );
    assert!(EXEC_STATE.contains("X86_BSP_CLIENT_PROGRESS cpu=0 step=post_reply_yield result=ok"));
    for required in [
        "[[ \"$(count \"X86_BSP_SAVED_DISPATCH_OK\")\" == \"0\" ]]",
        "no production selection of the caller on cpu 0 between the claim and its continuation",
        "D2_RECV_GENUINE_DISPATCH_DONE result=switch cpu=0 incoming=${CLIENT_TID}",
        "cpu0 0xF1 arrivals != 1",
        "server did not block again on cpu 1",
        "client did not block again on cpu 0",
    ] {
        assert!(REPLY_SH.contains(required), "{required}");
    }
    assert!(BOOT_MOD.contains("fn maybe_echo_smp_oracle_block("));
}

/// §3/§5: the witness is a default-off feature and a default-off knob, observes the VM owner
/// only on its one page, and every graded line is synchronous.
#[test]
fn the_witness_is_gated_and_observes() {
    assert!(ROOT_CARGO.contains("\nx86-smp1-witness = [\"x86-ipccall-direct-smp-oracle\"]\n"));
    let default = ROOT_CARGO
        .split("\ndefault = ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("default features");
    assert!(!default.contains("smp1"));
    assert!(X86_MOD.contains(
        "#[cfg(all(feature = \"x86-smp1-witness\", not(feature = \"hosted-dev\")))]\npub mod smp1_witness;"
    ));
    let txn = code(fn_body(VM_TXN, "pub(crate) fn run_vm_map_transaction<"));
    assert_eq!(
        txn.matches("crate::arch::x86_64::smp1_witness::watches(")
            .count(),
        1
    );
    assert_eq!(txn.matches("feature = \"x86-smp1-witness\"").count(), 4);
    // The witness performs no shootdown and no ACK: its only kernel mutation is the probe.
    let w = code(WITNESS_RS);
    for forbidden in [
        "tlb_request_shootdown",
        "service_own_tlb_request",
        "write_icr",
        "settle_displaced",
    ] {
        assert!(!w.contains(forbidden), "{forbidden}");
    }
    assert_eq!(w.matches("page_table::map_page(").count(), 1);
    assert!(
        !w.contains("yarm_log!"),
        "every witness line is synchronous"
    );
    // The programs use only the production syscalls: NR 2/3/6/7/13/15 and Yield.
    assert!(!WITNESS_S.contains("0xA9C6") && !WITNESS_S.contains("0xa9c6"));
    for grade in [
        "ACK generation != request generation",
        "ACK before the displaced backing is settled",
        "the resident target's verdict follows the ACK",
        "contention not exercised",
    ] {
        assert!(WITNESS_SH.contains(grade), "{grade}");
    }
}
