// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3-ACCEPTANCE — source guards for the overtaken-deferral settlement of the shared
//! x86_64/AArch64 bridge, its default-off live witness, and the pinned RISC-V firmware.
//!
//! Behaviour is executed elsewhere: the settlement by the hosted `overtaken_tests` interleavings
//! (through production owners), live by `scripts/qemu-{aarch64,x86_64}-overtaken-witness-smoke.sh`,
//! and the firmware identity by `scripts/qemu-riscv64-smp3-witness-smoke.sh`. These pin what those
//! cannot see: that EVERY failed class re-verify reaches the settlement, that the settlement cannot
//! return an unauthenticated continuation, and that the qualification runner cannot boot anything
//! but the pinned firmware.

const ROOT_CARGO: &str = include_str!("../Cargo.toml");
const BRIDGE: &str = include_str!("../src/arch/trap_entry.rs");
const RUNTIME: &str = include_str!("../src/runtime.rs");
const KERNEL_MOD: &str = include_str!("../src/kernel/mod.rs");
const WITNESS: &str = include_str!("../src/kernel/overtaken_witness.rs");
const SMP3_SH: &str = include_str!("../scripts/qemu-riscv64-smp3-witness-smoke.sh");
const SMP3_GRADER: &str = include_str!("../scripts/grade-riscv64-smp3-witness.py");
const FW_PIN: &str = include_str!("../scripts/firmware/riscv64-opensbi.pin");
const SBI: &str = include_str!("../src/arch/riscv64/sbi.rs");

fn code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn body_of<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("missing `{sig}`"));
    let rest = &src[start..];
    // A method ends at its four-space closing brace; a free function at column 0.
    let indent = src[..start].rsplit('\n').next().map(str::len).unwrap_or(0);
    let close = if indent == 0 { "\n}\n" } else { "\n    }\n" };
    let end = rest.find(close).unwrap_or(rest.len());
    &rest[..end]
}

/// Every blocking-class drain whose class re-verify fails reaches the settlement — the deferral is
/// cleared once, then settled. The D6 diagnostic observation is not a blocking-class deferral.
#[test]
fn every_failed_reverify_is_settled_not_fallen_through() {
    let c = code(BRIDGE);
    let sites = [
        (
            "D2_SEND_GENUINE_FALLBACK reason=state_changed",
            "d2_send_dispatch_clear(cpu_idx);",
            "\"d2_send_overtaken\"",
        ),
        (
            "D2_RECV_GENUINE_FALLBACK reason=state_changed",
            "d2_recv_dispatch_clear(cpu_idx);",
            "\"d2_recv_overtaken\"",
        ),
        (
            "\"QUEUE_ADVANCING_DISPATCH_DEFERRED reason=state_changed",
            "futex_wait_dispatch_clear(cpu_idx);",
            "\"futex_wait_overtaken\"",
        ),
        (
            "AARCH64_FUTEX_WAIT_DISPATCH_DEFERRED reason=state_changed",
            "futex_wait_dispatch_clear(cpu_idx);",
            "\"aarch64_futex_wait_overtaken\"",
        ),
        (
            "AARCH64_YIELD_DISPATCH_DEFERRED reason=state_changed",
            "yield_dispatch_clear(cpu_idx);",
            "\"aarch64_yield_overtaken\"",
        ),
        (
            "\"YIELD_DISPATCH_DEFERRED reason=state_changed",
            "yield_dispatch_clear(cpu_idx);",
            "\"yield_overtaken\"",
        ),
    ];
    for (marker, clear, site) in sites {
        let at = c
            .find(marker)
            .unwrap_or_else(|| panic!("missing branch `{marker}`"));
        let branch = &c[at..at + 900.min(c.len() - at)];
        let cl = branch
            .find(clear)
            .unwrap_or_else(|| panic!("`{marker}` does not clear its cell"));
        let st = branch
            .find("settle_overtaken_at_bridge(")
            .unwrap_or_else(|| panic!("`{marker}` falls through without settling"));
        assert!(
            cl < st,
            "`{marker}`: the cell is cleared exactly once, before the settlement"
        );
        assert!(
            branch[st..].contains(site),
            "`{marker}` settles under its own site {site}"
        );
    }
    assert_eq!(
        c.matches("reason=state_changed").count(),
        sites.len() + 1,
        "a new state_changed branch must be derived and settled here (the +1 is D6's observation)"
    );
}

/// The comments that justified the fall-through are gone: Runnable authorizes no return.
#[test]
fn no_text_claims_a_woken_task_resumes_through_the_entering_frame() {
    for stale in [
        "fall through so the trap returns to the re-runnable task",
        "the trap returns to the\n                // now-re-runnable task",
        "the trap returns to the now-re-runnable task",
    ] {
        assert!(
            !BRIDGE.contains(stale),
            "stale fall-through rationale: {stale:?}"
        );
    }
}

