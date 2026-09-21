// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-PAGEFAULT1 §3 — the minimal userspace witness for **demand page-fault recovery**.
//!
//! # Why this has to exist
//!
//! §1 measured what the existing profiles actually produce and found that the only live
//! page-fault population on any port is x86_64 COW under `VM_COW=1`. `DemandCandidate` had **zero
//! witnesses on all three architectures**, which is exactly why `page_fault_route_for` routes it
//! `Broad` everywhere — and "no witness" is an evidence gap, not an architectural limit. Nothing
//! could be admitted for that class while nothing exercised it, because zero faults is zero
//! evidence.
//!
//! # The mechanism is entirely existing
//!
//! No new ABI, no new syscall, no capacity increase. `VmBrk` growth moves the break bounds up and
//! **leaves the pages lazy** — the delivered handler's own behaviour, already witnessed by
//! `vm_entry_witness`'s `growth` case. A task that then touches an address inside the grown
//! window takes a fault whose address is inside `[brk_base, brk_end)`, which is precisely the
//! first arm of `evaluate_demand_backed_region`. That is a `DemandCandidate` by construction,
//! produced by the production owner, not simulated.
//!
//! # What each round proves, and what it does not
//!
//! Each round stores a round-dependent sentinel through a lazy address and reads it straight back
//! in the same inline-asm block:
//!
//! * **the faulting instruction retried.** A store that was skipped, or one the kernel resumed
//!   PAST, leaves the slot holding something other than the sentinel. Reading the sentinel back
//!   means the store re-executed after the fault was recovered.
//! * **the source register survived.** The value read back is what the store's source operand
//!   held at retry, so a clobbered register file writes the wrong bytes rather than none.
//! * **the rest of the register file survived.** Four callee-saved registers are seeded with
//!   distinct sentinels, carried through the faulting block and compared on the far side, so a
//!   restore that SHUFFLES the file is caught as well as one that drops it. RISC-V names no
//!   registers here (`CALLEE_SAVED_CHECKED` is 0 on that port) and the value check stands alone
//!   — the seal reports `checked=` so a reader can tell the two cases apart rather than assuming.
//!
//! It proves nothing about which OWNER recovered the fault. That is the kernel-side marker's job,
//! and keeping the two separate is deliberate: this witness reports what userspace observed, and
//! the `PAGE_FAULT_*` / `VM_DEMAND_*` markers report who did it. An oracle correlates them.
//!
//! # It does not assert
//!
//! Like `vm_entry_witness`, a panic here would turn a diagnostic into a boot dependency for every
//! profile sharing this image. It emits markers and a seal; the smoke script reads them.

use crate::arch::raw_syscall;

/// `VmBrk`. The same NR the kernel's `SYSCALL_VM_BRK_NR` names.
const SYSCALL_VM_BRK_NR: usize = 14;

const PAGE: usize = 4096;

/// How many distinct lazy pages to fault on. Each round is one page and one fault, so this is
/// also the expected demand-fault count — small enough to stay well inside the smoke timeout and
/// well inside a grown window, large enough that a single lucky round cannot carry the result.
const ROUNDS: u32 = 8;

