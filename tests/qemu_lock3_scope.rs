// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK3 — source guards for the x86_64 contention witness on `vm_state_lock` and the
//! reschedule IPI deferred through the holder's masked hold.
//!
//! QEMU's TCG cannot show where a record sits relative to the controller access it observes, that
//! the witness never writes an EOI or an ICR itself, that the lock path is bounded and console-free,
//! or that nothing in the kernel can open the interrupt window while the lock is held. These guards
//! pin those facts on comment-stripped source; the live behaviour is graded by
//! `scripts/qemu-x86_64-lock3-witness-smoke.sh`, and the grader by its `--self-test`.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const KMOD: &str = include_str!("../src/kernel/mod.rs");
const FACADE: &str = include_str!("../src/kernel/lock_witness.rs");
const LOCK: &str = include_str!("../src/kernel/lock.rs");
const WITNESS: &str = include_str!("../src/kernel/lock3_witness.rs");
const DESC: &str = include_str!("../src/arch/x86_64/descriptor_tables.rs");
const SMP: &str = include_str!("../src/arch/x86_64/smp.rs");
const IRQ: &str = include_str!("../src/arch/x86_64/irq.rs");
const TRAMP: &str = include_str!("../src/arch/x86_64/smp_trampoline.rs");
const UART: &str = include_str!("../src/arch/x86_64/uart_irq_witness.rs");
const SMP1: &str = include_str!("../src/arch/x86_64/smp1_witness.rs");
const SMP1_ASM: &str = include_str!("../src/arch/x86_64/smp1_witness.S");
const SPLIT: &str = include_str!("../src/kernel/syscall_split.rs");
const DEBUG: &str = include_str!("../src/kernel/syscall/debug.rs");
const SMOKE: &str = include_str!("../scripts/qemu-x86_64-lock3-witness-smoke.sh");
const SMP1_SMOKE: &str = include_str!("../scripts/qemu-x86_64-smp1-witness-smoke.sh");
const GRADER: &str = include_str!("../scripts/grade-x86_64-lock3-witness.py");

const GATE: &str = "#[cfg(feature = \"x86_64-lock3-witness\")]";

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

/// The witness-build body of one of the handler's splice macros (the raw string it expands to).
fn splice_body(name: &str) -> String {
    let d = DESC;
    let head = format!("macro_rules! {name} {{");
    let first = pos(d, &head);
    let gate = &d[..first];
    assert!(
        gate.rfind("feature = \"x86_64-lock3-witness\",")
            .is_some_and(|g| first - g < 160),
        "{name}'s first definition is the witness build's"
    );
    let body = &d[first..];
    let open = pos(body, "r#\"") + 3;
    let close = open + pos(&body[open..], "\"#");
    body[open..close].to_string()
}

/// Default off, co-enables SMP1 and the shared hook feature; the module and the facade route are
/// gated; no two of LOCK1, LOCK2, LOCK3 can be built together, and the internal feature alone is
/// refused.
#[test]
fn the_witness_is_gated_and_off_by_default() {
    assert!(ROOT_CARGO.contains("x86_64-lock3-witness = [\"x86-smp1-witness\", \"lock-witness\"]"));
    let default = ROOT_CARGO
        .split_once("default = [")
        .map(|(_, r)| r.split(']').next().unwrap_or(""))
        .unwrap_or("");
    assert!(!default.contains("lock3") && !default.contains("lock-witness"));
    assert!(squash(&code(KMOD)).contains(&squash(
        "#[cfg(feature = \"x86_64-lock3-witness\")] pub mod lock3_witness;"
    )));
    assert!(squash(&code(FACADE)).contains(&squash(
        "#[cfg(feature = \"x86_64-lock3-witness\")] pub use crate::kernel::lock3_witness::{ \
         VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released, };"
    )));
    for pair in [
        "riscv64-lock1-witness and aarch64-lock2-witness are mutually exclusive",
        "riscv64-lock1-witness and x86_64-lock3-witness are mutually exclusive",
        "aarch64-lock2-witness and x86_64-lock3-witness are mutually exclusive",
    ] {
        assert!(FACADE.contains(pair), "{pair}");
    }
    assert!(FACADE.contains("compile_error!(\"lock-witness is internal"));
    // The lock algorithm itself is the shared, unchanged one: the hooks are the facade's.
    let l = code(LOCK);
    assert!(l.contains("crate::kernel::lock_witness::maybe_hold(self.witness_id, token);"));
    assert!(!l.contains("lock3_witness"));
}

