// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ2 — source guards for the AArch64 PL011 -> GICv2 external-interrupt witness.
//!
//! These are SOURCE guards: they pin structure the live witness depends on (what is feature-gated,
//! what is ordered before what, which owner does what). They are not evidence that an interrupt was
//! delivered — `scripts/qemu-aarch64-pl011-irq-witness-smoke.sh` is. The `aarch64` module is not
//! compiled on an x86_64 host, so the unit tests beside it do not run here either; these guards
//! read its source instead.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const CP_CARGO: &str = include_str!("../crates/yarm-control-plane-servers/Cargo.toml");
const BOOT_MOD: &str = include_str!("../src/kernel/boot/mod.rs");
const BOOT_ENTRY: &str = include_str!("../src/arch/boot_entry.rs");
const A64_BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const A64_IRQ: &str = include_str!("../src/arch/aarch64/irq.rs");
const A64_PT: &str = include_str!("../src/arch/aarch64/page_table.rs");
const A64_TRAP: &str = include_str!("../src/arch/aarch64/trap.rs");
const A64_MOD: &str = include_str!("../src/arch/aarch64/mod.rs");
const WITNESS: &str = include_str!("../src/arch/aarch64/pl011_irq_witness.rs");
const RV_WITNESS: &str = include_str!("../src/arch/riscv64/uart_irq_witness.rs");
const FDT: &str = include_str!("../src/arch/fdt.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const ORCH: &str = include_str!("../src/kernel/boot/orchestrator_state.rs");
const RECEIVER: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/uart_irq_witness.rs");
const SERVICE: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/service.rs");
const DRIVER: &str = include_str!("../scripts/qemu-riscv64-uart-irq-driver.py");
const GRADER: &str = include_str!("../scripts/qemu-aarch64-pl011-irq-witness-smoke.sh");

const GATE: &str = "feature = \"aarch64-pl011-irq-witness\"";

fn code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn body_after<'a>(src: &'a str, head: &str) -> &'a str {
    let rest = src
        .split_once(head)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{head} missing"));
    rest.split("\n}\n").next().unwrap_or(rest)
}

fn attr_before<'a>(src: &'a str, head: &str) -> &'a str {
    let before = src
        .split_once(head)
        .map(|(b, _)| b)
        .unwrap_or_else(|| panic!("{head} missing"));
    before.rsplit("\n}\n").next().unwrap_or(before)
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// The witness is not part of any default build, and the servers accept the feature inertly.
#[test]
fn the_witness_feature_is_declared_and_not_default() {
    assert!(ROOT_CARGO.contains("\naarch64-pl011-irq-witness = []\n"));
    assert!(CP_CARGO.contains("\naarch64-pl011-irq-witness = []\n"));
    let default = ROOT_CARGO
        .lines()
        .find(|l| l.starts_with("default = "))
        .expect("default features");
    assert!(!default.contains("aarch64-pl011-irq-witness"), "{default}");
}

/// Every piece that configures the SPI, touches a UART interrupt register, provisions the
/// receiver or widens NR 5 is compiled only with a witness feature.
#[test]
fn every_witness_entry_point_is_feature_gated() {
    assert!(attr_before(A64_MOD, "pub mod pl011_irq_witness;").contains(GATE));
    assert!(attr_before(BOOT_MOD, "pub fn provision_init_uart_irq_witness(").contains(GATE));
    assert!(attr_before(RUNTIME, "fn witness_notification_probe_into_frame(").contains(GATE));
    assert!(
        attr_before(
            ORCH,
            "unsafe fn resolve_notification_recv_cap_in_pid_from_raw("
        )
        .contains(GATE)
    );
    assert!(attr_before(SERVICE, "pub(super) mod uart_irq_witness;").contains(GATE));
    for (name, src, call) in [
        (
            "dtb capture",
            A64_BOOT,
            "pl011_irq_witness::capture_from_dtb(dtb)",
        ),
        (
            "special claim",
            A64_BOOT,
            "pl011_irq_witness::note_special_claim(",
        ),
        (
            "drain",
            A64_BOOT,
            "pl011_irq_witness::note_claim_and_drain(",
        ),
        (
            "completion",
            A64_BOOT,
            "pl011_irq_witness::note_completion_written(",
        ),
        (
            "provisioning",
            A64_BOOT,
            "pl011_irq_witness::witness_intid()",
        ),
        (
            "enable",
            BOOT_ENTRY,
            "pl011_irq_witness::enable_source_before_unmask()",
        ),
    ] {
        let at = pos(src, call);
        let window = &src[at.saturating_sub(300)..at];
        assert!(window.contains(GATE), "{name} must be feature-gated");
    }
}

