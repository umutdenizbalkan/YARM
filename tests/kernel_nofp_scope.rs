// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KERNEL-NOFP1 — source guards for the kernel's no-FP/SIMD build rule.
//!
//! What these pin is configuration and inventory, which no hosted test can execute: the kernel
//! targets stay soft-float while the user targets keep their FP/SIMD; every artifact build runs
//! the final-image check and fails closed; every audited exception the sources record is a kind
//! the checker's policy knows, and every policy kind is recorded somewhere; every region the
//! sources record is an entry of the checker's reviewed exclusion inventory owned by that source,
//! and every inventory entry is recorded by its owner; on AArch64 FP/SIMD is assembled only
//! inside bracketed ranges. The final-image behaviour itself — every executable byte either
//! decoded or excluded by an anchored inventory entry, every FP/SIMD instruction inside an audited
//! range — is `scripts/check-kernel-nofp.py`, run on the built images; its behaviour on synthetic
//! images is `tests/kernel_nofp_cli.rs`.

use std::collections::BTreeSet;

const X86_KERNEL_TARGET: &str = include_str!("../targets/x86_64-yarm-none.json");
const A64_KERNEL_TARGET: &str = include_str!("../targets/aarch64-yarm-none.json");
const X86_USER_TARGET: &str = include_str!("../targets/x86_64-yarm-user-none.json");
const A64_USER_TARGET: &str = include_str!("../targets/aarch64-yarm-user-none.json");
const RV_USER_TARGET: &str = include_str!("../targets/riscv64-yarm-user-none.json");
const CARGO_CONFIG: &str = include_str!("../.cargo/config.toml");
const CHECKER: &str = include_str!("../scripts/check-kernel-nofp.py");
const BINDING: &str = include_str!("../scripts/lib/artifact-binding.sh");
const BUILD_X86: &str = include_str!("../scripts/build-qemu-x86_64-artifacts.sh");
const BUILD_A64: &str = include_str!("../scripts/build-qemu-aarch64-artifacts.sh");
const BUILD_RV: &str = include_str!("../scripts/build-qemu-riscv64-artifacts.sh");
const ARCH_MOD: &str = include_str!("../src/arch/mod.rs");

/// Every source that records an audited exception or region.
const RECORDING_SOURCES: &[(&str, &str)] = &[
    (
        "x86_64/descriptor_tables.rs",
        include_str!("../src/arch/x86_64/descriptor_tables.rs"),
    ),
    (
        "x86_64/context_switch.rs",
        include_str!("../src/arch/x86_64/context_switch.rs"),
    ),
    ("x86_64/boot.rs", include_str!("../src/arch/x86_64/boot.rs")),
    (
        "x86_64/smp_trampoline.rs",
        include_str!("../src/arch/x86_64/smp_trampoline.rs"),
    ),
    (
        "x86_64/context_witness.rs",
        include_str!("../src/arch/x86_64/context_witness.rs"),
    ),
    (
        "x86_64/smp1_witness.S",
        include_str!("../src/arch/x86_64/smp1_witness.S"),
    ),
    (
        "x86_64/apsched_witness.S",
        include_str!("../src/arch/x86_64/apsched_witness.S"),
    ),
    (
        "x86_64/overtaken_witness.S",
        include_str!("../src/arch/x86_64/overtaken_witness.S"),
    ),
    (
        "x86_64/vmi2_witness.S",
        include_str!("../src/arch/x86_64/vmi2_witness.S"),
    ),
    (
        "aarch64/boot.rs",
        include_str!("../src/arch/aarch64/boot.rs"),
    ),
    (
        "aarch64/context_switch.rs",
        include_str!("../src/arch/aarch64/context_switch.rs"),
    ),
    (
        "aarch64/context_witness.rs",
        include_str!("../src/arch/aarch64/context_witness.rs"),
    ),
    (
        "aarch64/smp2_witness.S",
        include_str!("../src/arch/aarch64/smp2_witness.S"),
    ),
    (
        "aarch64/apsched_witness.S",
        include_str!("../src/arch/aarch64/apsched_witness.S"),
    ),
    (
        "aarch64/overtaken_witness.S",
        include_str!("../src/arch/aarch64/overtaken_witness.S"),
    ),
    (
        "riscv64/smp3_witness.S",
        include_str!("../src/arch/riscv64/smp3_witness.S"),
    ),
    (
        "riscv64/apsched_witness.S",
        include_str!("../src/arch/riscv64/apsched_witness.S"),
    ),
    (
        "riscv64/sr_replace_witness.S",
        include_str!("../src/arch/riscv64/sr_replace_witness.S"),
    ),
    (
        "riscv64/vmi_witness.S",
        include_str!("../src/arch/riscv64/vmi_witness.S"),
    ),
];