/// x86 masking, from source: `irq_save` is `pushfq; cli` and `irq_restore` executes `sti` only if
/// the save found `IF` set. Every other `sti` in the kernel is enumerated here — the idle windows
/// (`sti; hlt`), the boot enable, and the UART witness's restore of the flag it found — so a held
/// `SpinLockIrq` (entered with `IF = 0` in every syscall and interrupt path) can never be unmasked
/// by its own critical section. A new `sti` fails this guard until it is classified.
#[test]
fn the_held_lock_is_masked_and_every_sti_is_enumerated() {
    let irq = code(IRQ.split("#[cfg(test)]\nmod tests").next().unwrap());
    let save = code(fn_body(
        &irq,
        "#[cfg(not(feature = \"hosted-dev\"))]\npub fn irq_save() -> X86IrqState {",
    ));
    assert!(pos(&save, "\"pushfq\", \"pop {}\"") < pos(&save, "\"cli\""));
    let restore = code(fn_body(
        &irq,
        "#[cfg(not(feature = \"hosted-dev\"))]\npub fn irq_restore(state: X86IrqState) {",
    ));
    assert!(pos(&restore, "if !state.interrupts_were_enabled {") < pos(&restore, "\"sti\""));
    let count = |src: &str| code(src).matches("\"sti\"").count();
    // irq.rs: the boot enable and the conditional restore.
    assert_eq!(
        count(IRQ.split("#[cfg(test)]\nmod tests").next().unwrap()),
        2
    );
    // descriptor_tables.rs: the idle park loop's `sti; hlt`.
    assert_eq!(count(DESC), 1);
    assert!(code(DESC).contains("core::arch::asm!(\"sti\", \"hlt\", options(nomem, nostack));"));
    // smp.rs: the managed AP idle loop's `sti; hlt; cli`.
    assert_eq!(count(SMP), 1);
    assert!(
        code(SMP).contains("core::arch::asm!(\"sti\", \"hlt\", \"cli\", options(nomem, nostack));")
    );
    // smp_trampoline.rs: the two pre-scheduler AP idle loops (`sti` then `hlt`).
    assert_eq!(count(TRAMP), 2);
    // uart_irq_witness.rs: restores only the flag it found.
    assert_eq!(count(UART), 1);
    assert!(code(UART).contains("if was_enabled {"));
    // The LOCK3 code adds none.
    assert_eq!(count(WITNESS), 0);
    assert_eq!(count(SMP1), 0);
}

