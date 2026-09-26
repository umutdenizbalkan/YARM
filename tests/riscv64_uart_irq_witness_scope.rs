// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ1 — source guards for the RISC-V UART external-interrupt witness.
//!
//! These are SOURCE guards: they pin structure the live witness depends on (what is feature-gated,
//! what is ordered before what, which owner does what). They are not evidence that an interrupt was
//! delivered — `scripts/qemu-riscv64-uart-irq-witness-smoke.sh` is.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const BOOT_MOD: &str = include_str!("../src/kernel/boot/mod.rs");
const RV_BOOT: &str = include_str!("../src/arch/riscv64/boot.rs");
const RV_TRAP: &str = include_str!("../src/arch/riscv64/trap.rs");
const RV_IRQ: &str = include_str!("../src/arch/riscv64/irq.rs");
const RV_PLIC: &str = include_str!("../src/arch/riscv64/plic.rs");
const RV_PT: &str = include_str!("../src/arch/riscv64/page_table.rs");
const RV_MOD: &str = include_str!("../src/arch/riscv64/mod.rs");
const WITNESS: &str = include_str!("../src/arch/riscv64/uart_irq_witness.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const RECEIVER: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/uart_irq_witness.rs");
const SERVICE: &str =
    include_str!("../crates/yarm-control-plane-servers/src/control_plane/init/service.rs");
const DRIVER: &str = include_str!("../scripts/qemu-riscv64-uart-irq-driver.py");

const GATE: &str = "feature = \"riscv-uart-irq-witness\"";

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

fn method_body_after<'a>(src: &'a str, head: &str) -> &'a str {
    let rest = src
        .split_once(head)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("{head} missing"));
    rest.split("\n    }\n").next().unwrap_or(rest)
}

fn attr_before<'a>(src: &'a str, head: &str) -> &'a str {
    let before = src
        .split_once(head)
        .map(|(b, _)| b)
        .unwrap_or_else(|| panic!("{head} missing"));
    // The attribute block immediately above the item: the text after the previous item's end.
    before.rsplit("\n}\n").next().unwrap_or(before)
}

/// The witness is not part of any default build.
#[test]
fn the_witness_feature_is_declared_and_not_default() {
    assert!(ROOT_CARGO.contains("\nriscv-uart-irq-witness = []\n"));
    let default = ROOT_CARGO
        .lines()
        .find(|l| l.starts_with("default = "))
        .expect("default features");
    assert!(!default.contains("riscv-uart-irq-witness"), "{default}");
}

/// Every piece that maps a device, enables a source, admits an S-mode external interrupt,
/// provisions the receiver or widens NR 5 is compiled only with the witness feature.
#[test]
fn every_witness_entry_point_is_feature_gated() {
    assert!(attr_before(RV_MOD, "pub mod uart_irq_witness;").contains(GATE));
    assert!(attr_before(RV_BOOT, "fn riscv_s_mode_external_trap(").contains(GATE));
    assert!(attr_before(BOOT_MOD, "pub fn provision_init_uart_irq_witness(").contains(GATE));
    assert!(attr_before(RUNTIME, "fn witness_notification_probe_into_frame(").contains(GATE));
    assert!(attr_before(SERVICE, "pub(super) mod uart_irq_witness;").contains(GATE));
    // Each call site too.
    for (name, src, call) in [
        (
            "plic window install",
            RV_PLIC,
            "uart_irq_witness::install_device_window_at_safe_point()",
        ),
        (
            "plic enable",
            RV_PLIC,
            "uart_irq_witness::enable_source_after_plic_ready(",
        ),
        (
            "trap drain",
            RV_TRAP,
            "uart_irq_witness::drain_before_delivery(",
        ),
        (
            "completion count",
            RV_IRQ,
            "uart_irq_witness::note_completion_written(",
        ),
        (
            "S-mode screen",
            RV_BOOT,
            "plic::is_accepted_s_mode_external_trap(",
        ),
        (
            "NR 5 arm",
            RUNTIME,
            "self.witness_notification_probe_into_frame(",
        ),
        ("provisioning", RV_BOOT, "provision_init_uart_irq_witness("),
    ] {
        let at = src
            .find(call)
            .unwrap_or_else(|| panic!("{name}: {call} missing"));
        let window = &src[at.saturating_sub(400)..at];
        assert!(
            window.contains("riscv-uart-irq-witness"),
            "{name} must be feature-gated"
        );
    }
}

