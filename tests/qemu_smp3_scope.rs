// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3 — source guards for the RISC-V two-hart IPI / remote-fence / context wiring.
//!
//! These pin PLACEMENT and ORDER, which neither a hosted test nor QEMU can execute: QEMU's TCG
//! runs every hart's memory accesses in host order and flushes a whole TLB on any `sfence.vma`,
//! so a boot cannot tell whether a fence is present. The built image's ordering is additionally
//! checked by `scripts/check-riscv64-smp3-sequence.sh`; behaviour is executed by the hosted
//! `ipi`, `smp3_record` and async-resume tests, and live behaviour is graded by
//! `scripts/qemu-riscv64-smp3-witness-smoke.sh`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const IPI: &str = include_str!("../src/arch/riscv64/ipi.rs");
const SBI: &str = include_str!("../src/arch/riscv64/sbi.rs");
const BOOT: &str = include_str!("../src/arch/riscv64/boot.rs");
const TRAP: &str = include_str!("../src/arch/riscv64/trap.rs");
const TIMER: &str = include_str!("../src/arch/riscv64/timer.rs");
const CONSOLE: &str = include_str!("../src/arch/riscv64/console.rs");
const RV_MOD: &str = include_str!("../src/arch/riscv64/mod.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const DRAINS: &str = include_str!("../src/kernel/ipccall_direct_txn.rs");
const TASK: &str = include_str!("../src/kernel/task.rs");
const WITNESS_RS: &str = include_str!("../src/arch/riscv64/smp3_witness.rs");
const WITNESS_SH: &str = include_str!("../scripts/qemu-riscv64-smp3-witness-smoke.sh");
const SEQ_CHECK: &str = include_str!("../scripts/check-riscv64-smp3-sequence.sh");

const WITNESS_GATE: &str = "#[cfg(feature = \"riscv64-smp3-witness\")]";

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

/// `src` with every whitespace character removed, for comparisons rustfmt may re-wrap.
fn squash(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// The wake publishes its work, fences, and only then asks the firmware to interrupt the target;
/// it refuses (sending nothing) a target that never published itself ready or has no hart.
#[test]
fn the_wake_publishes_before_it_notifies() {
    let body = code(fn_body(
        IPI,
        "pub fn send_reschedule(sender: CpuId, target: CpuId) -> Result<(), IpiRefusal> {",
    ));
    let not_ready = pos(&body, "return Err(IpiRefusal::NotReady);");
    let no_hart = pos(&body, "hart_of(t).ok_or(IpiRefusal::NoHart)?");
    let publish = pos(&body, "slot.fetch_or(bit, Ordering::AcqRel)");
    let fence = pos(&body, "full_fence();");
    let notify = pos(&body, "crate::arch::riscv64::sbi::send_ipi(hart_mask, 0)");
    assert!(
        not_ready < publish && no_hart < publish,
        "refusals precede any publication"
    );
    assert!(
        publish < fence && fence < notify,
        "publication, fence, notification — in order"
    );
    assert!(
        body.trim_end()
            .ends_with("result.map_err(IpiRefusal::Firmware)")
    );
    assert!(code(IPI).contains("#[inline(never)]\npub fn send_reschedule("));
}

/// The arrival clears `sip.SSIP` BEFORE it consumes the mailbox, so a publication racing the
/// consumption leaves SSIP set for the next arrival instead of being lost.
#[test]
fn the_arrival_clears_before_it_consumes() {
    let body = code(fn_body(
        IPI,
        "pub fn take_arrival(cpu: CpuId, origin: ArrivalOrigin) -> IpiArrival {",
    ));
    assert!(pos(&body, "clear_ssip();") < pos(&body, "p.swap(0, Ordering::AcqRel)"));
    assert!(code(IPI).contains("#[inline(never)]\npub fn take_arrival("));
    let park = code(fn_body(
        IPI,
        "pub fn take_park_release(cpu: CpuId) -> bool {",
    ));
    assert!(park.trim_start().starts_with("clear_ssip();"));
}

/// The remote fence names harts (never CPU indices), fails closed without one, makes the PTE store
/// globally visible before the firmware call, and never reads an error as completion.
#[test]
fn the_remote_fence_is_ordered_and_fails_closed() {
    let body = code(fn_body(IPI, "pub fn remote_invalidate_page("));
    let no_hart = pos(&body, "return Err(FenceRefusal::NoHart);");
    let fence = pos(&body, "full_fence();");
    let call = pos(&body, "crate::arch::riscv64::sbi::remote_sfence_vma_asid(");
    assert!(no_hart < fence && fence < call);
    assert!(body.contains("hart_mask_for(targets, hart_of)"));
    assert!(
        body.trim_end()
            .ends_with("result.map_err(FenceRefusal::Firmware)")
    );
    assert!(code(IPI).contains("#[inline(never)]\npub fn remote_invalidate_page("));
    // The coordinator's RISC-V arm: completion only on the firmware's `Ok`.
    assert!(squash(&code(RUNTIME)).contains(&squash(
        "crate::arch::riscv64::ipi::remote_invalidate_page(requester, targets, asid.0, virt.0) .is_ok()"
    )));
}

/// One production implementation of each operation, and no competing software shootdown: the
/// firmware calls live only in their owners, the wake is sent only by the two committed drains,
/// and the fence only by the coordinator.
#[test]
fn each_operation_has_one_production_owner() {
    let ipi = code(IPI);
    assert_eq!(
        ipi.matches("crate::arch::riscv64::sbi::send_ipi(").count(),
        3
    );
    assert_eq!(
        ipi.matches("crate::arch::riscv64::sbi::remote_sfence_vma_asid(")
            .count(),
        1
    );
    // The third `send_ipi` is the witness's own start-up kick, compiled only into the witness.
    let kick = pos(&ipi, "pub fn kick_self(cpu: CpuId) -> bool {");
    assert!(ipi[..kick].trim_end().ends_with(WITNESS_GATE));
    assert_eq!(
        code(DRAINS)
            .matches("crate::arch::riscv64::ipi::send_reschedule(")
            .count(),
        2
    );
    assert_eq!(
        code(RUNTIME)
            .matches("crate::arch::riscv64::ipi::remote_invalidate_page(")
            .count(),
        1
    );
    for (name, src) in [
        ("boot.rs", BOOT),
        ("trap.rs", TRAP),
        ("runtime.rs", RUNTIME),
    ] {
        let c = code(src);
        assert!(
            !c.contains("sbi::send_ipi(") && !c.contains("sbi::remote_sfence"),
            "{name} must not call the firmware directly"
        );
    }
    // No S-mode CLINT access and no PLIC traffic on the IPI path.
    for needle in ["clint", "CLINT", "msip", "plic", "PLIC"] {
        assert!(!ipi.contains(needle), "ipi.rs must not touch {needle}");
    }
    assert!(code(SBI).contains("SBI_EXT_IPI") && code(SBI).contains("SBI_EXT_RFENCE"));
}

/// Each drain sends the wake only after the enqueue committed on another CPU, with no lock held.
#[test]
fn the_wake_follows_a_committed_remote_enqueue() {
    let drains = code(DRAINS);
    for (site, _) in drains.match_indices("crate::arch::riscv64::ipi::send_reschedule(") {
        let guard = drains[..site].rfind("if success.wake_target_cpu != executing_cpu {");
        assert!(
            guard.is_some_and(|g| site - g < 200),
            "every wake is guarded by the committed target being another CPU"
        );
    }
}

/// An IPI is settled before the external-claim route and never claims or completes a PLIC source:
/// the entry claims only for a supervisor EXTERNAL cause, and consumes the mailbox only for a
/// supervisor SOFTWARE cause.
#[test]
fn an_ipi_never_reaches_the_external_claim() {
    let boot = code(BOOT);
    let claim = pos(&boot, "let external_claim = if is_external_interrupt {");
    let take = pos(
        &boot,
        "let software_interrupt = if crate::arch::riscv64::ipi::is_software_interrupt(scause) {",
    );
    assert!(claim < take);
    // Two claim sites, both for an EXTERNAL cause: the entry above and the idle-origin external
    // landing. The idle-origin SOFTWARE landing claims nothing.
    assert_eq!(boot.matches("claim_external_interrupt_once()").count(), 2);
    let sw = code(fn_body(BOOT, "fn riscv_s_mode_software_trap("));
    assert!(!sw.contains("claim_external_interrupt_once") && !sw.contains("plic"));
    let trap = code(TRAP);
    let ipi = pos(&trap, "if let Some(arrival) = context.software_interrupt {");
    let ext = pos(
        &trap,
        "if !ipi_handled {\n        match context.external_claim {",
    );
    assert!(
        ipi < ext,
        "the IPI is settled before, and instead of, the claim route"
    );
}

/// Secondary dispatch is default-off: the boot hart releases no secondary without the knob, and a
/// parked secondary leaves its park only through that release.
#[test]
fn secondary_dispatch_is_behind_the_knob() {
    let rel = code(fn_body(
        IPI,
        "pub fn release_secondaries_at_boot_borrow_end(trap_ready_cpus: u64) -> usize {",
    ));
    assert!(squash(&rel).starts_with(&squash(
        "if !crate::kernel::boot::ap_user_dispatch_enabled() { return 0;"
    )));
    let boot = code(BOOT);
    let release = pos(
        &boot,
        "if crate::arch::riscv64::ipi::take_park_release(cpu) {",
    );
    let main = pos(&boot, "riscv_secondary_dispatch_main(cpu, hart_id);");
    assert!(release < main && main - release < 300);
    assert_eq!(
        boot.matches("riscv_secondary_dispatch_main(cpu, hart_id);")
            .count(),
        1
    );
}

/// A blocking-class deferral or terminal-idle check that the other hart's wake overtook is
/// settled through the shared owners — never by returning into the woken task's stale frame.
#[test]
fn an_overtaken_deferral_is_settled_not_returned_through() {
    let trap = code(TRAP);
    let helper = code(fn_body(TRAP, "fn settle_overtaken_deferral("));
    let reverify = pos(&helper, "if !shared.yield_reverify_ready(cpu) {");
    let acquire = pos(&helper, "shared.queue_advance_acquire_incoming_split(");
    let resume = pos(
        &helper,
        "direct_dispatch_resume_incoming(shared, token, frame)",
    );
    assert!(reverify < acquire && acquire < resume);
    assert!(helper.contains("reason: RiscvIdleReason::QueueAdvanceNoIncoming,"));
    for (marker, site) in [
        (
            "D2_SEND_GENUINE_FALLBACK reason=state_changed",
            "\"riscv_d2_send_overtaken\"",
        ),
        (
            "D2_RECV_GENUINE_FALLBACK reason=state_changed",
            "\"riscv_d2_recv_overtaken\"",
        ),
        (
            "RISCV_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed",
            "\"riscv_futex_wait_overtaken\"",
        ),
        (
            "RISCV_BLOCKED_IPC_IDLE_OVERTAKEN",
            "\"riscv_blocked_ipc_overtaken\"",
        ),
    ] {
        let m = pos(&trap, marker);
        let s = pos(&trap, site);
        assert!(m < s && s - m < 700, "{marker} is settled at {site}");
    }
    assert_eq!(trap.matches("settle_overtaken_deferral(").count(), 5);
}

/// Only a parked completion the port's resume boundary CONSUMES can compete with an async tag;
/// the classes counted are exactly the classes the restore facts take.
#[test]
fn the_coexistence_refusal_counts_only_consumed_classes() {
    let classifier = code(fn_body(
        TASK,
        "pub(crate) fn classify_and_take_async_resume(",
    ));
    assert!(squash(&classifier).contains(&squash(
        ".is_some_and(|done| resume_boundary_consumes(done.syscall_class))"
    )));
    let consumes = code(fn_body(
        TASK,
        "pub(crate) const fn resume_boundary_consumes(class: BlockedSyscallClass) -> bool {",
    ));
    let cfg = squash("any(feature = \"ipc-reply-timeout-oracle-core\", target_arch = \"aarch64\")");
    assert!(squash(&consumes).contains(&format!("cfg!({cfg})")));
    assert!(consumes.contains("BlockedSyscallClass::IpcSend => true,"));
    // The restore facts take the IpcRecv record under the same cfg, and IpcSend unconditionally.
    let facts = squash(&code(fn_body(
        TASK,
        "pub(crate) fn take_thread_restore_facts(",
    )));
    assert!(facts.contains(&format!("#[cfg({cfg})]letrecv_completion=")));
    assert!(facts.contains("BlockedSyscallClass::IpcSend);"));
    assert!(squash(&code(TASK)).contains(&format!("#[cfg({cfg})]pub(crate)recv_completion:")));
}

/// The witness is feature-gated, off by default, observes rather than performs, and its console
/// line lock exists only in the witness build.
#[test]
fn the_witness_is_gated_and_observes() {
    assert!(ROOT_CARGO.contains("riscv64-smp3-witness = []"));
    let default = ROOT_CARGO
        .split_once("default = [")
        .map(|(_, r)| r.split(']').next().unwrap_or(""))
        .unwrap_or("");
    assert!(!default.contains("riscv64-smp3-witness"));
    assert!(squash(&code(RV_MOD)).contains(&squash(
        "#[cfg(all(feature = \"riscv64-smp3-witness\", not(feature = \"hosted-dev\"), target_arch = \"riscv64\"))] pub mod smp3_witness;"
    )));
    // Every production-file call into the witness sits directly under the witness gate.
    for (name, src) in [
        ("boot.rs", BOOT),
        ("trap.rs", TRAP),
        ("timer.rs", TIMER),
        ("ipi.rs", IPI),
    ] {
        let mut c = code(src);
        // `kick_self` is itself compiled only into the witness (pinned above), so its body's
        // calls need no gate of their own.
        if let Some(k) = c.find("pub fn kick_self(cpu: CpuId) -> bool {") {
            let end = k + c[k..].find("\n}\n").unwrap_or(0);
            c.replace_range(k..end, "");
        }
        for (i, _) in c.match_indices("crate::arch::riscv64::smp3_witness::") {
            let gate = c[..i].rfind(WITNESS_GATE);
            assert!(
                gate.is_some_and(|g| c[g..i].matches('\n').count() <= 2),
                "{name}: witness hook at byte {i} is not feature-gated"
            );
        }
    }
    let console = squash(&code(CONSOLE));
    assert_eq!(console.matches("staticLINE:AtomicBool").count(), 1);
    let lock = pos(&console, "staticLINE:AtomicBool");
    let gated = console[..lock].rfind("feature=\"riscv64-smp3-witness\"))]pubfnwrite_line");
    assert!(
        gated.is_some(),
        "the line lock is compiled only into the witness build"
    );
    // Labelled synchronization only, at points where no lock is held.
    assert!(WITNESS_RS.contains("WITNESS SYNCHRONIZATION — labelled, not production behaviour."));
}

/// The image-ordering check is run on every witness artifact, over exactly the three owners.
#[test]
fn the_image_ordering_check_covers_the_three_owners() {
    assert!(
        WITNESS_SH.contains(
            "scripts/check-riscv64-smp3-sequence.sh \"$BUILD_DIR/yarm-riscv64-smp3.elf\""
        )
    );
    for owner in [
        "\"remote_invalidate_page\"",
        "\"send_reschedule\"",
        "\"take_arrival\"",
    ] {
        assert!(SEQ_CHECK.contains(owner), "{owner} is checked in the image");
    }
    assert!(SEQ_CHECK.contains("--mattr=+m,+a,+c"));
}

/// A reply that lands while its caller is still inside its NR 6 commit on the other hart — the
/// terminal armed for this record and still open, no acknowledgement to claim — answers the
/// non-mutating `WouldBlock` the replier retries, before and instead of the "spent" refusal.
#[test]
fn a_reply_racing_its_callers_block_is_retried_not_spent() {
    const SPLIT: &str = include_str!("../src/kernel/syscall_split.rs");
    let c = code(SPLIT);
    let open = pos(
        &c,
        "} else if matches!(\n        facts.terminal,\n        crate::kernel::direct_eligibility::DirectReplyTerminal::AvailableExact\n    ) {",
    );
    let retry = pos(&c, "\"caller_not_yet_blocked\",");
    let spent = pos(&c, "\"mode_indeterminate\",");
    assert!(open < retry && retry < spent);
    assert!(c[open..retry].contains("crate::kernel::syscall::SyscallError::WouldBlock,"));
    assert!(
        !c[open..retry].contains("claim") || c[open..retry].contains("return nr7_refuse("),
        "the arm claims, copies and wakes nothing"
    );
}

/// Every `Riscv64TrapContext` built anywhere — including the feature-gated idle-origin landings no
/// default build compiles — names the software-interrupt arrival explicitly. QEMU-SMP3's first
/// frozen candidate missed the UART witness's landing, which only its own profile builds.
#[test]
fn every_trap_context_initializer_names_the_software_interrupt() {
    let boot = code(BOOT);
    let mut n = 0;
    for (i, _) in boot.match_indices("crate::arch::riscv64::trap::Riscv64TrapContext {") {
        let body = &boot[i..i + boot[i..].find("};").expect("initializer end")];
        assert!(
            body.contains("software_interrupt"),
            "initializer at byte {i} omits software_interrupt"
        );
        n += 1;
    }
    assert_eq!(
        n, 3,
        "the U-origin entry, the idle IPI landing and the idle external landing"
    );
}
