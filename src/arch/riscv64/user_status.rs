// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Canonical 199E-R1F — the SINGLE definition of what `sstatus` may be when this kernel
//! returns to U-mode.
//!
//! # Why this module exists
//!
//! 199E-R1D made asynchronous preemption preserve the exact interrupted INTEGER context. It
//! could not make the same claim for floating-point or vector state, because there is nowhere
//! to put it: `RiscvTrapFrame` is 304 bytes of `x1..x31` plus `sepc`/`sstatus`/`scause`/`stval`,
//! and `UserRegisterContext` is `user_gprs: [usize; 32]`. Neither holds `f0..f31`, `fcsr`, or
//! any vector register.
//!
//! That was safe only by accident. The user target advertised the `lp64d` hard-float ABI, and
//! `sstatus.FS` arrived from OpenSBI reading **Dirty**, so the hardware permitted user
//! floating-point the whole time — nothing broke purely because no binary in the tree happened
//! to contain an FP instruction. "Happens not to" is not a policy, and a single `f64` appearing
//! in a server would have turned it into silent cross-task corruption: task A's live `f` state
//! discarded at a preemption, task B's stale values observed as A's.
//!
//! This module replaces the accident with a decision. Userspace is built soft-float (`lp64`,
//! no `F`/`D`/`V` target features), and **every** return to U-mode forces `FS = Off` and
//! `VS = Off`. An unsupported floating-point or vector instruction therefore raises an
//! illegal-instruction trap and fails closed through the ordinary per-task user-fault policy,
//! instead of executing against register state no one is saving.
//!
//! # Why forcing the bits is load-bearing, not belt-and-braces
//!
//! Neither half is sufficient alone:
//!
//! * building userspace soft-float only removes the instructions the *compiler* emits — it says
//!   nothing about hand-written `asm!`, a future dependency, or a corrupted/hostile image;
//! * forcing `FS`/`VS` Off only closes the hardware door — with a hard-float ABI the compiler
//!   would still emit FP instructions that now trap on the first `f64` in ordinary code.
//!
//! Together they are a consistent configuration: nothing generates FP/vector instructions, and
//! anything that does is refused at the hardware boundary rather than silently corrupting a
//! neighbour.
//!
//! # `VS`
//!
//! `sstatus.VS` (bits 10:9) is the vector-extension status field. Writing `Off` is correct on
//! both sides of the question: on an implementation with `V` it disables user vector state, and
//! on one without it the field is WARL and reads back zero. Live `sstatus` on the accepted QEMU
//! profile already shows bits 10:9 clear, so this pins an observed value rather than changing
//! behaviour there — but it pins it against a *different* CPU model rather than trusting one.
//!
//! # What is deliberately NOT here
//!
//! No FP/vector save area, no lazy-FPU or dirty-tracking scheme, and no emulation. Those are
//! full FP/vector context management, which is future work; this module is the fail-closed
//! policy that makes their absence honest.

/// `sstatus.SPP` (bit 8). Clear = `sret` returns to U-mode.
pub const SSTATUS_SPP: u64 = 1 << 8;
/// `sstatus.SPIE` (bit 5) — the interrupt-enable `sret` restores into `SIE`.
pub const SSTATUS_SPIE: u64 = 1 << 5;
/// `sstatus.SUM` (bit 18) — S-mode may access U-mode pages, which the trap bookkeeping needs.
pub const SSTATUS_SUM: u64 = 1 << 18;
/// `sstatus.FS` (bits 14:13) — floating-point unit status. `0` = Off.
pub const SSTATUS_FS_MASK: u64 = 0b11 << 13;
/// `sstatus.VS` (bits 10:9) — vector unit status. `0` = Off.
pub const SSTATUS_VS_MASK: u64 = 0b11 << 9;

/// Every bit this kernel CLEARS on the way back to U-mode.
///
/// `SPP` so `sret` targets U-mode; `FS`/`VS` so no user floating-point or vector state can
/// exist to be lost.
pub const USER_RETURN_CLEAR_MASK: u64 = SSTATUS_SPP | SSTATUS_FS_MASK | SSTATUS_VS_MASK;