/// Without the feature the PLIC bring-up still defers with its pinned reason.
#[test]
fn the_feature_off_bring_up_still_defers() {
    assert!(RV_PLIC.contains("reason=uart0_is_safe_candidate_but_handler_not_ready"));
    assert!(RV_PLIC.contains("\"RISCV_EXTIRQ_DEFERRED reason={}\""));
    assert!(!RV_PLIC.contains("csrs sie") && !RV_PLIC.contains("sie, {"));
}

/// Enable order: mappings, readiness, route, controller, source, CPU admission.
#[test]
fn the_enable_order_is_mappings_readiness_route_controller_source_cpu() {
    let enable = body_after(WITNESS, "pub fn enable_source_after_plic_ready(");
    let pos = |needle: &str| {
        enable
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing"))
    };
    let readiness = pos("mmio_va(priority_pa(");
    let route = pos("ipc.irq_routes.get(source as usize)");
    let threshold = pos("write32(thresh_va, 0);");
    let priority = pos("write32(prio_va, 1);");
    let enable_bit = pos("write32(enable_va, read32(enable_va) | bit);");
    let ier = pos("write8(ier_va, UART_IER_ERBFI);");
    let seie = pos("csrs sie, {0}");
    assert!(readiness < route && route < threshold && threshold < priority);
    assert!(priority < enable_bit && enable_bit < ier && ier < seie);
    // And the window is installed before any of it, at the safe point, by the PLIC owner.
    let install = RV_PLIC
        .find("install_device_window_at_safe_point()")
        .expect("install");
    let hand_off = RV_PLIC
        .find("enable_source_after_plic_ready(")
        .expect("enable");
    let readiness_marker = RV_PLIC
        .find("RISCV_EXTIRQ_CLAIM_READINESS")
        .expect("marker");
    assert!(install < readiness_marker && readiness_marker < hand_off);
}

/// The fixture drains before delivery and completion, and never writes a completion itself.
#[test]
fn drain_precedes_delivery_and_the_fixture_never_completes() {
    let settle = body_after(RV_TRAP, "fn settle_riscv_external_claim(");
    let drain = settle.find("drain_before_delivery(").expect("drain");
    let deliver = settle
        .find("settle_external_interrupt_at_bridge(")
        .expect("deliver+complete");
    assert!(drain < deliver);
    assert!(!WITNESS.contains("write_claim_completion") && !WITNESS.contains("PLIC_CLAIM_OFFSET)"));
    assert!(
        !WITNESS.contains("read_claim_register"),
        "the fixture never claims"
    );
    // Identity comes from the claim's source, never from stval.
    assert!(!code(WITNESS).contains("stval"));
}

/// The window is kernel-only, outside every user-mappable address, and shared untracked.
#[test]
fn the_device_window_is_kernel_only() {
    let flags = RV_PT
        .split_once("pub const DEVICE_WINDOW_LEAF_FLAGS: u64 =")
        .and_then(|(_, r)| r.split_once(';'))
        .map(|(f, _)| f)
        .expect("leaf flags");
    assert!(
        !flags.contains("USER") && !flags.contains("EXECUTE"),
        "{flags}"
    );
    assert!(flags.contains("READ") && flags.contains("WRITE") && flags.contains("GLOBAL"));
    assert!(RV_PT.contains("pub const DEVICE_WINDOW_ROOT_SLOT: usize = 255;"));
    // Slot 255 starts at 0x3F_C000_0000, above the 0x8000_0000 user ceiling.
    assert!((255u64 << 30) >= 0x8000_0000);
    // Every root receives it where roots are born.
    let ensure = body_after(RV_PT, "fn ensure_asid(&mut self, asid: Asid)");
    assert!(ensure.contains("self.install_device_window_into_root(root_phys);"));
    // Teardown cannot free it: the shared tables are never entered into `pages`.
    let install = body_after(RV_PT, "pub fn install_device_window(pages: &[u64])");
    assert!(!install.contains("alloc_page()") && install.contains("alloc_pt_frame()"));
}

