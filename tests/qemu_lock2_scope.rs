// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK2 — source guards for the AArch64 contention witness on `vm_state_lock` and the
//! reschedule SGI deferred through the holder's masked hold.
//!
//! QEMU's TCG cannot show where a hook sits relative to the controller access it observes, that the
//! witness never acknowledges or completes an interrupt itself, or that the lock path is bounded and
//! console-free. These guards pin those facts on comment-stripped source; the live behaviour is
//! graded by `scripts/qemu-aarch64-lock2-witness-smoke.sh`, and the grader by its `--self-test`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const KMOD: &str = include_str!("../src/kernel/mod.rs");
const FACADE: &str = include_str!("../src/kernel/lock_witness.rs");
const WITNESS: &str = include_str!("../src/kernel/lock2_witness.rs");
const IRQ: &str = include_str!("../src/arch/aarch64/irq.rs");
const SMP: &str = include_str!("../src/arch/aarch64/smp.rs");
const SMP2: &str = include_str!("../src/arch/aarch64/smp2_witness.rs");
const SMP2_ASM: &str = include_str!("../src/arch/aarch64/smp2_witness.S");
const SMP2_RECORD: &str = include_str!("../src/kernel/boot/smp2_record.rs");
const BOOT: &str = include_str!("../src/arch/aarch64/boot.rs");
const SMOKE: &str = include_str!("../scripts/qemu-aarch64-lock2-witness-smoke.sh");
const GRADER: &str = include_str!("../scripts/grade-aarch64-lock2-witness.py");
const CORE: &str = include_str!("../scripts/lock_witness_core.py");

const GATE: &str = "#[cfg(feature = \"aarch64-lock2-witness\")]";
const HOOK_GATE: &str = "#[cfg(all(feature = \"aarch64-lock2-witness\", target_arch = \"aarch64\", not(feature = \"hosted-dev\")))]";

fn code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| match l.find(" // ") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn squash(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The brace-balanced body of the first fn whose head starts with `head`.
fn fn_body<'a>(src: &'a str, head: &str) -> &'a str {
    let i = src.find(head).unwrap_or_else(|| panic!("{head} missing"));
    let open = i + src[i..].find('{').unwrap();
    let b = src.as_bytes();
    let (mut depth, mut j) = (0i32, open);
    loop {
        match b[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=j];
                }
            }
            _ => {}
        }
        j += 1;
    }
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// Default off, co-enables SMP2 and the shared hook feature; the module and the facade route are
/// gated; LOCK1 and LOCK2 cannot be built together.
#[test]
fn the_witness_is_gated_and_off_by_default() {
    assert!(
        ROOT_CARGO.contains("aarch64-lock2-witness = [\"aarch64-smp2-witness\", \"lock-witness\"]")
    );
    let default = ROOT_CARGO
        .split_once("default = [")
        .map(|(_, r)| r.split(']').next().unwrap_or(""))
        .unwrap_or("");
    assert!(!default.contains("lock2") && !default.contains("lock-witness"));
    assert!(squash(&code(KMOD)).contains(&squash(
        "#[cfg(feature = \"aarch64-lock2-witness\")] pub mod lock2_witness;"
    )));
    assert!(squash(&code(FACADE)).contains(&squash(
        "#[cfg(feature = \"aarch64-lock2-witness\")] pub use crate::kernel::lock2_witness::{ \
         VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released, };"
    )));
    assert!(FACADE.contains("mutually exclusive"));
}