/// The 0xF1 handler's two records are spliced in at exactly two points — after the origin is known,
/// before the mailbox read; and right after the one EOI write, before the registers are restored —
/// and both macros expand to nothing in every other build. The bodies make no call, take no
/// acquisition (the only `lock`-prefixed instructions are counter increments and the slot claim),
/// never loop back, never write the EOI register (only the ISR/IRR words are read), and write only
/// the ring and its counters.
#[test]
fn the_handler_records_entry_and_eoi_without_calls_or_acquisitions() {
    let d = DESC;
    let stub = &d[pos(d, "yarm_ap_remote_wake_stub:")..];
    let stub = &stub[..pos(stub, "\n    iretq")];
    let entry = pos(stub, "lock3_wake_stub_entry!(),");
    let eoi = pos(stub, "lock3_wake_stub_eoi!(),");
    assert!(pos(stub, "\n95:\n") < entry && entry < pos(stub, "(4) coherent request read"));
    assert!(
        pos(stub, "mov dword ptr [rax], 0") < eoi && eoi < pos(stub, "(11) restore every register")
    );
    assert_eq!(stub.matches("lock3_wake_stub_").count(), 2);
    // Two definitions per macro: the witness body and the empty plain one.
    for name in ["lock3_wake_stub_entry", "lock3_wake_stub_eoi"] {
        assert_eq!(d.matches(&format!("macro_rules! {name} {{")).count(), 2);
        assert!(squash(d).contains(&squash(&format!(
            "not(feature = \"x86_64-lock3-witness\"), not(test), not(feature = \"hosted-dev\"), \
             target_arch = \"x86_64\" ))] macro_rules! {name} {{ () => {{ \"\" }}; }}"
        ))));
    }
    for name in ["lock3_wake_stub_entry", "lock3_wake_stub_eoi"] {
        let b = splice_body(name);
        for banned in [
            "call",
            "pause",
            "hlt",
            "sti",
            "cli",
            "iretq",
            "{lapic_eoi}]",
            "cr3",
        ] {
            assert!(!b.contains(banned), "{name} must not contain {banned}");
        }
        // Branches only forward (`NNf`), never back.
        for l in b.lines().map(str::trim) {
            if l.starts_with('j') {
                assert!(l.ends_with('f'), "{name}: backward branch `{l}`");
            }
            if l.starts_with("lock ") {
                assert!(
                    l.starts_with("lock add ") || l.starts_with("lock xadd "),
                    "{name}: `{l}` is not a counter increment or slot claim"
                );
            }
        }
        // Only the ISR (word 7) and IRR (word 7) are read from the LAPIC page.
        assert_eq!(b.matches("[rcx + 0x1C0]").count(), 1);
        assert_eq!(b.matches("[rcx + 0xC0]").count(), 1);
        assert!(b.contains("cmp byte ptr [rip + YARM_LOCK3_ARMED], 0"));
        assert!(b.contains("lock xadd dword ptr [rip + YARM_LOCK3_NEXT]"));
        // The slot is published last.
        let last_store = b.rfind("mov byte ptr [r").unwrap();
        assert!(
            b[last_store..].starts_with("mov byte ptr [rdi], 2")
                || b[last_store..].starts_with("mov byte ptr [rdx], 2")
        );
    }
    // The handler's interrupted RIP is read at the frame offset after its five pushes.
    assert!(splice_body("lock3_wake_stub_entry").contains("mov rdx, qword ptr [rsp + 40]"));
    // Word 7 holds the reschedule vector's bit only because the vector is 0xF1.
    assert!(d.contains("pub(crate) const AP_REMOTE_WAKE_VECTOR: u8 = 0xF1;"));
    assert!(WITNESS.contains("pub const RESCHED_VECTOR: u32 = 0xF1;"));
    // The ring the handler writes has the layout it assumes.
    for pin in [
        "#[repr(C)]\npub struct Slot {",
        "const _: () = assert!(core::mem::size_of::<Slot>() == 48);",
        "const _: () = assert!(core::mem::offset_of!(Slot, f) == 8);",
        "const _: () = assert!(core::mem::offset_of!(Slot, kind) == 1);",
        "const _: () = assert!(core::mem::offset_of!(Slot, hart) == 2);",
        "pub static YARM_LOCK3_SLOT: [Slot; SLOTS] = [EMPTY; SLOTS];",
        "pub const SLOTS: usize = 1024;",
        "pub const MAX_CPU: usize = 8;",
        "pub const K_ENTRY: u8 = 6;",
        "pub const K_EOI: u8 = 10;",
    ] {
        assert!(WITNESS.contains(pin), "{pin}");
    }
}

