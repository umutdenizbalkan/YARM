// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ3 — source guards for the x86_64 COM1 -> I/O APIC -> LAPIC external-interrupt witness.
//!
//! These are SOURCE guards: they pin structure the live witness depends on (what is feature-gated,
//! what is ordered before what, which owner does what). They are not evidence that an interrupt was
//! delivered — `scripts/qemu-x86_64-uart-irq-witness-smoke.sh` is. The MADT rule and the two
//! descriptor-table predicates are executed by the hosted unit tests beside them.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const CP_CARGO: &str = include_str!("../crates/yarm-control-plane-servers/Cargo.toml");
const DRIVER_CARGO: &str = include_str!("../crates/yarm-driver-servers/Cargo.toml");
const FS_CARGO: &str = include_str!("../crates/yarm-fs-servers/Cargo.toml");
const BOOT_MOD: &str = include_str!("../src/kernel/boot/mod.rs");
const BOOT_ENTRY: &str = include_str!("../src/arch/boot_entry.rs");
const X86_BOOT: &str = include_str!("../src/arch/x86_64/boot.rs");
const X86_IRQ: &str = include_str!("../src/arch/x86_64/irq.rs");
const X86_DT: &str = include_str!("../src/arch/x86_64/descriptor_tables.rs");
const X86_MOD: &str = include_str!("../src/arch/x86_64/mod.rs");
const ARCH_MOD: &str = include_str!("../src/arch/mod.rs");
const WITNESS: &str = include_str!("../src/arch/x86_64/uart_irq_witness.rs");
const RV_WITNESS: &str = include_str!("../src/arch/riscv64/uart_irq_witness.rs");
const MADT: &str = include_str!("../src/arch/acpi_madt_rule.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const ORCH: &str = include_str!("../src/kernel/boot/orchestrator_state.rs");
const RECEIVER: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/uart_irq_witness.rs");
const SERVICE: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/service.rs");
const DRIVER: &str = include_str!("../scripts/qemu-riscv64-uart-irq-driver.py");
const GRADER: &str = include_str!("../scripts/qemu-x86_64-uart-irq-witness-smoke.sh");

const GATE: &str = "feature = \"x86_64-uart-irq-witness\"";

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
    for cargo in [ROOT_CARGO, CP_CARGO, DRIVER_CARGO, FS_CARGO] {
        assert!(cargo.contains("\nx86_64-uart-irq-witness = []\n"));
    }
    let default = ROOT_CARGO
        .lines()
        .find(|l| l.starts_with("default = "))
        .expect("default features");
    assert!(!default.contains("x86_64-uart-irq-witness"), "{default}");
}

/// Every piece that touches COM1, the I/O APIC, provisioning or the receiver is compiled only with
/// the witness feature. The production owners it observes are not.
#[test]
fn every_witness_entry_point_is_feature_gated() {
    assert!(attr_before(X86_MOD, "pub mod uart_irq_witness;").contains(GATE));
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
    assert!(attr_before(X86_IRQ, "pub fn lapic_eoi_writes() -> u32 {").contains(GATE));
    assert!(attr_before(X86_IRQ, "pub fn lapic_configured() -> bool {").contains(GATE));
    assert!(attr_before(X86_DT, "pub(crate) fn boot_tss_io_map_facts()").contains(GATE));
    for (name, src, call) in [
        ("rsdp capture", X86_BOOT, "uart_irq_witness::capture_rsdp("),
        (
            "route + provisioning",
            X86_BOOT,
            "uart_irq_witness::derive_route()",
        ),
        ("enable", BOOT_ENTRY, "uart_irq_witness::enable_source()"),
        (
            "entry hook",
            X86_DT,
            "uart_irq_witness::note_entry_and_drain(",
        ),
        (
            "completion hook",
            X86_DT,
            "uart_irq_witness::note_after_dispatch(vector)",
        ),
    ] {
        let at = pos(src, call);
        let window = &src[at.saturating_sub(400)..at];
        assert!(window.contains(GATE), "{name} must be feature-gated");
    }
    // The EOI counter is an observation added AFTER the owner's one write, under the gate.
    let ack = body_after(X86_IRQ, "pub fn acknowledge_interrupt(_irq_line: u16) {");
    assert_eq!(ack.matches("lapic_write_eoi(").count(), 1);
    assert!(pos(ack, "lapic_write_eoi(") < pos(ack, "LAPIC_EOI_WRITES.fetch_add(1"));
    assert!(pos(ack, GATE) < pos(ack, "LAPIC_EOI_WRITES.fetch_add(1"));
}