/// The claim hook sits in the ONE `GICC_IAR` reader, after the read, handed the token exactly as
/// read; the completion hook sits in the ONE `GICC_EOIR` writer, after the write. Each is called from
/// nowhere else, and each owner keeps exactly one caller path from production.
#[test]
fn claims_and_completions_are_observed_at_the_one_reader_and_writer() {
    // Production code only: the hosted unit tests at the end of irq.rs drive a register array.
    let irq = code(IRQ.split("#[cfg(test)]\nmod tests").next().unwrap());
    // One reader of GICC_IAR, one writer of GICC_EOIR.
    assert_eq!(
        irq.matches("GICC_IAR_OFFSET").count(),
        2,
        "declared once, read once"
    );
    assert_eq!(
        irq.matches("GICC_EOIR_OFFSET").count(),
        2,
        "declared once, written once"
    );
    let read = code(fn_body(&irq, "fn gic_read_iar(base: usize) -> u32 {"));
    let v = pos(
        &read,
        "read_volatile((base + GICC_IAR_OFFSET) as *const u32)",
    );
    let hook = pos(&read, "crate::kernel::lock2_witness::note_claim(token);");
    assert!(v < hook && hook < pos(&read, "\n    token\n"));
    assert!(squash(&read[..hook]).contains(&squash(HOOK_GATE)));
    let write = code(fn_body(
        &irq,
        "fn gic_write_eoir(base: usize, token: u32) {",
    ));
    let w = pos(&write, "gic_write_u32(base, GICC_EOIR_OFFSET, token);");
    let hook = pos(
        &write,
        "crate::kernel::lock2_witness::note_completion(token);",
    );
    assert!(w < hook);
    assert!(squash(&write[..hook]).contains(&squash(HOOK_GATE)));
    // The hooks have no other call site in the tree's AArch64 sources.
    for src in [&code(BOOT), &code(SMP), &code(SMP2)] {
        assert!(!src.contains("note_claim(") && !src.contains("note_completion("));
    }
    assert_eq!(irq.matches("lock2_witness::note_claim(").count(), 1);
    assert_eq!(irq.matches("lock2_witness::note_completion(").count(), 1);
    // The vector entry claims once and the tail completes the same token once (unchanged owners).
    let boot = code(BOOT);
    assert_eq!(
        boot.matches("crate::arch::aarch64::irq::claim_interrupt()")
            .count(),
        1
    );
    assert_eq!(
        boot.matches("crate::arch::aarch64::irq::complete_interrupt(ack);")
            .count(),
        1
    );
}

/// The witness never acknowledges, completes or raises an interrupt itself: no MMIO access at all
/// in the module, the only controller read is the distributor pending view (no CPU-interface
/// register, no write), and the only publication is the production send owner.
#[test]
fn the_witness_never_claims_completes_or_synthesizes() {
    let w = code(WITNESS);
    for banned in [
        "read_volatile",
        "write_volatile",
        "GICC_",
        "GICD_SGIR",
        "claim_interrupt",
        "complete_interrupt",
        "kick_self",
        "external_irq_eoi",
    ] {
        assert!(!w.contains(banned), "the witness must not use {banned}");
    }
    assert_eq!(
        w.matches("crate::arch::aarch64::smp::send_reschedule_sgi(")
            .count(),
        1,
        "one production publication"
    );
    let view = code(fn_body(
        SMP,
        "pub fn sgi_pending_view() -> Option<(u8, bool, bool)> {",
    ));
    assert!(!view.contains("write32") && !view.contains("GICC_") && !view.contains("cpu_if"));
    for reg in [
        "const GICD_ISPENDR0: usize = 0x200;",
        "const GICD_ISACTIVER0: usize = 0x300;",
        "const GICD_SPENDSGIR0: usize = 0xF20;",
    ] {
        assert!(view.contains(reg), "{reg}");
    }
    assert_eq!(
        view.matches("read32(").count(),
        3,
        "three reads, nothing else"
    );
    // The observers exist only in the witness build.
    for head in [
        "pub fn sgi_pending_view() -> Option<(u8, bool, bool)> {",
        "pub fn interface_mask(cpu: CpuId) -> u8 {",
    ] {
        let s = code(SMP);
        let i = pos(&s, head);
        assert!(
            s[..i]
                .rfind(GATE)
                .is_some_and(|g| s[g..i].matches('\n').count() <= 1)
        );
    }
}

/// Everything on the lock path, the gate and the controller hooks is bounded, lock-free,
/// allocation-free and console-free; console output lives only in `dump`.
#[test]
fn the_lock_and_interrupt_paths_are_bounded_and_console_free() {
    for head in [
        "pub fn note_contended(id: u32) {",
        "pub fn note_acquired(id: u32) -> u64 {",
        "pub fn note_released(id: u32, token: u64) {",
        "pub fn maybe_hold(id: u32, token: u64) {",
        "pub fn note_claim(token: u32) {",
        "pub fn note_completion(token: u32) {",
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
        "fn record(kind: u8, hart: u8, f: [u64; 5]) {",
    ] {
        let b = code(fn_body(WITNESS, head));
        for banned in [
            "printk", "print!", "format!", "alloc::", ".lock()", "Box::", "Vec::", "loop {",
        ] {
            assert!(!b.contains(banned), "{head} must not use {banned}");
        }
        // Every wait counts down a finite budget.
        for (i, _) in b.match_indices("while ") {
            let body = &b[i..];
            let end = body.find("\n    }").unwrap_or(body.len());
            assert!(
                body[..end].contains("left") || body[..end].contains("polls"),
                "{head}: an unbounded wait"
            );
        }
    }
    for bound in [
        "const HOLD_SPINS: u64",
        "const GATE_SPINS: u64",
        "const PEND_SPINS: u64",
        "const QUIESCE_SPINS: u64",
    ] {
        assert!(WITNESS.contains(bound));
    }
    let c = code(WITNESS);
    assert_eq!(c.matches("printk_emit_sync(").count(), 1);
    assert!(pos(&c, "pub fn dump() {") < pos(&c, "printk_emit_sync("));
}

