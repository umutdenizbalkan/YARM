// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! BL5c — a QEMU core run is bound to the artifacts it boots.
//!
//! A strict core smoke used to boot whatever its artifact paths held: an initramfs built without
//! `timer-contract-witness` booted, ran to its timeout and failed late on the witness's absence,
//! and nothing tied a strict pass to the kernel and initramfs it judged. The artifact build now
//! records each artifact's role, hash and features, and each core smoke checks that binding before
//! QEMU starts — refusing, when strict, an artifact set it cannot bind or one lacking a feature
//! its mode requires.
//!
//! The behavioural tests run the real library (`scripts/lib/artifact-binding.sh`) and the real
//! manifest writer with `bash`; the source contract pins where each smoke calls them.

use std::process::Command;

const BINDING: &str = include_str!("../scripts/lib/artifact-binding.sh");
const COMMON: &str = include_str!("../scripts/lib/build-qemu-artifacts-common.sh");
const X86: &str = include_str!("../scripts/qemu-x86_64-core-smoke.sh");
const AARCH64: &str = include_str!("../scripts/qemu-aarch64-core-smoke.sh");
const RISCV64: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");
const WORKFLOW: &str = include_str!("../.github/workflows/core-qemu-smoke.yml");
const FINAL_SEAL: &str = include_str!("../scripts/qemu-ipccall-reply-direct-x86_64-final-seal.sh");
const LATE_REPLY: &str = include_str!("../scripts/qemu-riscv64-late-reply-yield-smoke.sh");

