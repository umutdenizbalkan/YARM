// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KERNEL-NOFP1-ACCEPTANCE — behavioural regressions for the final-image FP/SIMD checker,
//! run through the same public CLI the artifact builds use (`scripts/check-kernel-nofp.sh ARCH
//! ELF FEATURES`), on synthetic ELF images built by `tests/fixtures/nofp/mkfixture.py`.
//!
//! Each case asserts the exit status and the exact set of violated obligations the checker
//! reports on its `NOFP_CHECK ... obligations=... result=...` line. The cases cover the two false
//! passes reproduced against the delivered checker (`rex.WB fninit`, i.e. bytes `49 db e3`, and an
//! ordinary `fninit` hidden by a new `DATA arbitrary` map record), every REX/REX2/stacked-prefix
//! form of FP and FP-state operations, legitimately prefixed integer code, decoder output the
//! checker cannot interpret, data the disassembler emits inside code, and every way an exclusion
//! can fail its reviewed inventory (unknown, enlarged, shifted, mislabelled, missing, duplicate,
//! feature-inconsistent, linker-eliminated, overlapping, boot-mode inconsistent), alongside valid
//! data, user-program and boot-mode images that must pass.
//!
//! These are verifier fixtures on synthetic images: they test what the checker accepts and
//! rejects, not the kernel. The built kernels are checked by the artifact builds themselves.
//!
//! Needs `python3` and the disassemblers the checker supports (GNU objdump 2.42, LLVM 18
//! `llvm-objdump`), resolved by the checker itself (`--check-tools`: `YARM_OBJDUMP` /
//! `YARM_LLVM_OBJDUMP`, else PATH), exactly as the artifact builds resolve them. A missing or
//! unsupported tool fails the test with the checker's NOFP-TOOLS diagnostic; it never skips it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

enum Expect {
    Pass,
    /// Exit non-zero with `result=fail` and exactly these obligation codes.
    Fail(&'static [&'static str]),
}
use Expect::{Fail, Pass};

const ORD: &str = "NOFP-ORDINARY";
const DECODE: &str = "NOFP-DECODE";
const DATA: &str = "NOFP-DATA";
const UNKNOWN: &str = "NOFP-EXCL-UNKNOWN";
const ANCHOR: &str = "NOFP-EXCL-ANCHOR";
const PRESENCE: &str = "NOFP-EXCL-PRESENCE";
const OWNERSHIP: &str = "NOFP-EXCL-OWNERSHIP";
const OVERLAP: &str = "NOFP-EXCL-OVERLAP";
const MODE: &str = "NOFP-EXCL-MODE";
const META: &str = "NOFP-METADATA";
const EXC: &str = "NOFP-EXCEPTION";
const INV: &str = "NOFP-INVENTORY";
const TOOLS: &str = "NOFP-TOOLS";

