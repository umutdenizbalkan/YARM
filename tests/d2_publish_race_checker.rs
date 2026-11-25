// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IPC-D2-RULE1 — the route-specific `D2_PUBLISH_RACE_UNWIND` rule (`doc/AI_AGENT_RULES.md` §14.3)
//! is enforced by ONE checker, `scripts/check-d2-publish-race.py`. These tests run it, so a hosted
//! `cargo test --tests` fails if the checker, its controls or its archived replays regress:
//!
//! * its self-test — the 33 controls retained from IPC-D2-PUBLISH1 (11 per archived trace), the
//!   orphan-companion, duplicate-publication and duplicate-commit controls on each real trace, and
//!   the synthetic interleaved-calls cases;
//! * the archived VFS-CORRECT1C SMP2 traces (verbatim extracts): verdict `ok`, every race recovered;
//! * the gates that must call it: every core and optional-fs smoke, and the fd witness grader.

use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn checker(args: &[&str]) -> (i32, String) {
    let out = Command::new("python3")
        .arg("-I")
        .arg(format!("{ROOT}/scripts/check-d2-publish-race.py"))
        .args(args)
        .output()
        .expect("python3 must be available — every smoke depends on it");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn fixtures() -> Vec<String> {
    ["x86_64", "aarch64", "riscv64"]
        .iter()
        .map(|p| format!("{ROOT}/scripts/fixtures/d2-publish-race/{p}.txt"))
        .collect()
}

#[test]
fn the_checker_self_test_passes_every_control() {
    let f = fixtures();
    let mut args = vec!["--self-test"];
    args.extend(f.iter().map(String::as_str));
    let (rc, out) = checker(&args);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("D2_PUBLISH_RACE_CHECK_SELF_TEST result=ok"),
        "{out}"
    );
    assert!(out.contains("fixtures=3"), "{out}");
}

#[test]
fn the_archived_smp2_races_replay_as_recovered() {
    for (f, races) in fixtures().iter().zip([3usize, 1, 2]) {
        let (rc, out) = checker(&["--quiet", f]);
        assert_eq!(rc, 0, "{f}: {out}");
        assert!(
            out.contains(&format!(
                "verdict=ok races={races} split={races} broad=0 recovered={races} "
            )),
            "{f}: {out}"
        );
    }
}

#[test]
fn every_gate_uses_the_one_checker() {
    for gate in [
        "scripts/qemu-x86_64-core-smoke.sh",
        "scripts/qemu-aarch64-core-smoke.sh",
        "scripts/qemu-riscv64-core-smoke.sh",
        "scripts/qemu-x86_64-optional-fs-smoke.sh",
        "scripts/qemu-aarch64-optional-fs-smoke.sh",
    ] {
        let src = std::fs::read_to_string(format!("{ROOT}/{gate}")).expect(gate);
        assert!(
            src.contains("check-d2-publish-race.py"),
            "{gate} must run the checker"
        );
        assert!(
            src.contains("D2 publish-race check verdict is not ok"),
            "{gate} must fail on it"
        );
    }
    let grader =
        std::fs::read_to_string(format!("{ROOT}/scripts/grade-vfs-fd-witness.py")).unwrap();
    assert!(
        grader.contains("check-d2-publish-race.py") && grader.contains("rep.verdict() != \"ok\"")
    );
}