fn json_string(src: &str, key: &str) -> String {
    let head = format!("\"{key}\": \"");
    let rest = src
        .split_once(&head)
        .unwrap_or_else(|| panic!("{key} missing"))
        .1;
    rest[..rest.find('"').expect("closing quote")].to_string()
}

/// The kernel targets take the toolchain's own soft-float configuration: the features and rustc
/// ABI of `x86_64-unknown-none` and `aarch64-unknown-none-softfloat`.
#[test]
fn kernel_targets_are_soft_float() {
    assert_eq!(
        json_string(X86_KERNEL_TARGET, "features"),
        "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float"
    );
    assert_eq!(json_string(X86_KERNEL_TARGET, "rustc-abi"), "softfloat");
    assert_eq!(
        json_string(A64_KERNEL_TARGET, "features"),
        "+v8a,+strict-align,-neon,-lse"
    );
    assert_eq!(json_string(A64_KERNEL_TARGET, "abi"), "softfloat");
    assert_eq!(json_string(A64_KERNEL_TARGET, "rustc-abi"), "softfloat");
    // RISC-V keeps the built-in riscv64gc kernel target; its no-FP result is enforced on the
    // final image (it has never contained an FP or vector instruction).
    assert!(CARGO_CONFIG.contains("YARM_RISCV64_NONE_TARGET = \"riscv64gc-unknown-none-elf\""));
}

/// Userspace keeps its FP/SIMD: the kernel-only change must not rebuild user programs soft-float.
#[test]
fn user_targets_keep_their_fp_simd() {
    assert_eq!(json_string(X86_USER_TARGET, "features"), "-mmx,+sse2");
    assert_eq!(
        json_string(A64_USER_TARGET, "features"),
        "+v8a,+strict-align,+neon,-lse,+reserve-x18"
    );
    assert!(!A64_USER_TARGET.contains("softfloat"));
    assert!(!X86_USER_TARGET.contains("rustc-abi"));
    // RISC-V userspace was already soft-float (lp64) with FS/VS forced Off; unchanged.
    assert_eq!(json_string(RV_USER_TARGET, "features"), "+m,+a,+c");
    assert_eq!(json_string(RV_USER_TARGET, "llvm-abiname"), "lp64");
}