/// The ONE adapter applies every outcome, and the two contradictory ones diverge.
#[test]
fn the_bridge_applies_every_settlement_and_fails_closed_on_contradiction() {
    let adapter = body_of(BRIDGE, "fn settle_overtaken_at_bridge(");
    for arm in [
        "S::ReturnToInstalled { owner } =>",
        "S::Switch(token) =>",
        "S::Idle { reason } =>",
        "S::Unauthenticated(refusal) =>",
        "S::Torn { tid } =>",
    ] {
        assert!(adapter.contains(arm), "the adapter must apply `{arm}`");
    }
    let switch = &adapter[adapter.find("S::Switch(token) =>").unwrap()..];
    assert!(
        switch.contains("d2_resume_marked_incoming(shared, token, frame)")
            && switch.contains("direct_dispatch_rollback_split")
            && switch.contains("d2_resume_refused_fatal("),
        "a switch resumes by exact token, and a refused resume rolls back and diverges"
    );
    let unauth = &adapter[adapter.find("S::Unauthenticated(refusal) =>").unwrap()..];
    assert!(
        unauth[..unauth.find("S::Torn").unwrap()].contains("d2_resume_refused_fatal("),
        "an unauthenticated continuation is never returned to"
    );
    let idle = &adapter[adapter.find("S::Idle { reason } =>").unwrap()..];
    assert!(
        idle.contains("settle_post_lock_terminal_idle(")
            && idle.contains("enter_post_lock_idle_after_direct_dispatch("),
        "idle goes through each port's established terminal"
    );
    assert!(!adapter.contains("with_cpu("), "no broad acquisition");
    assert!(!adapter.contains("enqueue"), "no enqueue");
    assert!(
        !adapter.contains("set_ok(") && !adapter.contains("set_err("),
        "no manufactured result"
    );
}

/// The policy returns an installed continuation only through the frame-owner authentication, and
/// discharges an empty `current` only through the shared acquire.
#[test]
fn the_policy_authenticates_before_it_returns_and_owes_the_advance_otherwise() {
    let body = body_of(RUNTIME, "pub(crate) fn settle_overtaken_deferral_split(");
    let auth = body
        .find("FpuHomeExpectation::Owner(owner)")
        .expect("authenticate the frame's own owner");
    let ret = body
        .find("OvertakenSettlement::ReturnToInstalled { owner }")
        .expect("the installed return");
    assert!(auth < ret, "authentication precedes the return");
    assert!(
        body.contains(
            "return OvertakenSettlement::Unauthenticated(FpuHomeRefusal::UnownedContinuation);"
        ),
        "a frame with no owner is refused"
    );
    assert!(body.contains("queue_advance_acquire_incoming_split(authority, site, step)"));
    for forbidden in ["with_cpu(", "enqueue", "set_ok(", "loop {", "while "] {
        assert!(
            !body.contains(forbidden),
            "policy must not contain `{forbidden}`"
        );
    }
}

/// The witness is compile-time gated and default-off; production builds carry none of it.
#[test]
fn the_live_witness_is_default_off_and_gated() {
    let default = ROOT_CARGO
        .split("\ndefault = [")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("default features");
    for f in ["aarch64-overtaken-witness", "x86-overtaken-witness"] {
        assert!(
            ROOT_CARGO.contains(&format!("{f} = [")),
            "feature {f} declared"
        );
        assert!(!default.contains(f), "{f} must not be a default feature");
    }
    assert!(
        KERNEL_MOD.contains(
            "#[cfg(any(\n    feature = \"aarch64-overtaken-witness\",\n    feature = \"x86-overtaken-witness\"\n))]\npub mod overtaken_witness;"
        ),
        "the witness module is compiled only with its features"
    );
    let c = code(BRIDGE);
    for (gate, call) in [
        (
            "#[cfg(feature = \"x86-overtaken-witness\")]",
            "hold_before_futex_drain(shared, cpu, outgoing);",
        ),
        (
            "#[cfg(feature = \"aarch64-overtaken-witness\")]",
            "hold_before_futex_drain(shared, cpu, outgoing);",
        ),
    ] {
        assert!(c.contains(&format!("{gate}\n")), "gate {gate}");
        let _ = call;
    }
    assert_eq!(
        c.matches("hold_before_futex_drain(").count(),
        2,
        "one hold per FutexWait drain"
    );
    // The hold holds no lock of its own across the wait and is bounded.
    let hold = body_of(WITNESS, "pub fn hold_before_futex_drain(");
    assert!(
        hold.contains("if spins >= HOLD_POLLS"),
        "the hold is bounded"
    );
    assert!(
        !hold.contains(".lock()"),
        "the hold takes no lock across its wait"
    );
}

/// The RISC-V SMP3 qualification boots exactly the pinned firmware and fails on anything else.
#[test]
fn the_smp3_runner_boots_only_the_pinned_firmware() {
    assert!(
        !SMP3_SH.contains("\"-bios\", \"default\""),
        "no implicit firmware"
    );
    assert!(SMP3_SH.contains("\"-bios\", os.path.join(build, \"opensbi-fw_dynamic.bin\")"));
    assert!(SMP3_SH.contains("PIN=scripts/firmware/riscv64-opensbi.pin"));
    assert!(
        SMP3_SH.contains("reason=firmware_missing")
            && SMP3_SH.contains("reason=firmware_substituted")
    );
    assert!(
        SMP3_SH.contains("\"$PIN\" \"$LOGDIR/artifact-identity.txt\""),
        "the grader gets the pin and the record"
    );
    for field in [
        "path=",
        "sha256=",
        "provenance=",
        "banner=",
        "runtime_sbi=",
        "impl_id=",
        "impl_version=",
        "spec=",
    ] {
        assert!(
            FW_PIN.lines().any(|l| l.starts_with(field)),
            "pin field {field}"
        );
    }
    let sha = FW_PIN
        .lines()
        .find_map(|l| l.strip_prefix("sha256="))
        .unwrap();
    assert!(
        sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "a full sha256"
    );
    for check in [
        "the run recorded no firmware identity",
        "is not the pinned",
        "not the pinned implementation",
        "no firmware pin / artifact identity was given",
    ] {
        assert!(SMP3_GRADER.contains(check), "grader check: {check}");
    }
    assert!(
        SBI.contains("#[cfg(feature = \"riscv64-smp3-witness\")]\npub fn base_identity()"),
        "the firmware self-identification is witness-only"
    );
}
