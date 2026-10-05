// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK1 — source guards for the real subdomain-lock contention witness.
//!
//! These pin PLACEMENT, ORDER and SCOPE on the one witnessed production lock (`vm_state_lock`),
//! which neither a hosted test nor QEMU's TCG can execute: TCG runs both harts' atomics in host
//! order, so a boot cannot prove that the contention record is derived from the lock's own atomic
//! flag, that the witness touches exactly one `SpinLockIrq` instance, that the hold hook is bounded
//! and releases even if the contender never arrives, or that the waiter still contends through the
//! unchanged production acquisition path. Live behaviour is graded by
//! `scripts/qemu-riscv64-lock1-witness-smoke.sh`, and the grader itself is checked by
//! `scripts/grade-riscv64-lock1-witness.py --self-test`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const LOCK: &str = include_str!("../src/kernel/lock.rs");
const WITNESS: &str = include_str!("../src/kernel/lock1_witness.rs");
const BOOTSTRAP: &str = include_str!("../src/kernel/boot/bootstrap_state.rs");
const KMOD: &str = include_str!("../src/kernel/mod.rs");
const SMP3_WITNESS: &str = include_str!("../src/arch/riscv64/smp3_witness.rs");
const SMP3_RECORD: &str = include_str!("../src/kernel/boot/smp3_record.rs");
const SMOKE: &str = include_str!("../scripts/qemu-riscv64-lock1-witness-smoke.sh");
const GRADER: &str = include_str!("../scripts/grade-riscv64-lock1-witness.py");
const IPI: &str = include_str!("../src/arch/riscv64/ipi.rs");

const GATE: &str = "#[cfg(feature = \"riscv64-lock1-witness\")]";

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