/// Every bit this kernel SETS on the way back to U-mode.
///
/// `SUM` only. `SPIE` is deliberately absent: it is per-path policy (the first user entry
/// leaves interrupts masked; a resumed task carries whatever the trap saved), so it is left to
/// the caller rather than forced here.
pub const USER_RETURN_SET_MASK: u64 = SSTATUS_SUM;

/// The ONE transformation applied to an `sstatus` value that is about to `sret` into U-mode.
///
/// Takes the value the trap saved — or whatever a resume path assembled — and returns the only
/// shape this kernel will return to userspace with. Everything else in `sstatus` is preserved
/// untouched, so this is a sanitizer and not a rewrite: `SPIE`, `UXL` and the rest keep the
/// values the already-approved user-return paths gave them.
///
/// It is total and idempotent: applying it twice is applying it once, so a path that sanitizes
/// defensively cannot disagree with one that sanitizes once.
#[must_use]
pub const fn sanitize_user_sstatus(raw: u64) -> u64 {
    (raw & !USER_RETURN_CLEAR_MASK) | USER_RETURN_SET_MASK
}

/// `true` when `value` is a legal U-mode return `sstatus`: returning to U-mode, with both the
/// floating-point and vector units Off.
///
/// The predicate the guards and the focused tests assert against, so "what we sanitize to" and
/// "what we check for" cannot drift into two different definitions.
#[must_use]
pub const fn is_sanitized_user_sstatus(value: u64) -> bool {
    (value & SSTATUS_SPP) == 0
        && (value & SSTATUS_FS_MASK) == 0
        && (value & SSTATUS_VS_MASK) == 0
        && (value & SSTATUS_SUM) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The observed live value: SD set, UXL=2, SUM set, **FS = Dirty**. Exactly the value that
    /// must never reach U-mode.
    const OBSERVED_DIRTY: u64 = 0x8000_0002_0004_6000;

    #[test]
    fn a_saved_dirty_fs_can_never_escape_to_user_mode() {
        assert_ne!(
            OBSERVED_DIRTY & SSTATUS_FS_MASK,
            0,
            "the input really is Dirty"
        );
        let out = sanitize_user_sstatus(OBSERVED_DIRTY);
        assert_eq!(out & SSTATUS_FS_MASK, 0, "FS must be Off");
        assert_eq!(out & SSTATUS_VS_MASK, 0, "VS must be Off");
        assert_eq!(out & SSTATUS_SPP, 0, "SPP must select U-mode");
        assert_ne!(out & SSTATUS_SUM, 0, "SUM must survive");
        assert!(is_sanitized_user_sstatus(out));
    }

    #[test]
    fn every_fs_and_vs_encoding_is_forced_off() {
        for fs in 0..4u64 {
            for vs in 0..4u64 {
                let raw = (fs << 13) | (vs << 9) | SSTATUS_SPP;
                let out = sanitize_user_sstatus(raw);
                assert!(
                    is_sanitized_user_sstatus(out),
                    "FS={fs} VS={vs} was not sanitized"
                );
            }
        }
    }

    #[test]
    fn unrelated_status_bits_are_preserved_exactly() {
        // SPIE, UXL and SD are none of this sanitizer's business.
        let raw = SSTATUS_SPIE | (2 << 32) | (1 << 63) | SSTATUS_FS_MASK | SSTATUS_SPP;
        let out = sanitize_user_sstatus(raw);
        assert_ne!(
            out & SSTATUS_SPIE,
            0,
            "SPIE is per-path policy, not cleared here"
        );
        assert_eq!(out & (3 << 32), 2 << 32, "UXL preserved");
        assert_ne!(out & (1 << 63), 0, "SD preserved");
    }

    #[test]
    fn sanitization_is_idempotent() {
        let once = sanitize_user_sstatus(OBSERVED_DIRTY);
        assert_eq!(sanitize_user_sstatus(once), once);
    }

    /// The asm-side literals in `yarm_riscv64_enter_user` must equal these constants; the
    /// structural guard pins the asm text, and this pins the arithmetic it has to match.
    #[test]
    fn the_first_entry_masks_match_the_constants() {
        assert_eq!(USER_RETURN_CLEAR_MASK | SSTATUS_SPIE, 0x6720);
        assert_eq!(USER_RETURN_SET_MASK, 0x40000);
    }
}