/// Readiness is the walk, and the claim reads at the address the walk returned.
#[test]
fn readiness_is_the_walk_not_a_flag() {
    assert!(RV_PLIC.contains("super::page_table::device_pa_reachable_under_active_satp("));
    assert!(RV_PT.contains("crate::arch::device_window_rule::device_pa_reachable("));
    assert!(!RV_PLIC.contains("static CLAIM_READY") && !WITNESS.contains("CLAIM_READY"));
}

/// The NR 5 notification arm is narrow: a non-blocking probe, after the endpoint resolver's own
/// `WrongObject`, for a user receiver with recv-v2 metadata, written back by the endpoint probe's
/// own user-copy completion.
#[test]
fn the_nr5_notification_arm_is_narrow() {
    let call = RUNTIME
        .find("self.witness_notification_probe_into_frame(")
        .expect("the arm's call");
    let guard = &RUNTIME[call.saturating_sub(500)..call];
    assert!(guard.contains("KernelError::WrongObject) && timeout_ticks == 0"));
    let arm = method_body_after(RUNTIME, "fn witness_notification_probe_into_frame(");
    assert!(arm.contains("resolve_notification_recv_cap_in_pid_from_raw("));
    assert!(arm.contains("if meta_ptr == 0 || meta_len < IPC_RECV_META_V2_ENCODED_LEN"));
    assert!(arm.contains("notification.recv()"));
    assert!(arm.contains("self.complete_recv_boundary_user_copy(cpu, frame, &pending)"));
    assert!(arm.contains("SyscallError::WouldBlock.code()"));
    assert!(
        !code(arm).contains("notification_waiters"),
        "the arm never parks"
    );
    // NR 2 is untouched.
    let nr2 = method_body_after(
        RUNTIME,
        "pub fn try_split_ipc_recv_queued_plain_into_frame(",
    );
    assert!(!nr2.contains("witness_notification_probe"));
}

/// The two sides of the witness ABI agree.
#[test]
fn kernel_and_receiver_agree_on_selector_ring_and_words() {
    assert!(BOOT_MOD.contains("pub const UART_IRQ_WITNESS_SELECTOR: u64 = 30;"));
    assert!(RECEIVER.contains("pub(super) const SELECTOR: u32 = 30;"));
    assert!(BOOT_MOD.contains("pub const UART_IRQ_WITNESS_RING_VA: u64 = 0x2800_0000;"));
    assert!(RECEIVER.contains("pub(super) const RING_VA: usize = 0x2800_0000;"));
    for (kernel, user) in [
        ("RING_WORD_MAGIC: usize = 0;", "W_MAGIC: usize = 0;"),
        ("RING_WORD_ARMED: usize = 1;", "W_ARMED: usize = 1;"),
        ("RING_WORD_DISABLED: usize = 2;", "W_DISABLED: usize = 2;"),
        ("RING_WORD_BYTES: usize = 3;", "W_BYTES: usize = 3;"),
        ("RING_WORD_CLAIMS: usize = 4;", "W_CLAIMS: usize = 4;"),
        (
            "RING_WORD_COMPLETIONS: usize = 5;",
            "W_COMPLETIONS: usize = 5;",
        ),
        ("RING_WORD_IDLE_ORIGIN: usize = 6;", "W_IDLE: usize = 6;"),
        ("RING_WORD_USER_ORIGIN: usize = 7;", "W_USER: usize = 7;"),
        ("RING_WORD_EMPTY_DRAINS: usize = 8;", "W_EMPTY: usize = 8;"),
        ("RING_WORD_BOUND_LINE: usize = 9;", "W_LINE: usize = 9;"),
        (
            "RING_BYTES_OFFSET: usize = 256;",
            "RING_BYTES_OFFSET: usize = 256;",
        ),
        (
            "RING_MAGIC: u32 = 0x4952_5131;",
            "RING_MAGIC: u32 = 0x4952_5131;",
        ),
    ] {
        assert!(WITNESS.contains(kernel), "{kernel}");
        assert!(RECEIVER.contains(user), "{user}");
    }
    // The receiver's claim-register window address is the kernel's layout: slot 2 = context page.
    assert!(RECEIVER.contains("const WINDOW_CLAIM_VA: usize = DEVICE_WINDOW_BASE + 2 * 4096 + 4;"));
    let install = body_after(WITNESS, "pub fn install_device_window_at_safe_point()");
    let slot2 = install
        .split_once("let pages = [")
        .and_then(|(_, r)| r.split_once("];"))
        .map(|(p, _)| p)
        .expect("the page list");
    assert!(
        slot2
            .lines()
            .filter(|l| l.contains("_pa("))
            .nth(2)
            .is_some_and(|l| l.contains("threshold_pa"))
    );
}