fn squash(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

fn pos(src: &str, needle: &str) -> usize {
    src.find(needle)
        .unwrap_or_else(|| panic!("{needle} missing"))
}

/// The witness is a feature, off by default, that co-enables the SMP3 workload it rides on. The
/// witness module compiles only under that feature.
#[test]
fn the_witness_is_gated_and_off_by_default() {
    assert!(ROOT_CARGO.contains("riscv64-lock1-witness = [\"riscv64-smp3-witness\"]"));
    let default = ROOT_CARGO
        .split_once("default = [")
        .map(|(_, r)| r.split(']').next().unwrap_or(""))
        .unwrap_or("");
    assert!(!default.contains("riscv64-lock1-witness"));
    assert!(squash(&code(KMOD)).contains(&squash(
        "#[cfg(feature = \"riscv64-lock1-witness\")] pub mod lock1_witness;"
    )));
}

/// The `SpinLockIrq` identity field exists ONLY under the witness feature, so the default build's
/// layout and acquisition path are byte-for-byte unchanged; `new` sets it to 0.
#[test]
fn the_identity_field_is_feature_gated_and_defaults_to_zero() {
    let c = code(LOCK);
    let field = pos(&c, "witness_id: u32,");
    assert!(
        c[..field].rfind(GATE).is_some_and(|g| field - g < 120),
        "the witness_id field sits directly under the feature gate"
    );
    // `SpinLockIrq::new` — the production constructor every other lock uses — stamps id 0. This
    // pattern (the gate immediately above `witness_id: 0`) occurs only in that constructor.
    assert!(squash(&c).contains(&squash(
        "#[cfg(feature = \"riscv64-lock1-witness\")]\nwitness_id: 0,"
    )));
}

/// The contention record is derived from the lock's OWN atomic flag — recorded the first time the
/// acquisition observes `held` already set, inside the production spin-on-load loop — not from
/// elapsed time or a global counter. It fires once per `lock()` and only for a non-zero id.
#[test]
fn contention_is_observed_from_the_held_flag_once() {
    let body = code(fn_body(
        LOCK,
        "pub fn lock(&self) -> SpinLockIrqGuard<'_, T> {",
    ));
    // The observation lives inside the inner `while self.held.0.load(...)` spin, i.e. the lock was
    // seen held by someone else.
    let spin = pos(&body, "while self.held.0.load(Ordering::Relaxed) {");
    let contended = pos(
        &body,
        "crate::kernel::lock1_witness::note_contended(self.witness_id);",
    );
    let cas = pos(
        &body,
        "compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)",
    );
    assert!(
        spin < contended && contended < cas,
        "contention is recorded from the held flag, before the CAS"
    );
    // Guarded on a non-zero id and a once-per-call latch.
    assert!(squash(&body).contains(&squash("if self.witness_id != 0 && !observed_held {")));
    assert!(squash(&body).contains(&squash("observed_held = true;")));
    // Exactly one call site each for the three lock-path hooks in lock.rs.
    let c = code(LOCK);
    assert_eq!(c.matches("lock1_witness::note_contended(").count(), 1);
    assert_eq!(c.matches("lock1_witness::note_acquired(").count(), 1);
    assert_eq!(c.matches("lock1_witness::maybe_hold(").count(), 1);
    assert_eq!(c.matches("lock1_witness::note_released(").count(), 1);
}

/// Acquire and the (default-off) hold hook run AFTER the successful CAS, while the lock is held;
/// release is recorded BEFORE the atomic store makes the lock acquirable, so the holder's release
/// is causally before any waiter's acquire. Every hook is behind the gate and a non-zero id.
#[test]
fn acquire_holds_and_releases_in_causal_order() {
    let body = code(fn_body(
        LOCK,
        "pub fn lock(&self) -> SpinLockIrqGuard<'_, T> {",
    ));
    let cas_ok = pos(&body, ".is_ok()\n            {");
    let acq = pos(
        &body,
        "crate::kernel::lock1_witness::note_acquired(self.witness_id);",
    );
    let hold = pos(
        &body,
        "crate::kernel::lock1_witness::maybe_hold(self.witness_id);",
    );
    let ret = pos(&body, "return SpinLockIrqGuard {");
    assert!(
        cas_ok < acq && acq < hold && hold < ret,
        "acquire then hold, both while held, before the guard returns"
    );
    assert!(squash(&body).contains(&squash("if self.witness_id != 0 {")));
    // The guard's Drop records the release before the releasing store.
    let drop = code(fn_body(
        LOCK,
        "impl<T> Drop for SpinLockIrqGuard<'_, T> {\n    fn drop(&mut self) {",
    ));
    let rel = pos(
        &drop,
        "crate::kernel::lock1_witness::note_released(self.lock.witness_id);",
    );
    let store = pos(&drop, "self.lock.held.0.store(false, Ordering::Release);");
    assert!(
        rel < store,
        "release is recorded before the lock becomes acquirable"
    );
    assert!(squash(&drop).contains(&squash("if self.lock.witness_id != 0 {")));
    // Both hooks sit under the feature gate. acquire + hold share one gated `if self.witness_id != 0`
    // block, so maybe_hold sits a few lines below the gate; the threshold allows that block but still
    // catches a hook with no gate above it.
    for (hook, src) in [("acquire/hold", &body), ("release", &drop)] {
        let c: &str = src;
        for (i, _) in c.match_indices("crate::kernel::lock1_witness::") {
            let g = c[..i].rfind(GATE);
            assert!(
                g.is_some_and(|g| c[g..i].matches('\n').count() <= 4),
                "{hook}: a hook is not gated"
            );
        }
    }
}

/// Exactly ONE lock instance is witnessed: `vm_state_lock` is built with `new_witnessed(.., VM_LOCK_ID)`,
/// and that constructor is used exactly once in the whole tree.
#[test]
fn exactly_one_lock_is_witnessed() {
    assert_eq!(
        code(LOCK).matches("pub const fn new_witnessed(").count(),
        1,
        "one witnessed constructor"
    );
    // The one call site is the VM address-space lock, under the gate.
    let b = code(BOOTSTRAP);
    assert_eq!(
        b.matches("SpinLockIrq::new_witnessed(").count(),
        1,
        "one witnessed instance in the tree"
    );
    let site = pos(&b, "SpinLockIrq::new_witnessed(");
    assert!(
        b[site..].contains("crate::kernel::lock1_witness::VM_LOCK_ID"),
        "it is stamped with VM_LOCK_ID"
    );
    assert!(
        b[..site].rfind(GATE).is_some_and(|g| site - g < 200),
        "the witnessed init is gated"
    );
    // It guards the VM address space; the witness module documents that instance.
    assert!(
        b[..site].contains("vm_state_lock"),
        "the witnessed lock is vm_state_lock"
    );
    assert!(WITNESS.contains("KernelState::vm_state_lock") && WITNESS.contains("rank 5"));
}

/// The whole lock path in the witness module is bounded, lock-free and console-free: every hook
/// touches only statics, takes no other lock, never allocates and never prints. Console I/O lives
/// solely in `dump`, which runs after both witness tasks finish.
#[test]
fn the_lock_path_is_bounded_lock_free_and_console_free() {
    // The functions that run on the acquisition / release / gate path.
    for head in [
        "pub fn note_contended(id: u32) {",
        "pub fn note_acquired(id: u32) {",
        "pub fn note_released(id: u32) {",
        "pub fn maybe_hold(id: u32) {",
        "pub fn note_arrival(cpu: u8) {",
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
        "fn record(kind: u8, hart: u8, f: [u64; 5]) {",
    ] {
        let b = code(fn_body(WITNESS, head));
        for banned in [
            "printk", "print!", "format!", "alloc::", ".lock()", "Box::", "Vec::",
        ] {
            assert!(
                !b.contains(banned),
                "{head} must not use {banned} on the lock path"
            );
        }
    }
    // Console output appears only in dump (twice: the two synchronous emits).
    let c = code(WITNESS);
    assert_eq!(
        c.matches("printk_emit_sync(").count(),
        1,
        "one synchronous emit closure, in dump"
    );
    let emit = pos(&c, "printk_emit_sync(");
    let dump = pos(&c, "pub fn dump() {");
    assert!(dump < emit, "the only console output is inside dump");
}

/// The hold hook is bounded by finite spin budgets, waits only on the atomic contention indication
/// (never on held-lock work, interrupt delivery, a syscall or a remote ACK), and releases when the
/// budget elapses even if the contender never arrives.
#[test]
fn the_hold_hook_is_bounded_and_releases_regardless() {
    // Finite, named bounds.
    for bound in [
        "const HOLD_SPINS: u64",
        "const GATE_SPINS: u64",
        "const SSIP_SPINS: u64",
    ] {
        assert!(WITNESS.contains(bound), "{bound} is a finite constant");
    }
    let b = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32) {"));
    // It only designates the one holder, once per round.
    assert!(b.contains("if this_cpu() != HOLDER_CPU.load(Ordering::Acquire) {"));
    assert!(b.contains("if LAST_HELD_ROUND.load(Ordering::Acquire) >= round {"));
    // The wait is on CONTENTION_SEQ (the atomic indication the waiter bumps), and it breaks on the
    // bound — it releases even if the contender never arrives.
    let wait = pos(
        &b,
        "while CONTENTION_SEQ.load(Ordering::Acquire) <= baseline {",
    );
    let brk = pos(&b[wait..], "if left == 0 {\n            break;");
    assert!(brk < b[wait..].len(), "the hold wait is bounded");
    // It observes sip.SSIP (a pending bit the firmware set) but never clears or waits on delivery:
    // SIE is read, not written, anywhere in the hook.
    assert!(
        b.contains("let (_, p) = sie_ssip();"),
        "the pending bit is only read"
    );
    assert!(
        !b.contains("csrw") && !b.contains("csrs ") && !b.contains("csrc "),
        "no CSR writes in the hold hook"
    );
    // Nothing in the hook mentions a syscall completion, ACK or delivery wait.
    for banned in ["ack", "delivery", "complete", "syscall"] {
        assert!(
            !b.to_lowercase().contains(banned),
            "the hold hook must not wait on {banned}"
        );
    }
}

/// The waiter contends through the UNCHANGED production acquisition path: the gate never acquires or
/// bypasses the lock itself. The holder returns at once; the waiter publishes the §3 IPI through the
/// production send owner, then waits — bounded — only on the atomic `HELD_ROUND`.
#[test]
fn the_waiter_uses_the_production_acquisition_path() {
    let b = code(fn_body(
        WITNESS,
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
    ));
    // The gate must not touch the lock directly — it only sets up the race and publishes the IPI.
    for banned in [".lock()", "vm_state_lock", "held.0", "compare_exchange"] {
        assert!(
            !b.contains(banned),
            "the gate must not touch the lock ({banned})"
        );
    }
    // Holder returns immediately; waiter publishes then waits on HELD_ROUND.
    let holder_ret = pos(&b, "if cpu == holder {");
    let publish = pos(&b, "publish_ipi_to_holder(cpu, holder, round);");
    let gate_wait = pos(&b, "while HELD_ROUND.load(Ordering::Acquire) < round {");
    assert!(
        holder_ret < publish && publish < gate_wait,
        "holder returns, waiter publishes then waits"
    );
    assert!(
        b.contains("if left == 0 {\n            break;"),
        "the gate wait is bounded"
    );
    // The IPI goes through the production reschedule owner (the SMP3 send/mailbox owner), not a
    // bespoke firmware call.
    let ipi = code(fn_body(
        WITNESS,
        "fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {",
    ));
    assert!(ipi.contains(
        "crate::arch::riscv64::ipi::send_reschedule(CpuId(waiter_cpu), CpuId(holder_cpu))"
    ));
    assert!(
        !ipi.contains("sbi::") && !ipi.contains("send_ipi("),
        "the witness never calls the firmware directly"
    );
}

/// The round count is feature-gated so the plain SMP3 build is byte-identical (MUT_ROUNDS stays 4)
/// while the LOCK1 build runs 12 — six contended rounds per direction. The injected asm constant,
/// the record module and the witness module all agree.
#[test]
fn the_round_count_is_gated_consistently() {
    assert!(SMP3_WITNESS.contains(".equ YARM_MUT_ROUNDS, 4\\n"));
    assert!(SMP3_WITNESS.contains(".equ YARM_MUT_ROUNDS, 12\\n"));
    let rec = code(SMP3_RECORD);
    assert!(squash(&rec).contains(&squash(
        "#[cfg(not(feature = \"riscv64-lock1-witness\"))]\npub const MUT_ROUNDS: u64 = 4;"
    )));
    assert!(squash(&rec).contains(&squash(
        "#[cfg(feature = \"riscv64-lock1-witness\")]\npub const MUT_ROUNDS: u64 = 12;"
    )));
    assert!(WITNESS.contains("pub const LOCK1_ROUNDS: u64 = 12;"));
    // The witness's arm() and dump() hang off the SMP3 provision/dump, under the gate.
    let sw = code(SMP3_WITNESS);
    for hook in [
        "crate::kernel::lock1_witness::arm();",
        "crate::kernel::lock1_witness::dump();",
        "crate::kernel::lock1_witness::mut_round_gate(",
        "crate::kernel::lock1_witness::note_round_ok(",
    ] {
        let i = pos(&sw, hook);
        assert!(
            sw[..i]
                .rfind(GATE)
                .is_some_and(|g| sw[g..i].matches('\n').count() <= 2),
            "{hook} is gated"
        );
    }
}

/// The dump seals each record twice with its own checksum, and the independent grader re-derives
/// every credited round from the raw `LOCK1_REC` lines and additionally requires the SMP3 seal to
/// pass on the same boot. The smoke script builds with the feature and runs that grader.
#[test]
fn the_dump_is_sealed_and_the_grader_is_independent() {
    // Two checksummed passes, a per-record crc, and an overflow flag the grader can see.
    let dump = code(fn_body(WITNESS, "pub fn dump() {"));
    assert!(
        dump.contains("for pass in 1..=2u32 {"),
        "every line is emitted twice"
    );
    assert!(dump.contains("crc=0x{:08x}") && dump.contains("fnv1a(line.as_bytes())"));
    assert!(dump.contains("LOCK1_META") && dump.contains("overflow={}"));
    assert!(dump.contains("LOCK1_REC seq=") && dump.contains("LOCK1_DUMP_DONE records="));
    // The grader validates the crc, requires the SMP3 seal, and re-derives the chain per round.
    assert!(
        GRADER.contains("if fnv1a(body) != int(mm.group(10), 16):"),
        "the grader checks the record crc"
    );
    assert!(
        GRADER.contains("fail(\"the SMP3 seal did not pass: \""),
        "the grader requires the SMP3 seal"
    );
    assert!(
        GRADER.contains("MIN_PER_DIRECTION = 4") && GRADER.contains("MIN_DELIVERY_ROUNDS = 2"),
        "the grader keeps the per-direction and delivery-chain thresholds"
    );
    // The acceptance grader rejects the holes the old subsequence search admitted: a wrong lock, a
    // waiter release before its acquire, an IPI published after the interval, an unmasked holder, a
    // hold not covered by an un-released acquisition, and conflicting checksum-valid copies.
    for check in [
        "a lock event names lock",
        "the hold is not covered by an un-released holder acquisition",
        "no production IPI was published to the holder during the interval",
        "holder was not masked (sstatus.SIE set) inside the critical section",
        "the contender never released after it acquired",
        "conflicting checksum-valid copies of seq",
        "rounds with the full publish->masked->arrival delivery chain",
    ] {
        assert!(GRADER.contains(check), "the grader enforces: {check}");
    }
    // The smoke builds the feature and hands the boot log to that grader.
    assert!(SMOKE.contains("--features riscv64-lock1-witness"));
    assert!(SMOKE.contains("python3 scripts/grade-riscv64-lock1-witness.py"));
}

/// §3 — the delivery end of the IPI chain. The holder arms a per-CPU pending-arrival round at the
/// end of its (still masked) hold; the production arrival owner `take_arrival` records the matching
/// `K_ARRIVAL` once, after the holder unmasks, linking publish → masked-pending → delivery by round
/// identity. The hook observes the production consumption and takes nothing from it.
#[test]
fn the_arrival_hook_observes_the_production_consumption() {
    let w = code(WITNESS);
    assert!(w.contains("pub const K_ARRIVAL: u8 = 6;"));
    // The stash is armed at the end of the hold hook, while still masked.
    let hold = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32) {"));
    assert!(
        hold.contains("ARRIVAL_ROUND.get(me as usize)") && hold.contains("cell.store(round,"),
        "the hold hook arms the per-CPU pending-arrival round"
    );
    // note_arrival discharges it once (swap to 0) and records K_ARRIVAL; it is bounded and lock-free.
    let na = code(fn_body(WITNESS, "pub fn note_arrival(cpu: u8) {"));
    assert!(
        na.contains("cell.swap(0, Ordering::AcqRel)"),
        "one-shot discharge of the armed round"
    );
    assert!(
        na.contains("record(K_ARRIVAL, cpu,"),
        "records the arrival for the armed round"
    );
    assert!(
        na.contains("if round == 0 {"),
        "a CPU with nothing armed records nothing"
    );
    // The production arrival owner calls it, under the feature gate, after it has consumed the
    // mailbox (the hook observes; it does not alter the production consumption).
    let ta = code(fn_body(
        IPI,
        "pub fn take_arrival(cpu: CpuId, origin: ArrivalOrigin) -> IpiArrival {",
    ));
    let swap = pos(&ta, "p.swap(0, Ordering::AcqRel)");
    let note = pos(&ta, "crate::kernel::lock1_witness::note_arrival(cpu.0);");
    let ret = pos(&ta, "IpiArrival { origin, sources }");
    assert!(
        swap < note && note < ret,
        "the hook runs after the production consume, before return"
    );
    let gate = ta[..note].rfind(GATE);
    assert!(
        gate.is_some_and(|g| ta[g..note].matches('\n').count() <= 2),
        "the arrival hook sits under the feature gate"
    );
    // The dump names the new kind.
    assert!(code(fn_body(WITNESS, "pub fn dump() {")).contains("K_ARRIVAL => \"arrival\","));
}
