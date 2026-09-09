// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-VM-ENTRY1 §4 — the minimal userspace witness for NR 3, NR 13 and NR 14.
//!
//! None of the three had a live issuer. The whole user-visible syscall surface never called
//! `VmMap`, `VmAnonMap` or `VmBrk` from any server, so no profile on any architecture could have
//! executed the routes this mission converted, and a green boot would have proved nothing about
//! them. §4 authorizes a minimal userspace witness added to an existing profile for exactly this;
//! this module is it, and it is invoked once from an existing server binary rather than from a
//! new one, so no harness, image, packing rule or oracle inventory changes.
//!
//! ## What it does and does not claim
//!
//! Every line it prints is the result of a REAL trap: the syscall number and arguments go through
//! the architecture's own entry path, the ingress gate, the shared dispatcher and the route. It
//! reports the raw return lanes rather than a decoded verdict, so the evidence stays checkable.
//!
//! It deliberately does NOT assert. A witness that panicked would turn a diagnostic into a boot
//! dependency for every profile that shares this image; the oracles read its markers instead.
//!
//! ## The one case with no live issuer
//!
//! NR 3's SUCCESS path needs an `AddressSpace` capability, and no syscall returns one to
//! userspace — a spawn mints it into the spawner's cspace but reports only the child TID and the
//! packed send caps. So NR 3 is witnessed here through its authority branches (an unbacked slot,
//! and a slot holding an object that is not an address space), which are the only part of its
//! transaction that differs from NR 13's at all: everything after `resolve_map_target` is one
//! shared body. NR 3's mapping half is therefore reported as source-proven, not as executed, and
//! the two are never mixed.

use crate::arch::raw_syscall;

const SYSCALL_VM_MAP_NR: usize = 3;
const SYSCALL_VM_ANON_MAP_NR: usize = 13;
const SYSCALL_VM_BRK_NR: usize = 14;

const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;
const PROT_EXEC: usize = 0x4;

const PAGE: usize = 4096;

/// A window this witness owns outright: far above every server's image, heap and stack, and far
/// above any small integer an error lane could carry, so a success return is never mistakable for
/// an error code on the architectures that share one register for both.
const WITNESS_BASE: usize = 0x0000_2000_0000;

/// The raw lanes of one witnessed trap. `error` is x86_64's separate error register; on AArch64
/// and RISC-V the kernel writes the error code into the same register as `ret0`, so both are
/// printed and the marker stays readable on all three.
struct Lanes {
    ret0: usize,
    ret1: usize,
    error: usize,
}

fn trap(nr: usize, args: [usize; 6]) -> Lanes {
    // SAFETY: the architecture syscall ABI, with arguments this module chose. Every address it
    // passes is either inside the window it owns or deliberately invalid for a refusal case.
    let ret = unsafe { raw_syscall(nr, args) };
    Lanes {
        ret0: ret.ret0,
        ret1: ret.ret1,
        error: ret.error,
    }
}

fn note(nr: usize, case: &str, lanes: &Lanes) {
    crate::user_log!(
        "VM_ENTRY_WITNESS nr={} case={} ret0=0x{:x} ret1=0x{:x} err={}",
        nr,
        case,
        lanes.ret0,
        lanes.ret1,
        lanes.error
    );
}