/// The fixture never writes an EOI, never delivers and never binds a route. Device
/// acknowledgement (RBR) is its; controller completion (LAPIC EOI) is the production bridge's.
#[test]
fn the_fixture_owns_no_part_of_the_interrupt_path() {
    let c = code(WITNESS);
    for forbidden in [
        "acknowledge_interrupt(",
        "lapic_write_eoi",
        "0xB0",
        "0xb0",
        "settle_external_interrupt_at_bridge(",
        "deliver_external_irq",
        "send_irq(",
        "irq_routes[",
        "dispatch_trap_entry",
        "0x20, 0x20",
        "outb(0x20",
        "outb(0xA0",
    ] {
        assert!(!c.contains(forbidden), "fixture contains {forbidden}");
    }
    // It reads the route; it never writes one.
    assert!(c.contains("ipc.irq_routes.get(gsi as usize).copied().flatten()"));
    // Both hooks bracket the ONE production dispatch in the shared path.
    let entry = pos(X86_DT, "uart_irq_witness::note_entry_and_drain(");
    let dispatch = entry
        + pos(
            &X86_DT[entry..],
            "crate::arch::trap_entry::dispatch_trap_entry_with_shared_kernel(",
        );
    let after = pos(X86_DT, "uart_irq_witness::note_after_dispatch(vector)");
    assert!(entry < dispatch && dispatch < after);
}

/// The route is derived, not assumed: the MADT's ISA override rule, the I/O APIC chosen by GSI
/// base, vector 0x20 + GSI, and a refusal for anything the production decoder cannot name.
#[test]
fn the_route_is_derived_from_the_madt_and_the_decoder() {
    let derive = body_after(WITNESS, "pub fn derive_route() -> Option<u16> {");
    assert!(derive.contains("acpi_madt_rule::isa_route(madt, COM1_ISA_IRQ)"));
    assert!(derive.contains("acpi_madt_rule::ioapic_for_gsi(madt, route.gsi)"));
    assert!(derive.contains("VEC_EXTERNAL_BASE + route.gsi"));
    assert!(derive.contains("gsi_outside_decoder_lines"));
    assert!(derive.contains("ioapic_not_at_mapped_window"));
    // The rule is arch-neutral and hosted-tested; ISO type 2 wins over the ISA default.
    assert!(MADT.contains("const MADT_TYPE_ISO: u8 = 2;"));
    assert!(MADT.contains("fn irq4_has_no_override_and_takes_the_isa_defaults()"));
    assert!(ARCH_MOD.contains("pub mod acpi_madt_rule;"));
    // The line provisioned is the line derive_route returned.
    let prov = &X86_BOOT[pos(X86_BOOT, "uart_irq_witness::derive_route()")..];
    let prov = &prov[..pos(prov, "IRQ3_WITNESS_SLOTS")];
    assert!(prov.contains("Some(line) =>"));
    assert!(prov.contains("init_asid,\n                    line,"));
}