/// Run the witness once. Emits one marker per round and one seal.
///
/// A window this task does not own, or a growth the kernel refuses, is reported as such and the
/// witness stops — it never touches an address it has not been granted.
pub fn run_once() {
    // ── (1) Where this task's break currently ends ───────────────────────────────────────────
    //
    // The query shape: it mutates nothing and reports the current end. A task with no seeded
    // window answers 0, which is a real answer and is reported rather than worked around.
    // SAFETY: the query shape passes no addresses at all.
    let query = unsafe { raw_syscall(SYSCALL_VM_BRK_NR, [0, 0, 0, 0, 0, 0]) };
    let current_end = query.ret0;
    if current_end == 0 {
        crate::user_log!("PF1_DEMAND_WITNESS step=query result=no_window");
        crate::user_log!(
            "PF1_DEMAND_WITNESS_SEAL rounds=0 recovered=0 result=fail reason=no_window"
        );
        return;
    }
    crate::user_log!("PF1_DEMAND_WITNESS step=query end=0x{:x}", current_end);

    // ── (2) Grow the window, leaving the new pages lazy ──────────────────────────────────────
    //
    // One extra page beyond what the rounds use, so the last round's slot is comfortably inside
    // the window rather than on its edge.
    let grown = current_end + (ROUNDS as usize + 1) * PAGE;
    // SAFETY: the growth shape takes a page-aligned absolute end inside this task's own window.
    let grow = unsafe { raw_syscall(SYSCALL_VM_BRK_NR, [grown, 0, 0, 0, 0, 0]) };
    if grow.error != 0 {
        crate::user_log!(
            "PF1_DEMAND_WITNESS step=grow end=0x{:x} result=refused err={}",
            grown,
            grow.error
        );
        crate::user_log!(
            "PF1_DEMAND_WITNESS_SEAL rounds=0 recovered=0 result=fail reason=grow_refused"
        );
        return;
    }
    crate::user_log!(
        "PF1_DEMAND_WITNESS step=grow from=0x{:x} to=0x{:x} result=ok",
        current_end,
        grown
    );

    // ── (3) One fault per round ──────────────────────────────────────────────────────────────
    let mut recovered = 0u32;
    let mut value_bad = 0u32;
    let mut regs_ok = 0u32;
    let mut regs_bad = 0u32;
    let mut checked = 0u32;
    let mut first_bad_mask = 0u32;

    for round in 0..ROUNDS {
        // A DISTINCT page each round, so no round can be satisfied by a mapping an earlier round
        // installed — each one is a fresh demand fault rather than a hit on warm state.
        let addr = current_end + (round as usize) * PAGE;
        // Round-dependent in both halves, so a store that replayed an EARLIER round's operand is
        // caught as well as one that wrote nothing.
        let value = 0xD3_0000_0000u64 | ((round as u64) << 16) | 0xA5;
        let sentinel = 0x5115_0000_0000u64 | ((round as u64) << 8) | 0x5A;

        // SAFETY: `addr` is inside the window grown above — `[current_end, grown)` — and is page
        // aligned, so it is `u64`-aligned. The fault it takes is a demand fault by construction.
        let (read_back, mask, n) = unsafe {
            crate::syscall::touch_demand_page_checking_callee_saved(addr, value, sentinel)
        };
        checked = n;

        let value_ok = read_back == value;
        if value_ok {
            recovered += 1;
        } else {
            value_bad += 1;
        }
        let all_regs = if n == 0 { 0 } else { (1u32 << n) - 1 };
        if mask == all_regs {
            regs_ok += 1;
        } else {
            regs_bad += 1;
            if first_bad_mask == 0 {
                first_bad_mask = mask | 0x8000_0000;
            }
        }
        crate::user_log!(
            "PF1_DEMAND_WITNESS round={} addr=0x{:x} retried={} mask=0x{:x} checked={}",
            round,
            addr,
            u8::from(value_ok),
            mask,
            n
        );
    }

    // ── (4) The seal ─────────────────────────────────────────────────────────────────────────
    //
    // `recovered` is the load-bearing number: it counts rounds whose faulting store retried and
    // landed with the right operand. `regs_*` reports the register-file half separately, because
    // on RISC-V `checked=0` makes it vacuous and a reader must be able to see that rather than
    // read a passing mask as proof.
    let ok = recovered == ROUNDS && value_bad == 0 && regs_bad == 0;
    crate::user_log!(
        "PF1_DEMAND_WITNESS_SEAL rounds={} recovered={} value_bad={} regs_ok={} regs_bad={} \
         checked={} first_bad=0x{:x} result={}",
        ROUNDS,
        recovered,
        value_bad,
        regs_ok,
        regs_bad,
        checked,
        first_bad_mask,
        if ok { "ok" } else { "fail" }
    );
}