/// The witness never completes, raises or delivers an interrupt itself: no MMIO, no ICR or EOI
/// access, no handler invocation; the LAPIC is read through one helper that performs two reads and
/// nothing else; the only publications are the two production reschedule owners.
#[test]
fn the_witness_never_writes_eoi_or_icr_or_runs_the_handler() {
    let w = code(WITNESS);
    for banned in [
        "read_volatile",
        "write_volatile",
        "write_icr",
        "lapic_write_eoi",
        "acknowledge_interrupt",
        "external_irq_eoi",
        "yarm_ap_remote_wake_stub",
        "\"int ",
        "service_own_tlb_request",
        "tlb_request_shootdown",
    ] {
        assert!(!w.contains(banned), "the witness must not use {banned}");
    }
    assert_eq!(
        w.matches("crate::arch::x86_64::smp::send_reschedule_ipi_to(")
            .count(),
        1
    );
    assert_eq!(
        w.matches("crate::arch::x86_64::smp::c2c_send_reschedule_ipi_to(")
            .count(),
        1
    );
    let pubfn = code(fn_body(
        &w,
        "fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {",
    ));
    // Towards the BSP: NR7's owner; towards the AP: NR6's owner.
    assert!(
        pos(&pubfn, "if holder_cpu == 0 {")
            < pos(
                &pubfn,
                "c2c_send_reschedule_ipi_to(CpuId(waiter_cpu), CpuId(holder_cpu))"
            )
    );
    let s = code(SMP);
    let view = code(fn_body(
        &s,
        "pub(crate) fn lapic_vector_words(vector: u32) -> (u32, u32) {",
    ));
    assert_eq!(view.matches("read_volatile(").count(), 2);
    assert!(!view.contains("write"));
    assert!(view.contains("(base + 0x100 + word)") && view.contains("(base + 0x200 + word)"));
    assert_eq!(
        code(fn_body(&s, "pub(crate) fn lock3_icr_accepted() -> bool {")).trim(),
        "{\n    icr_delivery_idle()\n}"
    );
    // The ICR record sits in the ONE writer, after both register writes, gated.
    let icr = code(fn_body(&s, "fn write_icr(apic_id: u8, value: u32) {"));
    let low = pos(
        &icr,
        "write_volatile((base + LAPIC_ICR_LOW_OFFSET) as *mut u32, value);",
    );
    let hook = pos(
        &icr,
        "crate::kernel::lock3_witness::note_icr(apic_id, value);",
    );
    assert!(low < hook);
    assert!(squash(&icr[..hook]).contains(&squash(GATE)));
    assert_eq!(s.matches("lock3_witness::note_icr(").count(), 1);
    // The observers exist only in the witness build.
    for head in [
        "pub(crate) fn lapic_vector_words(vector: u32) -> (u32, u32) {",
        "pub(crate) fn lock3_icr_accepted() -> bool {",
    ] {
        let i = pos(&s, head);
        let attr = s[..i].rfind("#[cfg(").unwrap();
        assert_eq!(
            squash(&s[attr..i]),
            squash(
                "#[cfg(all(feature = \"x86_64-lock3-witness\", not(test), \
                 not(feature = \"hosted-dev\")))]"
            ),
            "{head} is gated"
        );
    }
}