/// Enable order: handler/access readiness, route bound, redirection written masked and read back,
/// device configured, source enabled, redirection unmasked — all with IF clear, and the CPU's
/// admission restored only after.
#[test]
fn the_enable_order_is_readiness_route_controller_device_source_admission() {
    let inner = body_after(
        WITNESS,
        "fn enable_source_interrupts_off(if_at_call: bool) -> bool {",
    );
    let order = [
        "lapic_configured()",
        "let f = page_facts(va);",
        "return deferred(\"pic_route_for_irq_open\");",
        "return deferred(\"route_unbound\");",
        "ioapic_write(redtbl, low | REDIR_MASKED);",
        "return deferred(\"redirection_readback_disagrees\");",
        "outb(COM1 + UART_IER, 0);",
        "outb(COM1 + UART_MCR, mcr_before | UART_MCR_OUT2);",
        "outb(COM1 + UART_IER, UART_IER_ERBFI);",
        "ioapic_write(redtbl, low);",
        "SOURCE_ENABLED.store(true, Ordering::Release);",
    ];
    let mut last = 0;
    for step in order {
        let at = pos(inner, step);
        assert!(at >= last, "{step} is out of order");
        last = at;
    }
    let outer = body_after(WITNESS, "pub fn enable_source() -> bool {");
    assert!(pos(outer, "\"cli\"") < pos(outer, "enable_source_interrupts_off(was_enabled)"));
    assert!(pos(outer, "enable_source_interrupts_off(was_enabled)") < pos(outer, "\"sti\""));
    // The enable runs after the tick is armed, from the timer bring-up.
    let timer = pos(BOOT_ENTRY, "X86_BOOTSTRAP_TIMER_STARTED");
    let hook = pos(BOOT_ENTRY, "uart_irq_witness::enable_source()");
    assert!(timer < hook);
}

/// The device cause is serviced before ANY console output in the handler (the console is the
/// same UART), and the disable masks the device before the controller.
#[test]
fn drain_before_output_and_device_first_disable() {
    let entry = body_after(WITNESS, "pub fn note_entry_and_drain(");
    let drain = pos(entry, "inb(COM1 + UART_RBR)");
    let first_log = pos(entry, "crate::yarm_log!(");
    assert!(
        drain < first_log,
        "the RX cause must be withdrawn before the first log line"
    );
    assert!(entry.contains("self_irr_after_drain"));
    let disable = body_after(WITNESS, "fn disable_source() {");
    assert!(pos(disable, "outb(COM1 + UART_IER, 0);") < pos(disable, "REDIR_MASKED"));
    let after = body_after(WITNESS, "pub fn note_after_dispatch(vector: u64) {");
    assert!(after.contains("lapic_eoi_writes()"));
    assert!(after.contains("eoi_writes == 1"));
    assert!(after.contains("REDIR_REMOTE_IRR | REDIR_DELIVERY_PENDING"));
}

/// No new mapping and no new port grant: the fixture reaches the I/O APIC and LAPIC through the
/// existing uncached kernel aliases and COM1 through ring-0 port I/O; the TSS still carries no I/O
/// bitmap.
#[test]
fn no_new_access_path_reaches_userspace() {
    let c = code(WITNESS);
    for forbidden in [
        "map_page(",
        "map_mmio",
        "install_device",
        "io_map_base =",
        "iopl",
    ] {
        assert!(!c.contains(forbidden), "fixture contains {forbidden}");
    }
    assert!(c.contains("platform_layout::IOAPIC_MMIO_BASE"));
    let facts = body_after(X86_DT, "pub(crate) fn boot_tss_io_map_facts()");
    assert!(facts.contains("read_unaligned()") && !code(facts).contains("write"));
    assert!(GRADER.contains("tss_io_map_base=104 tss_size=104 io_bitmap=false"));
}

