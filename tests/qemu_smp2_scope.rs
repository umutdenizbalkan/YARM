// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2 — source guards for the AArch64 two-CPU interrupt / TLB / context wiring.
//!
//! These pin PLACEMENT and SHAPE, which no hosted test can execute: that the default path is
//! untouched without the knob, that the AP publishes its own interface bit last and is admitted
//! in one scheduler transition, that the only senders of the reschedule SGI are the committed
//! remote enqueues, that the one claim and the one completion stay where IRQ2 put them, that the
//! SGI's dispatch is the shared bridge's idle advance, that the handled-syscall resume PC is per
//! CPU, and that the witness is feature-gated and observes rather than performs. Behaviour is
//! executed by the hosted `gicv2_sgi`, `smp2_record` and scheduler admission tests; live
//! behaviour is graded by `scripts/qemu-aarch64-smp2-witness-smoke.sh`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const SMP: &str = include_str!("../src/arch/aarch64/smp.rs");
const SGI: &str = include_str!("../src/arch/gicv2_sgi.rs");
const BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const TRAP: &str = include_str!("../src/arch/aarch64/trap.rs");
const A64_MOD: &str = include_str!("../src/arch/aarch64/mod.rs");
const BRIDGE: &str = include_str!("../src/arch/trap_entry.rs");
const BOOT_ENTRY: &str = include_str!("../src/arch/boot_entry.rs");
const DRAINS: &str = include_str!("../src/kernel/ipccall_direct_txn.rs");
const PAGE_TABLE: &str = include_str!("../src/arch/aarch64/page_table.rs");
const SCHED: &str = include_str!("../src/kernel/scheduler.rs");
const WITNESS_RS: &str = include_str!("../src/arch/aarch64/smp2_witness.rs");
const WITNESS_S: &str = include_str!("../src/arch/aarch64/smp2_witness.S");
const WITNESS_SH: &str = include_str!("../scripts/qemu-aarch64-smp2-witness-smoke.sh");

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

/// Without `yarm.ap_user_dispatch=1` the AP keeps its wake-only path and the BSP brings up no
/// SGI: both entries are behind the one knob.
#[test]
fn the_default_path_is_behind_the_knob() {
    let ap = code(fn_body(
        BOOT,
        "extern \"C\" fn yarm_aarch64_secondary_cpu_boot(",
    ));
    let knob = pos(&ap, "if crate::arch::aarch64::smp::requested() {");
    assert!(ap[knob..].starts_with(
        "if crate::arch::aarch64::smp::requested() {\n        crate::arch::aarch64::smp::ap_dispatch_main(cpu);"
    ));
    assert!(
        knob < pos(&ap, "trap_kernel_state_mut()"),
        "the knob branches before the old wait"
    );
    assert!(code(SMP).contains("crate::kernel::boot::ap_user_dispatch_enabled()"));
    let bsp = code(BOOT_ENTRY);
    let bring_up = pos(
        &bsp,
        "crate::arch::aarch64::smp::bring_up_sgi_on_this_cpu(cpu)",
    );
    assert!(bsp[..bring_up].ends_with(
        "if crate::arch::aarch64::smp::requested()\n                    && let Err(reason) = "
    ));
    assert!(
        bring_up
            < pos(
                &bsp,
                "crate::arch::aarch64::irq::enable_interrupts_for_boot();"
            )
    );
}

/// The AP's order: translation, the end of the BSP's boot borrow, its own interface, the one
/// scheduler admission, then the shared idle park — and the bit is published only after every
/// readback agreed.
#[test]
fn the_ap_is_admitted_only_after_its_interface_is_published() {
    let main = code(fn_body(SMP, "pub fn ap_dispatch_main("));
    let order = [
        "enable_ap_translation();",
        "while !bsp_boot_borrow_ended() {",
        "bring_up_sgi_on_this_cpu(cpu)",
        "admit_this_ap(shared, cpu)",
        "crate::arch::aarch64::trap::enter_ap_idle(cpu)",
    ];
    let at: Vec<usize> = order.iter().map(|n| pos(&main, n)).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    let bring = code(fn_body(SMP, "pub fn bring_up_sgi_on_this_cpu("));
    assert!(
        pos(&bring, "sgi_priority_readback") < pos(&bring, ".publish(cpu.0 as usize, itargetsr0)")
    );
    let borrow = code(fn_body(
        BOOT,
        "pub fn enter_dispatched_user_task_if_available(",
    ));
    assert!(
        pos(&borrow, "note_bsp_boot_borrow_ended();")
            < pos(&borrow, "yarm_aarch64_enter_user_mode_eret("),
        "the BSP ends its boot borrow before its first EL0 entry"
    );
    let admit = code(fn_body(SCHED, "pub fn admit_pinned_dispatch("));
    assert!(
        admit.contains("self.wake_only &= !bit;")
            && admit.contains("self.balance_excluded |= bit;")
    );
    assert!(code(TRAP).contains("pub(crate) fn enter_ap_idle(cpu: CpuId) -> ! {"));
    assert!(fn_body(TRAP, "pub(crate) fn enter_ap_idle(").contains("idle_no_eret_loop();"));
}