const CASES: &[(&str, Expect)] = &[
    // The baseline and the two reproduced false passes.
    ("x86_baseline", Pass),
    ("x86_ordinary_fninit", Fail(&[ORD])),
    ("x86_rexwb_fninit", Fail(&[ORD])),
    ("x86_data_arbitrary", Fail(&[UNKNOWN, ORD])),
    // Other REX combinations, REX2 and stacked prefixes on FP and FP-state operations.
    ("x86_rex_fninit", Fail(&[ORD])),
    ("x86_rexr_fninit", Fail(&[ORD])),
    ("x86_rexx_fninit", Fail(&[ORD])),
    ("x86_rexrb_fninit", Fail(&[ORD])),
    ("x86_rexwrxb_fninit", Fail(&[ORD])),
    ("x86_rex2_fninit", Fail(&[ORD])),
    ("x86_rex2w_fninit", Fail(&[ORD])),
    ("x86_data16_rex_fninit", Fail(&[ORD])),
    ("x86_fs_rexwb_fninit", Fail(&[ORD])),
    ("x86_repz_rexwb_fninit", Fail(&[ORD])),
    ("x86_lock_fninit", Fail(&[ORD])),
    ("x86_bare_rexw_then_rexb_fninit", Fail(&[ORD])),
    ("x86_rexw_ldmxcsr", Fail(&[ORD])),
    ("x86_rexwr_ldmxcsr", Fail(&[ORD])),
    ("x86_rexb_emms", Fail(&[ORD])),
    ("x86_rexb_vzeroupper", Fail(&[ORD])),
    ("x86_rex_stmxcsr", Fail(&[ORD])),
    ("x86_rexwb_fnsave", Fail(&[ORD])),
    ("x86_rexwb_frstor", Fail(&[ORD])),
    ("x86_rexwb_finit", Fail(&[ORD])),
    ("x86_rexwb_cr0_write", Fail(&[ORD])),
    ("x86_rex_clts", Fail(&[ORD])),
    ("x86_rexw_xsetbv", Fail(&[ORD])),
    // Ordinary integer instructions with legitimate prefixes.
    ("x86_prefixed_integer_ok", Pass),
    // Decoder output the checker cannot interpret, and incomplete inspection.
    ("x86_stub_passthrough", Pass),
    ("x86_stub_unknown_prefix", Fail(&[DECODE])),
    ("x86_stub_extra_tokens", Fail(&[DECODE])),
    ("x86_stub_bad_pseudo_prefix", Fail(&[DECODE])),
    ("x86_stub_garbage_line", Fail(&[DECODE])),
    ("x86_stub_dropped_line", Fail(&[DECODE])),
    // A disassembler that fails, is missing, reports an unsupported version or mis-decodes the
    // checker's probe is refused as NOFP-TOOLS before any image byte is judged.
    ("x86_stub_fails", Fail(&[TOOLS])),
    ("x86_tools_missing_objdump", Fail(&[TOOLS])),
    ("x86_tools_unsupported_version", Fail(&[TOOLS])),
    ("x86_tools_probe_misdecoded", Fail(&[TOOLS])),
    ("a64_tools_missing_llvm_objdump", Fail(&[TOOLS])),
    ("x86_trailing_partial_insn", Fail(&[DECODE])),
    ("x86_truncated_elf", Fail(&[DECODE])),
    // Exclusions: the boot-mode regions.
    ("x86_excl_handoff_enlarged", Fail(&[ANCHOR, DECODE, ORD])),
    ("x86_excl_gdt_shifted", Fail(&[ANCHOR, DECODE])),
    ("x86_excl_duplicate_boot32", Fail(&[PRESENCE])),
    ("x86_excl_missing_boot32", Fail(&[PRESENCE])),
    ("x86_excl_eliminated_boot32", Fail(&[PRESENCE])),
    (
        "x86_excl_conflict_exception_in_data",
        Fail(&[OVERLAP, EXC, ORD]),
    ),
    ("x86_excl_boot32_not_identity", Fail(&[MODE])),
    ("x86_excl_trampoline_oversize", Fail(&[MODE, DECODE])),
    ("x86_excl_unknown_user_program", Fail(&[UNKNOWN, ORD])),
    // Exclusions: embedded user programs.
    ("x86_user_vmi2_ok", Pass),
    ("x86_user_overtaken_ok", Pass),
    ("x86_user_enlarged", Fail(&[ANCHOR, ORD])),
    ("x86_user_mislabeled_kernel_code", Fail(&[ANCHOR, ORD])),
    (
        "x86_user_anchors_around_kernel_function",
        Fail(&[OWNERSHIP, ORD]),
    ),
    ("x86_user_without_feature", Fail(&[PRESENCE, ORD])),
    ("x86_user_record_missing", Fail(&[PRESENCE, ORD])),
    ("x86_user_eliminated_ok", Pass),
    ("x86_user_eliminated_but_linked", Fail(&[PRESENCE, ORD])),
    (
        "x86_user_segments_not_contiguous",
        Fail(&[ANCHOR, DECODE, ORD]),
    ),
    ("x86_user_anchor_defined_twice", Fail(&[ANCHOR, ORD])),
    // Malformed metadata.
    ("x86_meta_bad_map_word", Fail(&[META])),
    ("x86_meta_truncated_map_record", Fail(&[META])),
    ("x86_meta_malformed_record_name", Fail(&[META, UNKNOWN])),
    ("x86_meta_inverted_record", Fail(&[META, INV, ORD])),
    ("x86_meta_unknown_feature", Fail(&[META])),
    (
        "x86_meta_wrong_machine",
        Fail(&[META, DECODE, UNKNOWN, INV, ORD]),
    ),
    // AArch64: literal pools, data the disassembler emits, user programs, region kinds.
    ("a64_baseline", Pass),
    ("a64_ordinary_fmov", Fail(&[ORD])),
    ("a64_data_not_a_literal", Fail(&[DATA])),
    ("a64_data_pool_partly_loaded", Fail(&[DATA])),
    ("a64_user_smp2_ok", Pass),
    ("a64_user_lock2_ok", Pass),
    ("a64_user_enlarged", Fail(&[ANCHOR, DATA, ORD])),
    ("a64_user_without_feature", Fail(&[PRESENCE, DATA, ORD])),
    ("a64_excl_data_record", Fail(&[UNKNOWN, ORD])),
    ("a64_excl_x86_region_kind", Fail(&[UNKNOWN, ORD])),
    // RISC-V.
    ("rv_baseline", Pass),
    ("rv_ordinary_fld", Fail(&[ORD])),
    ("rv_user_vmi_ok", Pass),
    ("rv_user_smp3_ok", Pass),
    ("rv_user_eliminated_without_feature", Fail(&[PRESENCE])),
    ("rv_user_mislabeled_kernel_code", Fail(&[ANCHOR, ORD])),
    ("rv_mapping_data_still_decoded", Fail(&[ORD])),
];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The disassemblers every case uses: exactly the ones the checker itself resolves, version-gates
/// and probes (`--check-tools`), pinned for each run through `YARM_OBJDUMP` / `YARM_LLVM_OBJDUMP`
/// so fixtures and real kernel checks share one resolution. An unusable tool fails the test with
/// the checker's own NOFP-TOOLS diagnostic; nothing is skipped and nothing else is searched for.
struct Tools {
    objdump: String,
    llvm_objdump: String,
}