/// The acknowledge token travels whole from the one claim to the one completion; dispatch sees
/// only the INTID; a special INTID returns before any completion.
#[test]
fn the_vector_entry_claims_once_and_completes_the_token_once() {
    let entry = body_after(A64_BOOT, "extern \"C\" fn yarm_aarch64_vector_entry(");
    assert_eq!(entry.matches("irq::claim_interrupt()").count(), 1);
    assert_eq!(entry.matches("irq::complete_interrupt(").count(), 1);
    assert!(entry.contains("crate::arch::aarch64::irq::complete_interrupt(ack);"));
    assert!(entry.contains("claimed.map(crate::arch::aarch64::irq::GicAck::intid)"));
    let special = pos(entry, "note_special_claim(claimed, kind);");
    let early_return = special + pos(&entry[special..], "return;");
    assert!(early_return < pos(entry, "irq::complete_interrupt("));
    // Drain precedes delivery precedes completion; the fixture's hook sits between claim and
    // dispatch, the completion note after the completion write.
    let claim = pos(entry, "irq::claim_interrupt()");
    let drain = pos(entry, "note_claim_and_drain(");
    let dispatch = pos(entry, "dispatch_trap_entry_with_shared_kernel(");
    let complete = pos(entry, "irq::complete_interrupt(ack);");
    let noted = pos(entry, "note_completion_written(ack);");
    assert!(claim < drain && drain < dispatch && dispatch < complete && complete < noted);
    // The token is the IAR value unchanged, and EOIR is written with it.
    let claim_fn = body_after(A64_IRQ, "pub fn claim_interrupt() -> Option<GicAck> {");
    assert!(claim_fn.contains("Some(GicAck(gic_read_iar("));
    let complete_fn = body_after(A64_IRQ, "pub fn complete_interrupt(ack: GicAck) {");
    assert!(complete_fn.contains("intid_is_special(ack.intid())"));
    assert!(complete_fn.contains("ack.raw()"));
    // AArch64's shared-handler acknowledgement stays inert: no second EOI, no second policy.
    let ack = body_after(A64_IRQ, "pub fn acknowledge_interrupt(irq_line: u16) {");
    assert!(!ack.contains("gic_write") && !ack.contains("EOIR"));
}

/// The fixture never claims, never completes, and never touches the CPU interface's IAR/EOIR.
#[test]
fn the_fixture_owns_no_part_of_the_interrupt_path() {
    let c = code(WITNESS);
    for forbidden in [
        "claim_interrupt(",
        "complete_interrupt(",
        "external_irq_eoi(",
        "gic_write_eoir",
        "GICC_IAR",
        "GICC_EOIR",
        "0x00c",
        "0x010,",
        "settle_external_interrupt_at_bridge",
        "deliver_external_irq",
        "irq_routes[",
    ] {
        assert!(!c.contains(forbidden), "fixture contains {forbidden}");
    }
    // It reads the route; it never writes one.
    assert!(c.contains("ipc.irq_routes.get(intid as usize).copied().flatten()"));
}

/// Enable order: readiness from the live translation, handler readiness, route bound, controller
/// configuration with readbacks, UART, source, then distributor forwarding — and the whole
/// enable runs before the PE unmask the timer bring-up already had last.
#[test]
fn the_enable_order_is_readiness_route_controller_uart_source_admission() {
    let enable = body_after(WITNESS, "pub fn enable_source_before_unmask() -> bool {");
    let order = [
        "let f = live_facts(pa);",
        "super::irq::controller_configured()",
        "return deferred(\"route_unbound\");",
        "GICD_IGROUPR",
        "GICD_IPRIORITYR",
        "GICD_ITARGETSR",
        "GICD_ICFGR",
        "write32(uart + UART_IMSC, 0);",
        "write32(uart + UART_CR, cr_before | rx_on);",
        "write32(uart + UART_ICR, UART_ICR_ALL);",
        "bit_word(dist, GICD_ICPENDR, intid)",
        "write32(uart + UART_IMSC, UART_IMSC_RXIM);",
        "bit_word(dist, GICD_ISENABLER, intid)",
        "SOURCE_ENABLED.store(true, Ordering::Release);",
    ];
    let mut last = 0;
    for step in order {
        let at = pos(enable, step);
        assert!(at >= last, "{step} is out of order");
        last = at;
    }
    let hook = pos(
        BOOT_ENTRY,
        "pl011_irq_witness::enable_source_before_unmask()",
    );
    let unmask = pos(
        BOOT_ENTRY,
        "crate::arch::aarch64::irq::enable_interrupts_for_boot();",
    );
    let gic = pos(BOOT_ENTRY, "enable_bsp_arch_timer_ppi()");
    assert!(
        gic < hook && hook < unmask,
        "GIC confirmed -> source enabled -> PE unmask"
    );
}