/// The only production sender is the committed remote enqueue of the NR6/NR7 drains, after the
/// transaction returned `Ok`, and only when the target is another CPU. The controller write is
/// preceded by the barrier and never names an unpublished CPU.
#[test]
fn the_sgi_is_sent_only_for_a_committed_remote_wake() {
    let senders: Vec<&str> = [SMP, BRIDGE, BOOT, TRAP, DRAINS, BOOT_ENTRY]
        .iter()
        .flat_map(|s| s.lines())
        .filter(|l| l.contains("send_reschedule_sgi(") && !l.trim_start().starts_with("//"))
        .collect();
    assert_eq!(
        senders.len(),
        3,
        "the definition and the two drains: {senders:?}"
    );
    let drains = code(DRAINS);
    for head in [
        "fn drain_direct_request_post_work(",
        "fn drain_direct_reply_post_work(",
    ] {
        let body = code(fn_body(&drains, head));
        let ok = pos(&body, "if let Ok(success) = result {");
        let gate = pos(&body, "if success.wake_target_cpu != executing_cpu {");
        let send = pos(&body, "crate::arch::aarch64::smp::send_reschedule_sgi(");
        assert!(ok < gate && gate < send, "{head}");
    }
    let send = code(fn_body(SMP, "pub fn send_reschedule_sgi("));
    assert!(pos(&send, "TARGETS.sgir_for(") < pos(&send, "dsb ishst"));
    assert!(pos(&send, "dsb ishst") < pos(&send, "write32(dist, GICD_SGIR, sgir);"));
    assert!(code(SGI).contains("SgiRefusal::SelfTarget"));
}

/// Claim and completion stay single and in place: the SGI is counted from the claim the vector
/// entry already took, and completed by the one tail write; the bridge settles it before the
/// device route and discharges its dispatch through the existing idle drain.
#[test]
fn one_claim_one_completion_and_the_shared_idle_drain() {
    let entry = code(fn_body(BOOT, "extern \"C\" fn yarm_aarch64_vector_entry("));
    assert_eq!(entry.matches("claim_interrupt()").count(), 1);
    assert_eq!(entry.matches("complete_interrupt(ack)").count(), 1);
    assert!(pos(&entry, "note_sgi_arrival(") < pos(&entry, "complete_interrupt(ack)"));
    let smp = code(SMP);
    assert!(
        !smp.contains("GICC_EOIR") && !smp.contains("GICC_IAR"),
        "the SGI owner neither claims nor completes"
    );
    let bridge = code(BRIDGE);
    let route = pos(
        &bridge,
        "irq == crate::arch::gicv2_sgi::RESCHEDULE_SGI_INTID",
    );
    assert!(route < pos(&bridge, "settle_external_interrupt_at_bridge("));
    let arm = &bridge[route..route + 900];
    assert!(
        arm.contains("if idle_boundary_authenticated {")
            && arm.contains("sgi_idle_queue_advance = true;")
    );
    assert!(bridge.contains(
        "let timer_idle_queue_advance = timer_idle_queue_advance || sgi_idle_queue_advance;"
    ));
    assert_eq!(
        bridge.matches("if timer_idle_queue_advance {").count(),
        1,
        "one drain for both triggers"
    );
}

/// The handled-syscall resume PC is this CPU's own vector ELR. One global slot resumed a task on
/// one CPU at the ELR of an interrupt the other CPU had just taken.
#[test]
fn the_vector_elr_slot_is_per_cpu() {
    let boot = code(BOOT);
    assert!(boot.contains(
        "static LAST_VECTOR_RAW_ELR: [AtomicU64; crate::arch::platform_constants::MAX_CPUS]"
    ));
    assert!(!boot.contains("static LAST_VECTOR_RAW_ELR: AtomicU64"));
    let marker = code(fn_body(
        BOOT,
        "extern \"C\" fn yarm_aarch64_vector_elr_marker(",
    ));
    assert!(marker.contains("LAST_VECTOR_RAW_ELR.get(vector_cpu_index())"));
    let read = code(fn_body(
        BOOT,
        "pub(crate) fn last_vector_raw_elr() -> u64 {",
    ));
    assert!(read.contains("LAST_VECTOR_RAW_ELR\n        .get(vector_cpu_index())"));
}

/// The witness is feature-gated everywhere, observes, and performs none of the work it grades:
/// no claim, completion, dispatch, TLB invalidation or enqueue-by-proxy of its own beyond the AP's
/// start-up placement and kick, and its probe writes a leaf WITHOUT invalidating.
#[test]
fn the_witness_is_gated_and_observes() {
    assert!(ROOT_CARGO.contains("aarch64-smp2-witness = []"));
    assert!(A64_MOD.contains("feature = \"aarch64-smp2-witness\",\n    not(feature = \"hosted-dev\"),\n    target_arch = \"aarch64\"\n))]\npub mod smp2_witness;"));
    let w = code(WITNESS_RS);
    for banned in [
        "claim_interrupt",
        "complete_interrupt",
        "invalidate_page",
        "invalidate_asid",
        "tlbi",
        "direct_dispatch",
        "queue_advance",
        "send_reschedule_sgi",
        "commit_user_return",
    ] {
        assert!(!w.contains(banned), "witness must not call {banned}");
    }
    let probe = code(fn_body(
        PAGE_TABLE,
        "pub fn witness_repoint_without_invalidation(",
    ));
    assert!(!probe.contains("invalidate_page") && !probe.contains("tlbi"));
    for gated in [BOOT, BRIDGE, PAGE_TABLE, SMP] {
        for (i, _) in gated.match_indices("smp2_record::push(") {
            let head = &gated[..i];
            let attr = head.rfind("#[cfg(").expect("gated");
            assert!(
                head[attr..].contains("aarch64-smp2-witness"),
                "ungated record push"
            );
        }
    }
    assert!(
        !WITNESS_S.contains(" x18,") && !WITNESS_S.contains(", x18"),
        "x18 belongs to the kernel on every EL0 return"
    );
    assert!(WITNESS_SH.contains("re-derives every graded edge from the raw sealed record"));
}