/// Selector 30 is claimed by no other slot-5 cell.
#[test]
fn selector_30_is_free() {
    for src in [BOOT_MOD, SERVICE] {
        for taken in ["SELECTOR: u64 = 30;", "Some(30)"] {
            let n = src.matches(taken).count();
            let own = usize::from(
                src.contains("UART_IRQ_WITNESS_SELECTOR: u64 = 30;")
                    && taken.starts_with("SELECTOR"),
            );
            assert_eq!(n, own, "{taken} claimed elsewhere");
        }
    }
    let abi = include_str!("../crates/yarm-ipc-abi/src/terminal_fault_oracle_abi.rs");
    assert!(!abi.contains("= 30;"));
    let exit = include_str!("../crates/yarm-ipc-abi/src/exit_current_task_abi.rs");
    assert!(!exit.contains("= 30;"));
}

/// The S-mode interrupt window is closed: SIE is set only inside the stack-free idle wfi loop, and
/// the early-marker line lock is held with interrupts masked.
#[test]
fn the_idle_boundary_window_is_closed() {
    let marker = body_after(RV_BOOT, "pub(crate) fn early_sbi_marker(");
    let save = marker.find("irq::irq_save()").expect("mask");
    let lock = marker.find("early_marker_lock_acquire();").expect("lock");
    let unlock = marker.find("early_marker_lock_release();").expect("unlock");
    let restore = marker.find("irq::irq_restore(irq_state)").expect("restore");
    assert!(save < lock && lock < unlock && unlock < restore);
    let halt = body_after(RV_BOOT, "fn riscv_trap_halt(reason: &'static str) -> ! {");
    assert!(halt.contains("timer::halt_wait_loop()"));
    assert!(
        !halt.contains("asm!"),
        "the halt waits only through the audited wait"
    );
}

/// The host driver uses a dedicated serial backend, no monitor, and acknowledgement-driven
/// injection.
#[test]
fn the_driver_uses_a_dedicated_backend_and_acknowledgements() {
    assert!(DRIVER.contains("\"-monitor\", \"none\""));
    assert!(DRIVER.contains("socket,id=uart0,path={sock_path},server=on,wait=on"));
    assert!(DRIVER.contains("\"-serial\", \"chardev:uart0\""));
    assert!(!DRIVER.contains("mux=on") && !DRIVER.contains("-nographic"));
    assert!(DRIVER.contains("IRQ1_UART_READY seq="));
    // The only sleep is waiting for QEMU's listening socket before the guest has started.
    assert_eq!(DRIVER.matches("time.sleep(").count(), 1);
}