/// The hold: the distributor view is read BEFORE the waiter's gate is released, the contention is
/// awaited, the view is read again, both are recorded with the masked state inside the
/// acquisition, and a link is armed only for a request that appeared during the hold — never over
/// an unresolved link silently.
#[test]
fn the_hold_observes_the_controller_before_and_after() {
    let h = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32, token: u64) {"));
    let pre = pos(&h, "let pre = view();");
    let held = pos(&h, "\n    HELD_ROUND.store(round, Ordering::Release);");
    let wait = pos(&h, "while seen <= baseline {");
    let post = pos(&h, "let mut post = view();");
    let rec_hold = pos(&h, "K_HOLD,");
    let rec_pend = pos(&h, "K_PENDING,");
    let arm = pos(&h, "cell.swap(round | (mask << 16), Ordering::AcqRel)");
    assert!(pre < held && held < wait && wait < post && post < rec_hold);
    assert!(rec_hold < rec_pend && rec_pend < arm);
    assert!(squash(&h).contains(&squash(
        "valid == 1 && mask != 0 && pre & mask == 0 && post & mask != 0 && (post >> 8) & 1 == 0"
    )));
    assert!(h.contains("record(K_LINKLOST, me, [old & 0xffff, round, 0, 0, 0]);"));
    assert!(h.contains("irq_unmasked()"));
    // The claim discharges only the reschedule SGI from the linked source; the completion names
    // both tokens.
    let cl = code(fn_body(WITNESS, "pub fn note_claim(token: u32) {"));
    assert!(squash(&cl).contains(&squash(
        "let discharged = u64::from(kind == 0 && (1u64 << source) == mask);"
    )));
    let co = code(fn_body(WITNESS, "pub fn note_completion(token: u32) {"));
    assert!(co.contains("let inflight = f.swap(0, Ordering::AcqRel);"));
}

/// The gate: the holder waits for the waiter to ARRIVE (its marker copy, which takes the VM lock,
/// is then done) before taking the lock; the waiter marks arrival, waits for the holder inside its
/// masked ownership, and only then publishes.
#[test]
fn the_gate_orders_arrival_ownership_and_publication() {
    let g = code(fn_body(
        WITNESS,
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
    ));
    let holder_wait = pos(&g, "while WAITER_ARRIVED.load(Ordering::Acquire) < round {");
    let arrive = pos(&g, "WAITER_ARRIVED.store(round, Ordering::Release);");
    let held_wait = pos(&g, "while HELD_ROUND.load(Ordering::Acquire) < round {");
    let publish = pos(&g, "publish_sgi_to_holder(cpu, holder, round);");
    assert!(holder_wait < arrive && arrive < held_wait && held_wait < publish);
}

/// The SMP2 workload carries the witness: arm, gate, round completion and dump are gated hooks,
/// and the twelve-round count is injected into the assembly and the verifier together.
#[test]
fn the_smp2_hooks_and_round_count_are_gated() {
    let s = code(SMP2);
    for hook in [
        "crate::kernel::lock2_witness::arm();",
        "crate::kernel::lock2_witness::mut_round_gate(cpu.0, round);",
        "crate::kernel::lock2_witness::note_round_ok(round);",
        "crate::kernel::lock2_witness::dump();",
    ] {
        let i = pos(&s, hook);
        assert!(
            s[..i]
                .rfind(GATE)
                .is_some_and(|g| s[g..i].matches('\n').count() <= 2),
            "{hook} is gated"
        );
    }
    assert!(squash(&s).contains(&squash(
        "#[cfg(not(feature = \"aarch64-lock2-witness\"))] \
         core::arch::global_asm!(\".equ YARM_MUT_ROUNDS, 4\\n\", include_str!(\"smp2_witness.S\"));"
    )));
    assert!(squash(&s).contains(&squash(
        "#[cfg(feature = \"aarch64-lock2-witness\")] \
         core::arch::global_asm!(\".equ YARM_MUT_ROUNDS, 12\\n\", include_str!(\"smp2_witness.S\"));"
    )));
    assert!(SMP2_ASM.contains(".set MUT_ROUNDS, YARM_MUT_ROUNDS"));
    let r = code(SMP2_RECORD);
    assert!(squash(&r).contains(&squash(
        "#[cfg(not(feature = \"aarch64-lock2-witness\"))] pub const MUT_ROUNDS: u64 = 4;"
    )));
    assert!(squash(&r).contains(&squash(
        "#[cfg(feature = \"aarch64-lock2-witness\")] pub const MUT_ROUNDS: u64 = 12;"
    )));
    assert!(WITNESS.contains("pub const LOCK2_ROUNDS: u64 = 12;"));
}