fn selected_tools() -> Tools {
    let out = Command::new("python3")
        .args(["-I", "scripts/check-kernel-nofp.py", "--check-tools"])
        .current_dir(repo())
        .output()
        .expect(
            "python3 is required to run the KERNEL-NOFP1 checker (the artifact builds need it too)",
        );
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the no-FP checker's disassemblers are not usable here:\n{text}"
    );
    let path_of = |tool: &str| {
        text.lines()
            .filter(|l| l.contains(&format!(" tool={tool} ")))
            .find_map(|l| l.split(' ').find_map(|w| w.strip_prefix("path=")))
            .unwrap_or_else(|| panic!("--check-tools did not report {tool}:\n{text}"))
            .to_string()
    };
    Tools {
        objdump: path_of("objdump"),
        llvm_objdump: path_of("llvm-objdump"),
    }
}

fn build_fixtures(out: &Path) -> Vec<(String, String, String, String)> {
    let _ = std::fs::remove_dir_all(out);
    let st = Command::new("python3")
        .arg("-I")
        .arg("tests/fixtures/nofp/mkfixture.py")
        .arg("--out")
        .arg(out)
        .current_dir(repo())
        .status()
        .expect("python3 is required to build the KERNEL-NOFP1 fixtures");
    assert!(st.success(), "fixture builder failed");
    std::fs::read_to_string(out.join("cases.tsv"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 4, "manifest line `{l}`");
            let none = |s: &str| {
                if s == "-" {
                    String::new()
                } else {
                    s.to_string()
                }
            };
            (f[0].to_string(), f[1].to_string(), none(f[2]), none(f[3]))
        })
        .collect()
}

struct Outcome {
    success: bool,
    result: String,
    obligations: BTreeSet<String>,
    text: String,
}