fn bash(script: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg("binding")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("bash must be available — the smokes themselves depend on it");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn the_library_self_test_accepts_every_bound_set_and_refuses_every_unbound_one() {
    let (ok, text) = bash("bash scripts/lib/artifact-binding.sh --self-test", &[]);
    assert!(ok, "{text}");
    assert!(
        text.contains(
            "[ok] ARTIFACT-BINDING self-test passed (7 accepted, 11 refused, 4 feature records)"
        ),
        "{text}"
    );
}

/// The REAL manifest writer, driven exactly as an artifact build drives it, produces a manifest
/// the REAL binding check accepts for that build and refuses for a mismatched one.
#[test]
fn the_build_manifest_binds_its_own_artifacts_and_records_their_features() {
    let dir = std::env::temp_dir().join(format!("yarm-bl5c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = r#"
set -euo pipefail
source scripts/lib/build-qemu-artifacts-common.sh
d="$1"
build() { # DIR FEATURE_ARGS
  mkdir -p "$1"
  printf 'kernel %s\n' "$2" >"$1/kernel_boot.elf"
  printf 'initramfs %s\n' "$2" >"$1/initramfs-core.cpio"
  BUILD_COMMIT=test BUILD_START_HUMAN=now BUILD_START_EPOCH=0 ARTIFACT_MANIFEST="$1/artifact-manifest.txt" \
    BOOTSTRAP_FEATURE_ARGS="$2" common_write_manifest x86_64 "$1/kernel_boot.elf" "$1/initramfs-core.cpio" >/dev/null
}
build "$d/w" "--no-default-features --features timer-contract-witness"
build "$d/p" "--no-default-features"
cat "$d/w/artifact-manifest.txt"
artifact_binding_check x86_64 1 "$d/w/kernel_boot.elf" "$d/w/initramfs-core.cpio" initramfs:timer-contract-witness && echo VERDICT_W=accept || echo VERDICT_W=refuse
artifact_binding_check x86_64 1 "$d/p/kernel_boot.elf" "$d/p/initramfs-core.cpio" initramfs:timer-contract-witness && echo VERDICT_P=accept || echo VERDICT_P=refuse
artifact_binding_check x86_64 0 "$d/p/kernel_boot.elf" "$d/p/initramfs-core.cpio" && echo VERDICT_P0=accept || echo VERDICT_P0=refuse
artifact_binding_check x86_64 1 "$d/w/kernel_boot.elf" "$d/p/initramfs-core.cpio" && echo VERDICT_MIX=accept || echo VERDICT_MIX=refuse
"#;
    let (ok, text) = bash(script, &[dir.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{text}");
    assert!(
        text.contains("build_feature_args=--no-default-features --features timer-contract-witness")
    );
    assert!(text.contains("/kernel_boot.elf sha256="));
    assert!(text.contains(" role=kernel default_features=0 features=timer-contract-witness\n"));
    assert!(text.contains(" role=initramfs default_features=0 features=timer-contract-witness\n"));
    assert!(text.contains("VERDICT_W=accept"), "{text}");
    assert!(text.contains("VERDICT_P=refuse"), "{text}");
    assert!(text.contains(
        "this run requires the initramfs to be built with 'timer-contract-witness'; it was built with features=none"
    ));
    assert!(text.contains("refusing to boot"));
    assert!(text.contains("VERDICT_P0=accept"), "{text}");
    assert!(text.contains("VERDICT_MIX=refuse"), "{text}");
    assert!(text.contains("is not the initramfs"));
}

fn before(src: &str, first: &str, then: &str) -> bool {
    match (src.find(first), src.find(then)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

#[test]
fn every_core_smoke_binds_its_artifacts_before_it_boots_them() {
    for (src, call) in [
        (
            X86,
            "artifact_binding_check x86_64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" \"${artifact_required[@]}\" || exit 1",
        ),
        (
            AARCH64,
            "artifact_binding_check aarch64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" || exit 1",
        ),
        (
            RISCV64,
            "artifact_binding_check riscv64 \"$QEMU_SMOKE_STRICT\" \"$KERNEL_IMAGE\" \"$INITRAMFS_IMAGE\" || exit 1",
        ),
    ] {
        assert_eq!(src.matches(call).count(), 1, "{call}");
        assert!(
            before(src, call, "-kernel \"$KERNEL_IMAGE\""),
            "bound before QEMU: {call}"
        );
        assert!(
            src.contains("source \"$(dirname \"${BASH_SOURCE[0]}\")/lib/artifact-binding.sh\"")
        );
    }
}

#[test]
fn a_strict_x86_64_run_requires_the_witness_of_the_initramfs_that_carries_it() {
    let req = X86
        .split("artifact_required=()")
        .nth(1)
        .unwrap()
        .split("artifact_binding_check")
        .next()
        .unwrap();
    assert!(req.contains("if [[ \"$QEMU_SMOKE_STRICT\" == \"1\" ]]; then"));
    assert!(req.contains("artifact_required+=(\"initramfs:timer-contract-witness\")"));
    // The live witness check stays: a bound witness artifact whose witness never runs is a
    // regression, not an artifact mismatch.
    assert!(X86.contains("timer contract witness absent"));
}

#[test]
fn the_manifest_records_role_and_features_per_artifact() {
    let w = COMMON
        .split("common_write_manifest() {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    assert!(w.contains("feat=\"$(artifact_features_record \"${BOOTSTRAP_FEATURE_ARGS:-}\")\""));
    assert!(w.contains("artifact_manifest_line \"$kernel\" kernel \"$feat\""));
    assert!(w.contains("artifact_manifest_line \"$initramfs\" initramfs \"$feat\""));
    assert!(BINDING.contains("printf 'artifact %s sha256=%s size=%s mtime=%s role=%s %s\\n'"));
}

#[test]
fn strict_runners_build_and_bind_compatible_artifacts() {
    let strict_job = WORKFLOW
        .split("x86_64-core-smoke-strict:")
        .nth(1)
        .unwrap()
        .split("aarch64-core-smoke:")
        .next()
        .unwrap();
    assert!(strict_job.contains(
        "BOOTSTRAP_FEATURE_ARGS: \"--no-default-features --features timer-contract-witness\""
    ));
    assert!(FINAL_SEAL.contains(
        "BOOTSTRAP_FEATURE_ARGS=\"--no-default-features --features timer-contract-witness\""
    ));
    assert!(FINAL_SEAL.contains("ARTIFACT_MANIFEST=build-x86_64/artifact-manifest.txt"));
    assert!(LATE_REPLY.contains(
        "artifact_write_run_manifest \"$LOGDIR/artifact-binding.txt\" riscv64 \"$KBIN\""
    ));
    assert!(LATE_REPLY.contains("ARTIFACT_MANIFEST=\"$LOGDIR/artifact-binding.txt\""));
}