/// Level-triggered completion order: the drain empties the FIFO before delivery, and the disable
/// masks the device before the distributor.
#[test]
fn drain_before_completion_and_device_first_disable() {
    let drain = body_after(WITNESS, "pub fn note_claim_and_drain(");
    assert!(drain.contains("read32(uart + UART_FR) & UART_FR_RXFE == 0"));
    assert!(drain.contains("if !reachable(uart)"));
    let disable = body_after(WITNESS, "fn disable_source() {");
    assert!(pos(disable, "write32(uart + UART_IMSC, 0);") < pos(disable, "GICD_ICENABLER"));
    let note = body_after(WITNESS, "pub fn note_completion_written(ack: GicAck) {");
    assert!(note.contains("GICD_ISACTIVER") && note.contains("GICD_ISPENDR"));
    assert!(note.contains("GICC_RPR"));
}

/// The route is bound to the GIC INTID the DTB names — never to the SPI number — and the
/// specifier rule is the arch-neutral one the hosted suite executes.
#[test]
fn the_route_uses_the_intid_not_the_spi_number() {
    assert!(FDT.contains("0 if number < 988 => 32 + number,"));
    assert!(FDT.contains("1 if number < 16 => 16 + number,"));
    let capture = body_after(WITNESS, "pub fn capture_from_dtb(dtb: &[u8]) {");
    assert!(capture.contains("find_node_gic_interrupt_by_name_prefix(dtb, b\"pl011@\")"));
    assert!(capture.contains("INTID.store(irq.intid as u32"));
    let slots = pos(A64_BOOT, "pl011_irq_witness::witness_intid()");
    let prov = &A64_BOOT[slots..slots + 400];
    assert!(prov.contains("provision_init_uart_irq_witness("));
}

/// The device leaves every root carries are kernel-only, Device and execute-never at both levels,
/// and userspace still cannot map over them.
#[test]
fn the_reused_device_leaves_are_kernel_only_and_execute_never() {
    let flags = body_after(A64_PT, "fn device_leaf_flags() -> u64 {");
    assert!(flags.contains("PageFlags::DEVICE_RW"));
    assert!(flags.contains("PageTableEntry::PRIV_NO_EXECUTE"));
    let reserved = body_after(A64_PT, "fn is_reserved_device_va(va: u64) -> bool {");
    assert!(reserved.contains("page == EARLY_UART_MMIO_VA"));
    // No new mapping was introduced for the witness.
    assert!(!code(WITNESS).contains("map_page(") && !code(WITNESS).contains("install_device"));
}

/// The idle halt waits masked and takes interrupts at exactly one instruction, in a sized
/// stack-free leaf, after a fresh park.
#[test]
fn the_idle_halt_is_a_masked_wait_then_a_one_instruction_take() {
    let leaf = A64_TRAP
        .split_once("yarm_aarch64_idle_wfi_window:\n")
        .and_then(|(_, r)| r.split_once(".size yarm_aarch64_idle_wfi_window"))
        .map(|(b, _)| b)
        .expect("the leaf");
    let insns: Vec<&str> = leaf
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('.') && !l.ends_with(':'))
        .collect();
    assert_eq!(
        insns,
        ["wfi", "msr daifclr, #0x3", "msr daifset, #0x3", "ret"]
    );
    let park_loop = body_after(
        A64_TRAP,
        "extern \"C\" fn aarch64_idle_park_loop(cpu: usize) -> ! {",
    );
    assert!(
        pos(park_loop, "idle_boundary::park(cpu, sp)")
            < pos(park_loop, "yarm_aarch64_idle_wfi_window();")
    );
    assert!(!code(park_loop).contains("daifclr"));
    // The grader places idle ELR at the take point.
    assert!(GRADER.contains("(( pc == ws + 8 ))"));
    // The take point is labelled, and the vector tail returns to it MASKED — after the one
    // completion, through the arch-neutral rule the hosted suite executes.
    assert!(leaf.contains("yarm_aarch64_idle_wfi_take:\n    msr daifset, #0x3"));
    let entry = body_after(A64_BOOT, "extern \"C\" fn yarm_aarch64_vector_entry(");
    let complete = pos(entry, "irq::complete_interrupt(ack);");
    let masked = pos(entry, "idle_boundary::aarch64_take_point_return_spsr(");
    assert!(complete < masked);
    assert!(entry.contains("crate::arch::aarch64::trap::idle_take_point()"));
}