/// Every QEMU artifact build publishes its kernel through the shared publish path, after the
/// marker contract and before the pair is recorded; that path runs the final-image check on the
/// staged bytes and publishes nothing on a failure.
#[test]
fn every_artifact_build_runs_the_final_image_check() {
    for (build, call) in [
        (
            BUILD_X86,
            "artifact_publish_kernel \"x86_64\" \"$KERNEL_ELF\" \"${BOOTSTRAP_FEATURE_ARGS}\" \"$KERNEL_IMAGE\"\n",
        ),
        (
            BUILD_A64,
            "artifact_publish_kernel \"aarch64\" \"$KERNEL_ELF\" \"${BOOTSTRAP_FEATURE_ARGS}\" \"$KERNEL_IMAGE\" \"$KERNEL_BIN_IMAGE\"\n",
        ),
        (
            BUILD_RV,
            "artifact_publish_kernel \"riscv64\" \"$KERNEL_ELF\" \"${BOOTSTRAP_FEATURE_ARGS}\" \"$KERNEL_ELF_IMAGE\" \"$KERNEL_IMAGE\"\n",
        ),
    ] {
        assert!(build.contains("set -euo pipefail"));
        let at = build.find(call).expect("the publish path is called");
        let markers = build.find("common_verify_kernel_markers").expect("markers");
        let record = build.find("common_record_artifacts").expect("record");
        assert!(markers < at && at < record);
    }
    let body = BINDING
        .split_once("artifact_publish_kernel() {")
        .expect("publish defined")
        .1;
    let body = &body[..body.find("\n}\n").expect("end")];
    let check = body.find("scripts/check-kernel-nofp.sh").expect("check");
    let publish = body.find("mv -f \"$stage_elf\" \"$out_elf\"").expect("publish");
    assert!(check < publish, "the check runs before anything is published");
    assert!(body.contains("return 1"));
    assert!(body.contains("_ab_invalidate \"$out_elf\" \"$out_boot\""));
}

fn policy_kinds(arch: &str) -> BTreeSet<String> {
    let start = CHECKER
        .find(&format!("    \"{arch}\": {{"))
        .unwrap_or_else(|| panic!("policy for {arch}"));
    let rest = &CHECKER[start..];
    if rest
        .lines()
        .next()
        .is_some_and(|l| l.trim_end().ends_with("{},"))
    {
        return BTreeSet::new();
    }
    let block = &rest[..rest.find("\n    },").expect("end of arch policy")];
    block
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix('"')
                .and_then(|r| r.split_once("\": ("))
                .map(|(k, _)| k.to_string())
        })
        .collect()
}

fn recorded_kinds(prefix: &str) -> BTreeSet<String> {
    recorded_names(prefix)
        .into_iter()
        .map(|(_, k)| k.split(' ').next().unwrap_or(&k).to_string())
        .collect()
}

/// (source, full record name) for every map record the sources under `prefix` emit.
fn recorded_names(prefix: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (name, src) in RECORDING_SOURCES
        .iter()
        .filter(|(n, _)| n.starts_with(prefix))
    {
        let mut in_map = false;
        for line in src.lines() {
            let t = line.trim();
            if t.starts_with(".pushsection .yarm_nofp_map") {
                in_map = true;
                continue;
            }
            if t == ".popsection" {
                in_map = false;
                continue;
            }
            let kind = if let Some(r) = t.strip_prefix(".asciz \"").filter(|_| in_map) {
                r.split('"').next()
            } else if t.contains("nofp_map_record!(") {
                t.rsplit_once(", \"")
                    .map(|(_, r)| r.trim_end_matches("\"),"))
            } else {
                None
            };
            if let Some(k) = kind {
                assert!(!k.is_empty(), "{name}: empty record kind");
                out.insert((name.to_string(), k.to_string()));
            }
        }
    }
    out
}