/// Run the witness once. Bounded, allocation-free, and safe to call before a server's own work:
/// it touches only its own address window and its own break.
///
/// `non_aspace_cap` should be a capability the caller genuinely holds that is NOT an address
/// space — an endpoint from the startup context, say. It witnesses NR 3's `WrongObject` branch.
/// Pass `None` when the caller holds none and that branch is skipped rather than faked.
pub fn run_once(non_aspace_cap: Option<u32>) {
    crate::user_log!("VM_ENTRY_WITNESS_BEGIN");

    // ── NR 13 VmAnonMap ─────────────────────────────────────────────────────────────────────
    // Ordinary refusals first, so a failure to map later cannot be confused with them.
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "len_zero_refused",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [0, WITNESS_BASE, 0, PROT_READ | PROT_WRITE, 0, 0],
        ),
    );
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "unaligned_addr_refused",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [0, WITNESS_BASE + 1, PAGE, PROT_READ | PROT_WRITE, 0, 0],
        ),
    );
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "unknown_prot_refused",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [0, WITNESS_BASE, PAGE, PROT_READ | 0x8000, 0, 0],
        ),
    );
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "overflow_len_refused",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [0, WITNESS_BASE, usize::MAX - PAGE, PROT_READ, 0, 0],
        ),
    );

    // One page, read/execute — the shape that never trips the stack-guard rule.
    let single = trap(
        SYSCALL_VM_ANON_MAP_NR,
        [0, WITNESS_BASE, PAGE, PROT_READ | PROT_EXEC, 0, 0],
    );
    note(SYSCALL_VM_ANON_MAP_NR, "single_page", &single);

    // A multi-page run, so the transaction's per-page loop, its accounting and its shootdown
    // settlement all run more than once in a single call.
    let multi = trap(
        SYSCALL_VM_ANON_MAP_NR,
        [
            0,
            WITNESS_BASE + 16 * PAGE,
            4 * PAGE,
            PROT_READ | PROT_EXEC,
            0,
            0,
        ],
    );
    note(SYSCALL_VM_ANON_MAP_NR, "multi_page", &multi);

    // Re-map over a range this witness already owns: the DISPLACED-mapping commit path, where
    // each page replaces one and the run settles it through shootdown-then-reclaim.
    let remap = trap(
        SYSCALL_VM_ANON_MAP_NR,
        [
            0,
            WITNESS_BASE + 16 * PAGE,
            2 * PAGE,
            PROT_READ | PROT_EXEC,
            0,
            0,
        ],
    );
    note(SYSCALL_VM_ANON_MAP_NR, "remap_displacing", &remap);

    // The stack-guard rule, asked of a page whose predecessor this witness just mapped. A
    // writable, non-executable mapping immediately above a live page is the delivered refusal.
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "guard_page_refused",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [
                0,
                WITNESS_BASE + 17 * PAGE,
                PAGE,
                PROT_READ | PROT_WRITE,
                0,
                0,
            ],
        ),
    );

    // Writable data, at a base whose preceding page is unmapped — the guard rule does not fire.
    note(
        SYSCALL_VM_ANON_MAP_NR,
        "writable_page",
        &trap(
            SYSCALL_VM_ANON_MAP_NR,
            [
                0,
                WITNESS_BASE + 64 * PAGE,
                PAGE,
                PROT_READ | PROT_WRITE,
                0,
                0,
            ],
        ),
    );

    // ── NR 3 VmMap — the authority branches ─────────────────────────────────────────────────
    // A slot that resolves to nothing: `InvalidCapability`, produced before anything is acquired.
    note(
        SYSCALL_VM_MAP_NR,
        "invalid_capability_refused",
        &trap(
            SYSCALL_VM_MAP_NR,
            [
                0xFFFF_FFFF,
                WITNESS_BASE + 128 * PAGE,
                PAGE,
                PROT_READ | PROT_EXEC,
                0,
                0,
            ],
        ),
    );
    // A slot that resolves to something that is not an address space: `WrongObject`. This is the
    // ONE decision that distinguishes NR 3 from NR 13; everything after it is shared.
    if let Some(cap) = non_aspace_cap {
        note(
            SYSCALL_VM_MAP_NR,
            "wrong_object_refused",
            &trap(
                SYSCALL_VM_MAP_NR,
                [
                    cap as usize,
                    WITNESS_BASE + 128 * PAGE,
                    PAGE,
                    PROT_READ | PROT_EXEC,
                    0,
                    0,
                ],
            ),
        );
    } else {
        crate::user_log!("VM_ENTRY_WITNESS nr=3 case=wrong_object_skipped reason=no_cap_held");
    }
    crate::user_log!("VM_ENTRY_WITNESS nr=3 case=map_ok_no_live_issuer reason=no_aspace_cap_abi");

    // ── NR 14 VmBrk — every shape, attempted unconditionally ────────────────────────────────
    //
    // The query first: it changes nothing and reports the current end, which every later shape is
    // expressed relative to.
    //
    // NR 14's four non-query shapes need a break window that already exists — the delivered
    // handler's own rule, "growth requires pre-initialized brk bounds to avoid creating an empty
    // [base, end) window from unset state", preserved exactly. Each architecture's boot seeds one
    // for the supervisor, the process manager and the init server (`set_task_brk_bounds` in
    // `arch/{x86_64,aarch64,riscv64}/boot.rs`), and Fork copies a parent's; a task with none is
    // refused by the bounds lookup. This witness runs in the init server, which is seeded, so
    // every shape reaches its own body.
    //
    // Every shape is nevertheless ATTEMPTED unconditionally and its real answer recorded, so a
    // task without a window would produce the pre-window refusal in the log rather than a silent
    // skip, and the report can say which shapes executed instead of assuming.
    let query = trap(SYSCALL_VM_BRK_NR, [0, 0, 0, 0, 0, 0]);
    note(SYSCALL_VM_BRK_NR, "query", &query);
    let current_end = query.ret0;
    if current_end == 0 {
        crate::user_log!("VM_ENTRY_WITNESS nr=14 note=no_brk_window_exists_for_this_task");
    }
    // A page-aligned reference point, so the shapes below are well formed whether or not a window
    // exists. With no window every one of them is refused by the bounds lookup, which is itself
    // the delivered answer and is recorded as such.
    let anchor = if current_end == 0 {
        WITNESS_BASE + 256 * PAGE
    } else {
        current_end
    };
    let current_end = anchor;

    // Below the base — the refusal, before any shape is chosen.
    note(
        SYSCALL_VM_BRK_NR,
        "below_base_refused",
        &trap(SYSCALL_VM_BRK_NR, [1, 0, 0, 0, 0, 0]),
    );

    // Growth: bounds move up, pages stay lazy.
    let grown = current_end + 8 * PAGE;
    note(
        SYSCALL_VM_BRK_NR,
        "growth",
        &trap(SYSCALL_VM_BRK_NR, [grown, 0, 0, 0, 0, 0]),
    );

    // The no-op: the same value again, which still rewrites the bounds.
    note(
        SYSCALL_VM_BRK_NR,
        "no_op",
        &trap(SYSCALL_VM_BRK_NR, [grown, 0, 0, 0, 0, 0]),
    );

    // A shrink that stays inside one page: bounds only, nothing unmapped.
    note(
        SYSCALL_VM_BRK_NR,
        "shrink_within_page",
        &trap(SYSCALL_VM_BRK_NR, [grown - 8, 0, 0, 0, 0, 0]),
    );

    // A shrink that crosses pages: the one shape that unmaps, and the one the delivered split
    // route serviced only at a single CPU online.
    note(
        SYSCALL_VM_BRK_NR,
        "shrink_unmapping",
        &trap(SYSCALL_VM_BRK_NR, [current_end, 0, 0, 0, 0, 0]),
    );

    // And back to the query, so the end really did return to where it started.
    note(
        SYSCALL_VM_BRK_NR,
        "query_after",
        &trap(SYSCALL_VM_BRK_NR, [0, 0, 0, 0, 0, 0]),
    );

    crate::user_log!("VM_ENTRY_WITNESS_END");
}