/// One receiver, one ring layout, one selector for both ports.
#[test]
fn kernel_and_receiver_agree_on_selector_ring_and_words() {
    assert!(BOOT_MOD.contains("pub const UART_IRQ_WITNESS_SELECTOR: u64 = 30;"));
    assert!(RECEIVER.contains("pub(super) const SELECTOR: u32 = 30;"));
    for (w, name) in [
        (0, "MAGIC"),
        (1, "ARMED"),
        (2, "DISABLED"),
        (3, "BYTES"),
        (4, "CLAIMS"),
        (5, "COMPLETIONS"),
        (6, "IDLE_ORIGIN"),
        (7, "USER_ORIGIN"),
        (8, "EMPTY_DRAINS"),
        (9, "BOUND_LINE"),
    ] {
        let decl = format!("pub const RING_WORD_{name}: usize = {w};");
        assert!(WITNESS.contains(&decl), "{decl}");
        assert!(RV_WITNESS.contains(&decl), "{decl}");
    }
    assert!(WITNESS.contains("pub const RING_MAGIC: u32 = 0x4952_5131;"));
    assert!(WITNESS.contains("pub const RING_BYTES_OFFSET: usize = 256;"));
    assert!(RECEIVER.contains("const RING_BYTES_OFFSET: usize = 256;"));
    // The AArch64 isolation probes name the PL011 page, which every root maps kernel-only.
    assert!(RECEIVER.contains("const PL011_BASE_VA: usize = 0x0900_0000;"));
    assert!(RECEIVER.contains("const ISOLATION_LOAD_VA: usize = PL011_BASE_VA + 0x18;"));
}

/// The receiver's blocking receive declares SIMD clobbered, and the idle-resume SIMD loss is
/// measured rather than hidden.
#[test]
fn the_receiver_does_not_depend_on_simd_across_a_blocking_receive() {
    let recv = body_after(
        RECEIVER,
        "fn recv_timeout(cap: u32, timeout: u64) -> Recv {",
    );
    let a64 = recv
        .split_once("#[cfg(target_arch = \"aarch64\")]")
        .map(|(_, r)| r)
        .expect("aarch64 arm");
    assert!(a64.contains("clobber_abi(\"C\")"));
    for v in 8..16 {
        assert!(a64.contains(&format!("out(\"v{v}\") _")), "v{v}");
    }
    assert!(RECEIVER.contains("fn park_measuring_simd("));
    assert!(RECEIVER.contains("idle_resume_simd_mask=0x{:x}"));
}

/// The host driver owns the PL011's only backend and paces by acknowledgement.
#[test]
fn the_driver_uses_a_dedicated_backend_and_acknowledgements() {
    assert!(DRIVER.contains("choices=[\"riscv64\", \"aarch64\"]"));
    assert!(DRIVER.contains("\"-cpu\", \"cortex-a72\""));
    assert!(DRIVER.contains("\"-m\", \"1024M\", \"-smp\", \"1\""));
    assert!(DRIVER.contains("socket,id=uart0,path={sock_path},server=on,wait=on"));
    assert!(DRIVER.contains(
        "IDLE_RE_AARCH64 = re.compile(rb\"TIMER_IDLE_ADVANCE_SETTLED cpu=0 incoming=none reason=idle settlement=kernel_idle\")"
    ));
    assert!(!DRIVER.contains("mux=on") && !DRIVER.contains("-nographic"));
    assert!(!DRIVER.contains("time.sleep(0.5") && !DRIVER.contains("time.sleep(1"));
}