/// The sources and the checker agree on the exception inventory: every recorded kind is one the
/// policy knows (or a region kind), and every policy kind is recorded by some source.
#[test]
fn recorded_exception_kinds_match_the_checker_policy() {
    let regions: BTreeSet<String> = ["CODE16", "CODE32", "DATA", "USER_PROGRAM"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (arch, prefix) in [("x86_64", "x86_64/"), ("aarch64", "aarch64/")] {
        let policy = policy_kinds(arch);
        let recorded = recorded_kinds(prefix);
        assert!(!policy.is_empty() && !recorded.is_empty());
        for k in &recorded {
            assert!(
                policy.contains(k) || regions.contains(k),
                "{arch}: source records `{k}`, which the checker policy does not know"
            );
        }
        for k in &policy {
            assert!(
                recorded.contains(k),
                "{arch}: policy kind `{k}` is recorded by no source"
            );
        }
    }
    assert!(
        policy_kinds("riscv64").is_empty(),
        "RISC-V has no exceptions"
    );
    // RISC-V records only its embedded user programs, never an exception.
    let rv = recorded_kinds("riscv64/");
    assert_eq!(
        rv.into_iter().collect::<Vec<_>>(),
        vec!["USER_PROGRAM".to_string()]
    );
    assert!(ARCH_MOD.contains("macro_rules! nofp_map_record"));
    assert!(ARCH_MOD.contains(".quad 0x3150464F4E4D5259"));
    assert!(CHECKER.contains("MAGIC = 0x3150464F4E4D5259"));
}

/// The sources and the checker's reviewed exclusion inventory (`--list-inventory`) agree: every
/// CODE16/CODE32/DATA/USER_PROGRAM record a source emits is an inventory entry of that port owned
/// by that very source, every inventory entry is emitted by its owner, and every anchor symbol an
/// entry names is a label its owner defines.
#[test]
fn recorded_regions_match_the_reviewed_exclusion_inventory() {
    let out = std::process::Command::new("python3")
        .args(["-I", "scripts/check-kernel-nofp.py", "--list-inventory"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required to list the KERNEL-NOFP1 exclusion inventory");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let mut inventory = BTreeSet::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        assert_eq!(f.len(), 7, "inventory line `{line}`");
        let (arch, name, source, segs) = (f[0], f[1], f[3], f[6]);
        let owner = source
            .strip_prefix("src/arch/")
            .unwrap_or_else(|| panic!("{name}: owner {source} outside src/arch"));
        let (_, src) = RECORDING_SOURCES
            .iter()
            .find(|(n, _)| *n == owner)
            .unwrap_or_else(|| panic!("{arch} `{name}`: owner {owner} is not a recording source"));
        assert!(owner.starts_with(&format!("{arch}/")), "{name}: {owner}");
        for anchor in segs.split(',').flat_map(|seg| seg.split("..")) {
            if anchor.starts_with("section-") {
                continue;
            }
            assert!(
                src.lines().any(|l| l.trim() == format!("{anchor}:")),
                "{arch} `{name}`: anchor `{anchor}` is not a label {owner} defines"
            );
        }
        inventory.insert((owner.to_string(), name.to_string()));
    }
    let heads = ["CODE16 ", "CODE32 ", "DATA ", "USER_PROGRAM "];
    let recorded: BTreeSet<(String, String)> = ["x86_64/", "aarch64/", "riscv64/"]
        .iter()
        .flat_map(|p| recorded_names(p))
        .filter(|(_, k)| heads.iter().any(|h| k.starts_with(h)))
        .collect();
    assert_eq!(
        recorded, inventory,
        "region records emitted by the sources == the reviewed exclusion inventory"
    );
}

/// AArch64 assembles FP/SIMD only inside bracketed ranges: every `.arch_extension fp` / `simd`
/// in a source is closed again by `nofp` / `nosimd`.
#[test]
fn aarch64_fp_simd_assembly_is_bracketed() {
    for (name, src) in RECORDING_SOURCES
        .iter()
        .filter(|(n, _)| n.starts_with("aarch64/"))
    {
        let count = |needle: &str| {
            src.lines()
                .filter(|l| {
                    let t = l.trim().trim_matches(|c| c == '"' || c == ',');
                    t == needle
                })
                .count()
        };
        assert_eq!(
            count(".arch_extension fp"),
            count(".arch_extension nofp"),
            "{name}: unbalanced fp"
        );
        assert_eq!(
            count(".arch_extension simd"),
            count(".arch_extension nosimd"),
            "{name}: unbalanced simd"
        );
    }
}

/// The checker's own classifier and policy self-test.
#[test]
fn final_image_checker_self_test_passes() {
    let out = std::process::Command::new("python3")
        .args(["-I", "scripts/check-kernel-nofp.py", "--self-test"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("python3 is required to run the KERNEL-NOFP1 checker self-test");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("mismatches=0 result=ok"), "{text}");
}
