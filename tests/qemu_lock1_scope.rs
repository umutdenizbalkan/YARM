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
/// The architecture-neutral half every lock-witness grader imports (transport, ownership, contention).
const CORE: &str = include_str!("../scripts/lock_witness_core.py");
const IPI: &str = include_str!("../src/arch/riscv64/ipi.rs");

/// The `SpinLockIrq` hooks are gated on the internal `lock-witness` feature that LOCK1 (and LOCK2)
/// enable, and reach the building architecture's witness through the `lock_witness` facade.
const GATE: &str = "#[cfg(feature = \"lock-witness\")]";
const FACADE: &str = include_str!("../src/kernel/lock_witness.rs");
/// The RISC-V-only hooks (SMP3 workload, mailbox owners) stay under the LOCK1 feature itself.
const LOCK1_GATE: &str = "#[cfg(feature = \"riscv64-lock1-witness\")]";

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
    assert!(
        ROOT_CARGO.contains("riscv64-lock1-witness = [\"riscv64-smp3-witness\", \"lock-witness\"]")
    );
    // The facade hands `SpinLockIrq` THIS module's hooks under the LOCK1 feature, and refuses the
    // internal feature on its own or together with the AArch64 witness.
    assert!(squash(&code(FACADE)).contains(&squash(
        "#[cfg(feature = \"riscv64-lock1-witness\")] pub use crate::kernel::lock1_witness::{ \
         VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released, };"
    )));
    assert!(FACADE.contains(
        "compile_error!(\"riscv64-lock1-witness and aarch64-lock2-witness are mutually exclusive\")"
    ));
    assert!(FACADE.contains("compile_error!(\"lock-witness is internal"));
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
        "#[cfg(feature = \"lock-witness\")]\nwitness_id: 0,"
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
        "crate::kernel::lock_witness::note_contended(self.witness_id);",
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
    assert_eq!(c.matches("lock_witness::note_contended(").count(), 1);
    assert_eq!(c.matches("lock_witness::note_acquired(").count(), 1);
    assert_eq!(c.matches("lock_witness::maybe_hold(").count(), 1);
    assert_eq!(c.matches("lock_witness::note_released(").count(), 1);
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
        "crate::kernel::lock_witness::note_acquired(self.witness_id);",
    );
    let hold = pos(
        &body,
        "crate::kernel::lock_witness::maybe_hold(self.witness_id, token);",
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
        "crate::kernel::lock_witness::note_released(self.lock.witness_id, self.witness_token);",
    );
    let store = pos(&drop, "self.lock.held.0.store(false, Ordering::Release);");
    assert!(
        rel < store,
        "release is recorded before the lock becomes acquirable"
    );
    assert!(squash(&drop).contains(&squash("if self.lock.witness_id != 0 {")));
    // QEMU-LOCK1-SEAL: the acquisition token note_acquired returned travels in the guard to the
    // release, so each release record names exactly the acquisition it ends; the guard field exists
    // only under the feature.
    assert!(squash(&body).contains(&squash(
        "let token = crate::kernel::lock_witness::note_acquired(self.witness_id);"
    )));
    assert!(squash(&body).contains(&squash(
        "#[cfg(feature = \"lock-witness\")] witness_token, _not_send: PhantomData,"
    )));
    assert!(squash(&code(LOCK)).contains(&squash(
        "#[cfg(feature = \"lock-witness\")] witness_token: u64,"
    )));
    // Both hooks sit under the feature gate. acquire + hold share one gated `if self.witness_id != 0`
    // block, so maybe_hold sits a few lines below the gate; the threshold allows that block but still
    // catches a hook with no gate above it.
    for (hook, src) in [("acquire/hold", &body), ("release", &drop)] {
        let c: &str = src;
        for (i, _) in c.match_indices("crate::kernel::lock_witness::") {
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
        b[site..].contains("crate::kernel::lock_witness::VM_LOCK_ID"),
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
        "pub fn note_acquired(id: u32) -> u64 {",
        "pub fn note_released(id: u32, token: u64) {",
        "pub fn maybe_hold(id: u32, token: u64) {",
        "pub fn note_arrival(cpu: u8, sources: u64) {",
        "pub fn note_publication(sender: u8, target: u8) {",
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
    let b = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32, token: u64) {"));
    // It only designates the one holder, once per round, for a recorded acquisition.
    assert!(b.contains("if me != HOLDER_CPU.load(Ordering::Acquire) {"));
    assert!(b.contains("if id != VM_LOCK_ID || token == 0 || !ACTIVE.load(Ordering::Acquire) {"));
    assert!(b.contains("if LAST_HELD_ROUND.load(Ordering::Acquire) >= round {"));
    // The wait is on CONTENTION_SEQ (the atomic indication the waiter bumps), and it breaks on the
    // bound — it releases even if the contender never arrives.
    let wait = pos(&b, "while seen <= baseline {");
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
    // Holder records and returns immediately; the waiter waits on HELD_ROUND (stored after the
    // holder's CAS), records whether that held, and only THEN publishes — so the published work is
    // born while the holder owns the lock with interrupts masked.
    let holder_ret = pos(&b, "if cpu == holder {");
    let gate_wait = pos(&b, "while HELD_ROUND.load(Ordering::Acquire) < round {");
    let gate_rec = pos(
        &b,
        "record(K_GATE, cpu, [round, u64::from(holder), 1, ok, 0]);",
    );
    let publish = pos(&b, "publish_ipi_to_holder(cpu, holder, round);");
    assert!(
        holder_ret < gate_wait && gate_wait < gate_rec && gate_rec < publish,
        "holder returns; waiter waits, records its gate, then publishes"
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
                .rfind(LOCK1_GATE)
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
        CORE.contains("if not mm or fnv1a(mm.group(1)) != int(mm.group(3), 16):"),
        "the grader checks every LOCK1 line's crc (metadata, records, completion)"
    );
    assert!(
        GRADER.contains("corroborating_seal(lines, \"SMP3_VERDICT\", fail)")
            && CORE.contains("fail(\"the %s seal did not pass: \" % name"),
        "the grader requires the SMP3 seal"
    );
    assert!(GRADER.contains("from lock_witness_core import"));
    assert!(GRADER.contains("meta, recs, _ = transport(lines, \"LOCK1\", META_RX, fail)"));
    // Transport accounting (shared core): the declared count, the completion record, the
    // contiguous sequence, and the same-boot seal's intact, non-conflicting verdict.
    for check in [
        "no intact %s_META line",
        "conflicting checksum-valid %s_META copies",
        "the dump is incomplete: no intact %s_DUMP_DONE completion record",
        "conflicting checksum-valid %s_DUMP_DONE copies",
        "inconsistent counts: %s_META declares %d records, %s_DUMP_DONE %d",
        "missing records: %d of the %d declared have no intact copy",
        "records beyond the declared count",
        "no intact %s verdict on this boot",
        "conflicting checksum-valid %s verdicts",
        "conflicting checksum-valid copies of seq",
    ] {
        assert!(CORE.contains(check), "the shared core enforces: {check}");
    }
    // Arrival ordering: after the release record of the acquisition the pending observation named.
    assert!(GRADER.contains("precedes the release record (seq %s) of acquisition"));
    // The fixtures go through the same transport validation: genuine metadata and completion.
    assert!(GRADER.contains(
        "meta = \"LOCK1_META vm_lock_id=%d rounds=%d slots_used=%d overflow=0 dump_cpu=0\" % ("
    ));
    assert!(GRADER.contains(
        "done = \"LOCK1_DUMP_DONE records=%d\" % (n if done_count is None else done_count)"
    ));
    assert!(
        !GRADER.contains("crc=0x0\""),
        "no fixture line carries a placeholder checksum"
    );
    assert!(
        GRADER.contains("MIN_PER_DIRECTION = 4"),
        "contention and attributed delivery are each required per direction"
    );
    // The ownership model and value-linked contention (shared core) ...
    for check in [
        "a lock event names lock",
        "overlapping owners",
        "release with no preceding acquisition",
        "duplicate acquisition id",
        "does not match the owning acquisition",
        "was never released",
        "recorded contention while it owned the lock",
        "no waiter contention record produced",
    ] {
        assert!(CORE.contains(check), "the shared core enforces: {check}");
    }
    for call in [
        "acqs = ownership(ev, ROUNDS, fail)",
        "contention_outside_own(ev, acqs, ROUNDS, fail)",
        "contention_credit(r, W, hold, ev, acqs)",
    ] {
        assert!(GRADER.contains(call), "the LOCK1 grader applies {call}");
    }
    // ... and LOCK1's own attributed-delivery obligations.
    for check in [
        "has no complete record",
        "the hold falls outside its acquisition",
        "holder was not masked (sstatus.SIE set) inside the critical section",
        "did not swap out the waiter's bit",
        "which was never armed there",
        "the consumption's generation is stale",
        "was never discharged",
        "overwrote round",
        "attributed masked deliveries",
    ] {
        assert!(GRADER.contains(check), "the grader enforces: {check}");
    }
    // The smoke builds the feature and hands the boot log to that grader.
    assert!(SMOKE.contains("--features riscv64-lock1-witness"));
    assert!(SMOKE.contains("python3 scripts/grade-riscv64-lock1-witness.py"));
}

/// §3 — attributed delivery at the production mailbox. The publication owner advances the
/// (target, sender) generation BEFORE it sets the bit; the masked holder positively reads its own
/// mailbox and that generation inside its ownership and arms a link only if the waiter's bit is
/// outstanding (never silently overwriting an unresolved link); the consumption owner hands the
/// witness the sources it actually swapped out, and the link is discharged only by a consumption
/// that removed the linked sender's bit. The hooks observe; they change no mailbox semantics.
#[test]
fn the_ipi_work_is_attributed_at_the_production_mailbox() {
    let w = code(WITNESS);
    for k in [
        "pub const K_ARRIVAL: u8 = 6;",
        "pub const K_PENDING: u8 = 7;",
        "pub const K_GATE: u8 = 8;",
        "pub const K_DONE: u8 = 9;",
        "pub const K_LINKLOST: u8 = 10;",
    ] {
        assert!(w.contains(k), "{k}");
    }
    // Publication: generation before bit, after every refusal.
    let sr = code(fn_body(
        IPI,
        "pub fn send_reschedule(sender: CpuId, target: CpuId) -> Result<(), IpiRefusal> {",
    ));
    let refusal = pos(&sr, "let slot = PENDING.get(t).ok_or(IpiRefusal::NoHart)?;");
    let genr = pos(
        &sr,
        "crate::kernel::lock1_witness::note_publication(sender.0, target.0);",
    );
    let publish = pos(&sr, "let old = slot.fetch_or(bit, Ordering::AcqRel);");
    assert!(refusal < genr && genr < publish);
    assert!(
        sr[..genr]
            .rfind(LOCK1_GATE)
            .is_some_and(|g| sr[g..genr].matches('\n').count() <= 2)
    );
    // The masked positive observation and the arming.
    let hold = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32, token: u64) {"));
    let rec_hold = pos(&hold, "K_HOLD,");
    let read = pos(
        &hold,
        "let outstanding = (mailbox(me) >> (u64::from(sender) & 63)) & 1;",
    );
    let seen = pos(&hold, "let seen_gen = pub_gen(me, sender);");
    let rec_pend = pos(&hold, "K_PENDING,");
    let no_arm = pos(&hold, "if outstanding == 0 {");
    let arm = pos(
        &hold,
        "cell.swap(encode_link(round, sender, seen_gen), Ordering::AcqRel)",
    );
    let lost = pos(&hold, "record(K_LINKLOST, me,");
    assert!(rec_hold < read && read < seen && seen < rec_pend && rec_pend < no_arm);
    assert!(
        no_arm < arm && arm < lost,
        "arm only when outstanding; record an overwritten link"
    );
    // The mailbox read is the production non-consuming observer.
    assert!(w.contains("crate::arch::riscv64::ipi::pending(crate::kernel::scheduler::CpuId(cpu))"));
    // Consumption: discharge only on the linked sender's bit in the swapped sources.
    let na = code(fn_body(
        WITNESS,
        "pub fn note_arrival(cpu: u8, sources: u64) {",
    ));
    assert!(
        na.contains("if link == 0 {"),
        "a CPU with nothing armed records nothing"
    );
    let disc = pos(
        &na,
        "let discharged = (sources >> (u64::from(sender) & 63)) & 1;",
    );
    let clear = pos(&na, "if discharged == 1 {");
    assert!(disc < clear && na.contains("record(\n        K_ARRIVAL,"));
    // The production consumption owner passes the ACTUAL swapped sources, after the swap.
    let ta = code(fn_body(
        IPI,
        "pub fn take_arrival(cpu: CpuId, origin: ArrivalOrigin) -> IpiArrival {",
    ));
    let swap = pos(&ta, "p.swap(0, Ordering::AcqRel)");
    let note = pos(
        &ta,
        "crate::kernel::lock1_witness::note_arrival(cpu.0, sources);",
    );
    let ret = pos(&ta, "IpiArrival { origin, sources }");
    assert!(
        swap < note && note < ret,
        "after the production consume, before return"
    );
    let gate = ta[..note].rfind(LOCK1_GATE);
    assert!(gate.is_some_and(|g| ta[g..note].matches('\n').count() <= 2));
    // The dump names the new kinds.
    let dump = code(fn_body(WITNESS, "pub fn dump() {"));
    for n in [
        "\"arrival\"",
        "\"pending\"",
        "\"gate\"",
        "\"done\"",
        "\"linklost\"",
    ] {
        assert!(dump.contains(n), "dump names {n}");
    }
}
