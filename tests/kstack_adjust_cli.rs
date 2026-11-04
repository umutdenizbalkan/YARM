// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD-CLOSURE1 §C — the kernel-image stack-adjustment check, through its real CLI.
//!
//! `scripts/check-kthread-stack-adjustments.py` bounds every single stack-pointer decrement in a
//! kernel image by `(KSTACK_STRIDE - 16 KiB - 4 KiB) / 2`, so a kernel thread that overflows always
//! faults in its guard gap. These cases feed it pre-decoded disassembly per ISA: decrements by an
//! immediate, by a constant built in a register (x86 32-bit moves into the 64-bit register,
//! AArch64 `mov`/`movk`, RISC-V `lui`/`addiw`), a decrement past the bound, and one whose amount
//! cannot be resolved — which must fail, never be skipped.

use std::process::Command;

fn check(arch: &str, fixture: &str) -> (bool, String) {
    let out = Command::new("python3")
        .args([
            "-I",
            "scripts/check-kthread-stack-adjustments.py",
            "--arch",
            arch,
            "--disassembly",
            &format!("tests/fixtures/kstack_adjust/{fixture}.dis"),
        ])
        .output()
        .expect("python3");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn decrements_within_the_bound_pass_on_every_isa() {
    for (arch, fixture, max) in [
        ("x86_64", "x86_ok", "max=0x100000"),
        ("aarch64", "aarch64_ok", "max=0x104000"),
        ("riscv64", "riscv_ok", "max=0xffff0"),
    ] {
        let (ok, out) = check(arch, fixture);
        assert!(ok, "{fixture}: {out}");
        assert!(
            out.contains(max) && out.contains("result=ok"),
            "{fixture}: {out}"
        );
    }
}

#[test]
fn a_decrement_past_the_bound_fails_on_every_isa() {
    for (arch, fixture) in [
        ("x86_64", "x86_big"),
        ("aarch64", "aarch64_big"),
        ("riscv64", "riscv_big"),
    ] {
        let (ok, out) = check(arch, fixture);
        assert!(!ok, "{fixture} must fail: {out}");
        assert!(
            out.contains("max=0x900000") && out.contains("result=fail"),
            "{fixture}: {out}"
        );
    }
}

#[test]
fn an_unresolvable_decrement_fails_instead_of_being_skipped() {
    let (ok, out) = check("x86_64", "x86_unresolved");
    assert!(!ok, "{out}");
    assert!(
        out.contains("KSTACK_ADJUST_UNRESOLVED") && out.contains("unresolved=1"),
        "{out}"
    );
}