/// The smoke builds every image with the feature, records the identity, and hands the boot log to
/// the independent grader, which re-derives everything through the shared core.
#[test]
fn the_smoke_and_grader_are_wired() {
    assert!(SMOKE.contains("FEATURES=aarch64-lock2-witness"));
    assert!(
        SMOKE.contains("BOOTSTRAP_FEATURE_ARGS=\"--no-default-features --features $FEATURES\"")
    );
    assert!(SMOKE.contains("python3 scripts/grade-aarch64-lock2-witness.py"));
    assert!(SMOKE.contains("sha256sum \"$BUILD_DIR/yarm-aarch64.bin\""));
    for call in [
        "corroborating_seal(lines, \"SMP2_VERDICT\", fail)",
        "transport(lines, \"LOCK2\", META_RX, fail, summary=(COUNTS_RX, \"cpu\"))",
        "acqs = ownership(ev, ROUNDS, fail)",
        "contention_outside_own(ev, acqs, ROUNDS, fail)",
        "contention_credit(r, W, hold, ev, acqs)",
        "MIN_PER_DIRECTION = 4",
    ] {
        assert!(GRADER.contains(call), "the grader applies {call}");
    }
    for check in [
        "the discharging claim (token 0x%x) is not the reschedule SGI from the",
        "mismatched acknowledgement token",
        "lies inside the masked acquisition",
        "names round %d's link outside its lifetime",
        "which was never armed there",
        "contradicts its controller views",
        "unbalanced controller obligations",
        "had not settled its claimed interrupt",
        "lies outside the holder's masked acquisition before",
        "was never discharged",
    ] {
        assert!(GRADER.contains(check), "the grader enforces: {check}");
    }
    assert!(CORE.contains("def ownership(ev, rounds, fail):"));
}

/// Round-indexed mailbox words live at `M_x + round * 8`: every field's stride must hold the most
/// rounds the build runs, and the last word must stay inside the one mailbox page the kernel clears.
/// (QEMU-LOCK2's first candidate kept the plain 0x40 stride with twelve rounds; rounds >= 8 then
/// read their neighbours' words and the handshakes returned early.)
#[test]
fn the_mailbox_stride_holds_every_round() {
    let a = SMP2_ASM;
    assert!(a.contains(".if YARM_MUT_ROUNDS > 7\n    .set MSTRIDE, 0x100\n    .else\n    .set MSTRIDE, 0x40\n    .endif"));
    let fields: Vec<u64> = a
        .lines()
        .filter_map(|l| l.trim().strip_prefix(".set M_"))
        .filter_map(|l| l.split_once(", ").map(|(_, v)| v.trim()))
        .filter_map(|v| v.strip_suffix(" * MSTRIDE").and_then(|k| k.parse().ok()))
        .collect();
    assert_eq!(
        fields,
        (1..=12).collect::<Vec<u64>>(),
        "one stride per round-indexed field"
    );
    for (rounds, stride) in [(12u64, 0x100u64), (4, 0x40)] {
        assert!(
            (rounds + 1) * 8 <= stride,
            "{rounds} rounds fit a 0x{stride:x} stride"
        );
        assert!(
            12 * stride + (rounds + 1) * 8 <= 0x1000,
            "inside the mailbox page"
        );
    }
    let s = code(SMP2);
    assert!(squash(&s).contains(&squash(
        "#[cfg(feature = \"aarch64-lock2-witness\")] \
         kernel.copy_to_user(s_asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;"
    )));
}