fn run_cli(arch: &str, elf: &Path, features: &str, stub: &str, tools: &Tools) -> Outcome {
    let mut cmd = Command::new("scripts/check-kernel-nofp.sh");
    cmd.arg(arch)
        .arg(elf)
        .arg(features)
        .current_dir(repo())
        .env("YARM_OBJDUMP", &tools.objdump)
        .env("YARM_LLVM_OBJDUMP", &tools.llvm_objdump);
    match stub {
        "" => {}
        // An explicitly selected disassembler that does not exist.
        "missing-objdump" => {
            cmd.env("YARM_OBJDUMP", "/nonexistent/objdump");
        }
        "missing-llvm-objdump" => {
            cmd.env("YARM_LLVM_OBJDUMP", "/nonexistent/llvm-objdump");
        }
        // The stub stands in for objdump and rewrites the real one's output.
        mode => {
            cmd.env(
                "YARM_OBJDUMP",
                repo().join("tests/fixtures/nofp/stub-bin/objdump"),
            )
            .env("YARM_NOFP_STUB_REAL", &tools.objdump)
            .env("YARM_NOFP_STUB", mode);
        }
    }
    let out = cmd.output().expect("run scripts/check-kernel-nofp.sh");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.starts_with("NOFP_CHECK "))
        .unwrap_or("")
        .to_string();
    let field = |k: &str| {
        line.split(' ')
            .find_map(|w| w.strip_prefix(k))
            .unwrap_or("")
            .to_string()
    };
    let obligations = field("obligations=")
        .split(',')
        .filter(|s| !s.is_empty() && *s != "-")
        .map(str::to_string)
        .collect();
    Outcome {
        success: out.status.success(),
        result: field("result="),
        obligations,
        text,
    }
}

/// Every fixture through the real CLI: exit status, verdict and the exact violated obligations.
#[test]
fn checker_cli_fixture_regressions() {
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("kernel-nofp-cli");
    std::fs::create_dir_all(&work).unwrap();
    let tools = selected_tools();
    let fixtures = build_fixtures(&work.join("fixtures"));
    let expect: BTreeMap<&str, &Expect> = CASES.iter().map(|(n, e)| (*n, e)).collect();
    assert_eq!(expect.len(), CASES.len(), "duplicate case in CASES");
    let built: BTreeSet<&str> = fixtures.iter().map(|f| f.0.as_str()).collect();
    let listed: BTreeSet<&str> = expect.keys().copied().collect();
    assert_eq!(
        built, listed,
        "every fixture the builder makes has an expectation here, and vice versa"
    );

    let results: Vec<(String, Outcome)> = std::thread::scope(|s| {
        let chunks: Vec<_> = fixtures.chunks(fixtures.len().div_ceil(8)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let (tools, work) = (&tools, &work);
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|(name, arch, feats, stub)| {
                            let elf = work.join("fixtures").join(format!("{name}.elf"));
                            (name.clone(), run_cli(arch, &elf, feats, stub, tools))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });

    let mut bad = Vec::new();
    for (name, got) in &results {
        let ok = match expect[name.as_str()] {
            Pass => got.success && got.result == "pass" && got.obligations.is_empty(),
            Fail(codes) => {
                let want: BTreeSet<String> = codes.iter().map(|c| c.to_string()).collect();
                !got.success && got.result == "fail" && got.obligations == want
            }
        };
        if !ok {
            let want = match expect[name.as_str()] {
                Pass => "pass".to_string(),
                Fail(c) => format!("fail {c:?}"),
            };
            bad.push(format!(
                "{name}: want {want}, got exit_ok={} result={} obligations={:?}\n{}",
                got.success, got.result, got.obligations, got.text
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} fixture(s) misjudged:\n{}",
        bad.len(),
        results.len(),
        bad.join("\n")
    );
}

/// A missing image is an incomplete inspection, never a pass.
#[test]
fn checker_cli_missing_image_fails() {
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("kernel-nofp-cli-missing");
    std::fs::create_dir_all(&work).unwrap();
    let tools = selected_tools();
    let got = run_cli("x86_64", &work.join("no-such.elf"), "", "", &tools);
    assert!(!got.success && got.result == "fail", "{}", got.text);
    assert_eq!(
        got.obligations,
        BTreeSet::from([DECODE.to_string()]),
        "{}",
        got.text
    );
    assert!(got.text.contains("inspection_incomplete:image_missing"));
}