/// Everything on the lock path, the ICR hook and the gate is bounded, lock-free, allocation-free
/// and console-free; console output lives only in `dump`.
#[test]
fn the_lock_and_interrupt_paths_are_bounded_and_console_free() {
    for head in [
        "pub fn note_contended(id: u32) {",
        "pub fn note_acquired(id: u32) -> u64 {",
        "pub fn note_released(id: u32, token: u64) {",
        "pub fn maybe_hold(id: u32, token: u64) {",
        "pub fn note_icr(apic: u8, low: u32) {",
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
        "pub fn note_round_ok(round: u64) {",
        "fn record(kind: u8, hart: u8, f: [u64; 5]) {",
    ] {
        let b = code(fn_body(WITNESS, head));
        for banned in [
            "printk", "print!", "format!", "alloc::", ".lock()", "Box::", "Vec::", "loop {",
        ] {
            assert!(!b.contains(banned), "{head} must not use {banned}");
        }
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

/// The hold: only after the holder left its own gate; the LAPIC view and TLB generation are read
/// and recorded BEFORE the waiter's gate is released, the contention is awaited, the view is read
/// again (bounded), and the masked state and both views are recorded inside the acquisition. The
/// kernel's outstanding flag is exactly the grader's predicate.
#[test]
fn the_hold_observes_the_lapic_before_and_after() {
    let h = code(fn_body(WITNESS, "pub fn maybe_hold(id: u32, token: u64) {"));
    let gated = pos(&h, "|| HOLDER_GATED.load(Ordering::Acquire) != round");
    let pre = pos(&h, "let (pre_isr, pre_irr) = view();");
    let begin = pos(&h, "K_BEGIN,");
    let held = pos(&h, "\n    HELD_ROUND.store(round, Ordering::Release);");
    let wait = pos(&h, "while seen <= baseline {");
    let post = pos(&h, "let (mut isr, mut irr) = view();");
    let rec_hold = pos(&h, "K_HOLD,");
    let rec_pend = pos(&h, "K_PENDING,");
    assert!(gated < pre && pre < begin && begin < held && held < wait && wait < post);
    assert!(post < rec_hold && rec_hold < rec_pend);
    assert!(squash(&h).contains(&squash(
        "pre_irr & VBIT == 0 && pre_isr & VBIT == 0 && irr & VBIT != 0 && isr & VBIT == 0 \
         && pre_tlb == post_tlb"
    )));
    assert!(h.contains("if_set()"));
}

/// The gate: the holder waits for the waiter to ARRIVE (its marker copy, which takes the VM lock, is
/// then done), then marks itself gated; the waiter first confirms its own last ICR write accepted,
/// marks arrival, waits for the holder inside its masked ownership, and only then publishes.
#[test]
fn the_gate_orders_arrival_ownership_and_publication() {
    let g = code(fn_body(
        WITNESS,
        "pub fn mut_round_gate(cpu: u8, round: u64) {",
    ));
    let holder_wait = pos(&g, "while WAITER_ARRIVED.load(Ordering::Acquire) < round {");
    let gated = pos(&g, "HOLDER_GATED.store(round, Ordering::Release);");
    let accepted = pos(&g, "let settled = u64::from(icr_accepted());");
    let arrive = pos(&g, "WAITER_ARRIVED.store(round, Ordering::Release);");
    let held_wait = pos(&g, "while HELD_ROUND.load(Ordering::Acquire) < round {");
    let publish = pos(&g, "publish_ipi_to_holder(cpu, holder, round);");
    assert!(holder_wait < gated && gated < accepted);
    assert!(accepted < arrive && arrive < held_wait && held_wait < publish);
}

/// The SMP1 workload carries the witness: armed with the provisioning, the two markers reach the
/// gate from the split DebugLog route only (never the broad route, which could hold a broad
/// acquisition), the dump follows the SMP1 summary, and the twelve-round count, the gate and the
/// post-NR 3 check are injected into the programs only in the witness build.
#[test]
fn the_smp1_hooks_and_round_count_are_gated() {
    let s = code(SMP1);
    for hook in [
        "crate::kernel::lock3_witness::arm();",
        "crate::kernel::lock3_witness::dump();",
        "crate::kernel::lock3_witness::mut_round_gate(this_cpu().0, round);",
        "crate::kernel::lock3_witness::note_round_ok(round);",
    ] {
        assert_eq!(s.matches(hook).count(), 1, "{hook}");
    }
    assert!(pos(&s, "\"SMP1_WITNESS_SUMMARY result=ok\"") < pos(&s, "lock3_witness::dump();"));
    assert!(squash(&s).contains(&squash(
        "#[cfg(all(not(feature = \"hosted-dev\"), not(feature = \"x86_64-lock3-witness\")))] \
         core::arch::global_asm!( \".equ YARM_SMP1_MUTUAL_ROUNDS, 4\\n.equ YARM_LOCK3, 0\\n\", \
         include_str!(\"smp1_witness.S\") );"
    )));
    assert!(squash(&s).contains(&squash(
        "#[cfg(all(not(feature = \"hosted-dev\"), feature = \"x86_64-lock3-witness\"))] \
         core::arch::global_asm!( \".equ YARM_SMP1_MUTUAL_ROUNDS, 12\\n.equ YARM_LOCK3, 1\\n\", \
         include_str!(\"smp1_witness.S\") );"
    )));
    assert!(SMP1_ASM.contains(".set SMP1_MUTUAL_ROUNDS, YARM_SMP1_MUTUAL_ROUNDS"));
    assert!(WITNESS.contains("pub const LOCK3_ROUNDS: u64 = 12;"));
    // The programs: gate, pattern, NR 3, result, the six callee-saved registers and the FXSAVE image,
    // and only then the round-done marker — all inside `.if YARM_LOCK3`.
    let a = SMP1_ASM;
    let l3 = pos(a, ".if YARM_LOCK3\n    /* The kernel's round gate");
    let seq = [
        "lea rdi, [rip + \\pfx\\()_m_l3_gate]",
        "\\pfx\\()_l3_gate_ret:",
        "SMP1_SET_CALLEE \\p",
        "SMP1_REPLACE_W \\capB",
        "\\pfx\\()_l3_nr3_ret:",
        "test ecx, ecx",
        "SMP1_CHECK_CALLEE \\p, \\pfx\\()_l3_ctx_fail",
        "call \\fpcheck",
        "lea rdi, [rip + \\pfx\\()_m_l3_done]",
    ];
    let mut at = l3;
    for step in seq {
        let i = at + pos(&a[at..], step);
        assert!(i >= at, "{step} in order");
        at = i;
    }
    // The split route dispatches the markers; the broad route does not.
    let sp = code(SPLIT);
    let hook = pos(
        &sp,
        "crate::arch::x86_64::smp1_witness::observe_lock3_marker(msg, frame.arg(2) as u64);",
    );
    assert!(
        sp[..hook]
            .rfind("feature = \"x86_64-lock3-witness\",")
            .is_some_and(|g| hook - g < 160)
    );
    assert!(!DEBUG.contains("lock3"));
}

/// The smoke builds the witness kernel over the plain base image set, records the identity, runs
/// the SMP1 grader on the same boot with twelve mutual rounds and the waiter's six publications per
/// direction, and hands the boot log to the independent grader, which re-derives everything through
/// the shared core.
#[test]
fn the_smoke_and_grader_are_wired() {
    assert!(SMOKE.contains("FEATURES=x86_64-lock3-witness"));
    assert!(SMOKE.contains("--no-default-features --features \"$FEATURES\""));
    assert!(SMOKE.contains("SMP1_MUTUAL_ROUNDS=12 SMP1_EXTRA_WAKES_01=6 SMP1_EXTRA_WAKES_10=6"));
    assert!(SMOKE.contains("python3 scripts/grade-x86_64-lock3-witness.py"));
    assert!(
        SMOKE.contains(
            "sha256sum \"$BUILD_DIR/kernel_boot.elf\" \"$BUILD_DIR/initramfs-core.cpio\""
        )
    );
    assert!(SMP1_SMOKE.contains("MUTUAL=${SMP1_MUTUAL_ROUNDS:-4}"));
    for call in [
        "transport(lines, \"LOCK3\", META_RX, fail, summary=(COUNTS_RX, \"cpu\"))",
        "acqs = ownership(ev, ROUNDS, fail)",
        "contention_outside_own(ev, acqs, ROUNDS, fail)",
        "contention_credit(r, W, hold, ev, acqs)",
        "MIN_PER_DIRECTION = 4",
    ] {
        assert!(GRADER.contains(call), "the grader applies {call}");
    }
    for check in [
        "contradicts its LAPIC views",
        "the publication's ICR write is not a fixed 0x%x IPI",
        "lies outside the holder's masked hold",
        "lies inside the masked acquisition",
        "stale or missing delivery",
        "not ring 3 at the",
        "has no EOI of its own",
        "arrival ordinal gap",
        "unbalanced handler obligations",
        "had not settled the 0xF1 arrival",
        "second-sender-in-window",
        "gate-timeout",
        "entries_with_tlb_work",
    ] {
        assert!(GRADER.contains(check), "the grader enforces: {check}");
    }
}