/// Interrupt-return contracts this package changed: owner revalidation only for ring-3 frames, and
/// a same-task asynchronous return keeps the interrupted flags.
#[test]
fn return_contracts_are_ring3_only_revalidation_and_preserved_flags() {
    assert!(X86_DT.contains(
        "if matches!(exiting_tid, None | Some(0))\n            && owner_revalidation_admissible(entering_cs)"
    ));
    assert!(
        X86_DT
            .contains("let same_task_async_return = !switched && vector as usize != VEC_SYSCALL;")
    );
    assert!(
        X86_DT
            .contains("frame.rflags = ring3_return_rflags(frame.rflags, same_task_async_return);")
    );
    assert!(X86_DT.contains("fn owner_revalidation_is_admissible_only_for_ring3_frames()"));
    assert!(X86_DT.contains(
        "fn ring3_return_rflags_keeps_interrupted_flags_only_for_same_task_async_returns()"
    ));
    // The idle loop halts with the one-instruction `sti` shadow, and idle-origin traps never
    // return into it.
    let park = body_after(
        X86_DT,
        "extern \"C\" fn x86_idle_park_loop(cpu: usize) -> ! {",
    );
    assert!(park.contains("core::arch::asm!(\"sti\", \"hlt\""));
    assert!(pos(park, "idle_boundary::park(cpu, rsp)") < pos(park, "\"sti\", \"hlt\""));
}

/// One receiver, one ring layout, one selector for all three ports.
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
    assert!(RECEIVER.contains("const ISOLATION_LOAD_VA: usize = 0xFFFF_FFFF_FEC0_0000;"));
    assert!(
        attr_before(SERVICE, "pub(super) mod uart_irq_witness;")
            .contains("all(feature = \"x86_64-uart-irq-witness\", target_arch = \"x86_64\")")
    );
}

/// The receiver checks GPRs and the carry flag across ring-3 interrupts; XMM is observational
/// only, reported on its own line, and never graded.
#[test]
fn the_receiver_grades_gprs_and_flags_and_only_observes_xmm() {
    let spin = body_after(
        RECEIVER,
        "fn spin_checking_registers(iters: u64, seed: u64) -> u32 {",
    );
    let x86 = spin
        .split_once("#[cfg(target_arch = \"x86_64\")]\n    unsafe {")
        .map(|(_, r)| r)
        .expect("x86 arm");
    for r in ["r12", "r13", "r14", "r15", "r9", "r10", "xmm8", "xmm9"] {
        assert!(x86.contains(&format!("inout(\"{r}\")")), "{r}");
    }
    assert!(x86.contains("\"stc\"") && x86.contains("\"setc {cf:l}\""));
    assert!(RECEIVER.contains("if bad_mask & 0xffff != 0 {"));
    assert!(RECEIVER.contains("IRQ1_UART_SIMD seq={} mode={} user_simd_mask=0x{:x}"));
    let recv = body_after(
        RECEIVER,
        "fn recv_timeout(cap: u32, timeout: u64) -> Recv {",
    );
    let x86_recv = recv
        .split_once("#[cfg(target_arch = \"x86_64\")]")
        .map(|(_, r)| r)
        .expect("x86 arm");
    assert!(x86_recv.contains("clobber_abi(\"C\")"));
}

/// The host driver owns COM1's only backend on the core smoke's machine and paces by
/// acknowledgement.
#[test]
fn the_driver_uses_a_dedicated_backend_and_acknowledgements() {
    assert!(DRIVER.contains("choices=[\"riscv64\", \"aarch64\", \"x86_64\"]"));
    assert!(DRIVER.contains("\"-machine\", \"q35\", \"-cpu\", \"qemu64\""));
    assert!(DRIVER.contains("\"-m\", \"512M\", \"-smp\", \"1\""));
    assert!(DRIVER.contains("socket,id=uart0,path={sock_path},server=on,wait=on"));
    assert!(DRIVER.contains("DISABLED_RE_X86_64 = re.compile(rb\"IRQ3_UART_SOURCE_DISABLED \")"));
    assert!(!DRIVER.contains("mux=on") && !DRIVER.contains("-nographic"));
    assert!(!DRIVER.contains("time.sleep(0.5") && !DRIVER.contains("time.sleep(1"));
    // The grader places idle RIPs after an idle `sti; hlt` and user RIPs in the checked spin.
    assert!(GRADER.contains("descriptor_tables::x86_idle_park_loop"));
    assert!(GRADER.contains("uart_irq_witness::spin_checking_registers"));
}
