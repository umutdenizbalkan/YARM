// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! BUILD-GATES1 §4 — the AArch64 U9-RX4 reply-terminal grader judges the exact transaction.
//!
//! Runs the REAL grader (`scripts/lib/u9rx4-terminal-eval.sh`, the one the AArch64 core smoke
//! sources) over the fixture corpus from `tests/fixtures/u9rx4/mkcases.py`: fresh records, valid
//! slot reuse at a later generation on both request routes, stale generations, another transaction
//! on the same slot, substituted request/reply identities, duplicate and missing completions, and
//! the evaluator's original self-test cases. Every reject must fail for its named reason.
//!
//! Mutation controls re-introduce the two defects the repair removed — the generation-1 pin and the
//! record-index wildcard on the reply resolution — and require the corpus to catch each.

use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "scripts/lib/u9rx4-terminal-eval.sh";
const SMOKE: &str = include_str!("../scripts/qemu-aarch64-core-smoke.sh");

fn repo() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("yarm-u9rx4-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn corpus(dir: &Path) -> Vec<(String, String, String)> {
    let st = Command::new("python3")
        .args(["-I", "tests/fixtures/u9rx4/mkcases.py", "--out"])
        .arg(dir)
        .current_dir(repo())
        .status()
        .expect("python3 is required to build the U9-RX4 fixtures");
    assert!(st.success(), "fixture builder failed");
    std::fs::read_to_string(dir.join("cases.tsv"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 3, "cases.tsv line `{l}`");
            (f[0].into(), f[1].into(), f[2].into())
        })
        .collect()
}

/// Runs `script` with the grader library at `lib` sourced; returns (success, output).
fn with_lib(lib: &Path, script: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("source \"$1\"; shift; {script}"))
        .arg("grader")
        .arg(lib)
        .args(args)
        .current_dir(repo())
        .output()
        .expect("bash is required");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn verdict(lib: &Path, log: &Path) -> (bool, String) {
    with_lib(
        lib,
        "u9rx4_eval_terminal \"$(tr '\\r' '\\n' <\"$1\")\" CASE",
        &[log.to_str().unwrap()],
    )
}

#[test]
fn the_grader_judges_every_fixture_case_for_its_reason() {
    let t = Scratch::new("corpus");
    let cases = corpus(&t.0);
    let lib = Path::new(repo()).join(LIB);
    let mut accepted = 0;
    for (name, expect, reason) in &cases {
        let (ok, out) = verdict(&lib, &t.0.join(format!("{name}.log")));
        match expect.as_str() {
            "accept" => {
                assert!(ok, "{name} must be accepted:\n{out}");
                assert!(out.contains("[ok] CASE:"), "{name}:\n{out}");
                accepted += 1;
            }
            "reject" => {
                assert!(!ok, "{name} must be rejected:\n{out}");
                assert!(
                    out.contains(reason.as_str()),
                    "{name} must be rejected because `{reason}`:\n{out}"
                );
            }
            other => panic!("bad expectation {other}"),
        }
    }
    // The brief's cases are all present.
    for name in [
        "fresh_reply_win",
        "fresh_timeout_win_prelock",
        "reuse_reply_win_queued",
        "reuse_reply_win_direct",
        "reuse_timeout_win",
        "stale_generation_reply",
        "stale_generation_route",
        "another_transaction_on_the_same_slot",
        "substituted_reply_identity",
        "substituted_request_identity",
        "duplicate_completion_reply_commit",
        "duplicate_completion_both_winners",
        "missing_completion",
    ] {
        assert!(cases.iter().any(|(n, _, _)| n == name), "case {name}");
    }
    assert_eq!(accepted, 8);
    assert_eq!(cases.len(), 35);
    // The smoke's own self-test entry point agrees on the whole corpus.
    let (ok, out) = with_lib(&lib, "u9rx4_selftest \"$1\"", &[t.0.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "[ok] U9-RX4: terminal-outcome evaluator self-test passed (8 accepted, 27 rejected)"
        ),
        "{out}"
    );
}

/// Each repaired defect, re-introduced, must make the self-test fail on the cases that catch it.
#[test]
fn the_corpus_catches_the_defects_the_repair_removed() {
    let t = Scratch::new("mut");
    corpus(&t.0);
    let src = std::fs::read_to_string(Path::new(repo()).join(LIB)).unwrap();
    for (label, from, to, must_flip) in [
        (
            "generation pinned to 1 on the arming",
            "record_index=[0-9]+ record_generation=[0-9]+ .*finite_deadline=1",
            "record_index=[0-9]+ record_generation=1 .*finite_deadline=1",
            &[
                "reuse_reply_win_queued",
                "reuse_reply_win_direct",
                "reuse_timeout_win",
            ][..],
        ),
        (
            "reply resolution matching any record index",
            "cap=${cap} reply_index=${rec} generation=${gen}( |\\$)\")\"\n  cap_resolves",
            "cap=${cap} reply_index=[0-9]+ generation=${gen}( |\\$)\")\"\n  cap_resolves",
            &[
                "fresh_substituted_reply_index",
                "substituted_reply_identity",
            ][..],
        ),
        (
            "replier revoke not bound to the record",
            "if (( replier_revoke_any != replier_revoke )); then",
            "if false; then",
            &["extra_revoke_of_pm_cap_against_another_record"][..],
        ),
    ] {
        assert_eq!(src.matches(from).count(), 1, "mutation site `{label}`");
        let m = t.0.join("mutant.sh");
        std::fs::write(&m, src.replace(from, to)).unwrap();
        let (ok, out) = with_lib(&m, "u9rx4_selftest \"$1\"", &[t.0.to_str().unwrap()]);
        assert!(!ok, "the corpus must catch: {label}\n{out}");
        for case in must_flip {
            assert!(
                out.contains(&format!("'{case}'")),
                "{label}: `{case}` must catch it\n{out}"
            );
        }
    }
}

/// The smoke grades with the library and runs the corpus self-test before the real log.
#[test]
fn the_aarch64_core_smoke_grades_with_the_shared_library() {
    let src = SMOKE.find("source \"$(dirname \"${BASH_SOURCE[0]}\")/lib/u9rx4-terminal-eval.sh\"");
    let selftest = SMOKE.find("! u9rx4_selftest \"$u9rx4_fixtures\"");
    let eval = SMOKE.find("if ! u9rx4_eval_terminal \"$u9rx4_log\" \"U9-RX4\"; then");
    match (src, selftest, eval) {
        (Some(a), Some(b), Some(c)) => assert!(a < b && b < c),
        other => panic!("wiring {other:?}"),
    }
    assert!(SMOKE.contains("tests/fixtures/u9rx4/mkcases.py"));
    // No second copy of the evaluator lives in the smoke.
    assert!(!SMOKE.contains("u9rx4_eval_terminal() {"));
    assert!(!SMOKE.contains("record_generation=1 "));
}

/// The live reuse witness is labeled and default-off: the feature is inert outside the supervisor,
/// the supervisor's yield is compiled only under it, and the witness run boots only the checked
/// witness build and grades premise and outcome separately.
#[test]
fn the_reuse_witness_is_labeled_default_off_and_separately_graded() {
    let sup = include_str!(
        "../crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs"
    );
    let gate = sup
        .find("#[cfg(feature = \"u9rx4-reuse-witness\")]\n            {")
        .expect("the yield is compiled only under the witness feature");
    let query = sup
        .find("yarm_user_rt::user_log!(\"SUPERVISOR_LIFECYCLE_QUERY tid={}\", supervisor_tid);")
        .unwrap();
    assert!(gate < query);
    assert_eq!(sup.matches("u9rx4-reuse-witness").count(), 2);
    for toml in [
        include_str!("../Cargo.toml"),
        include_str!("../crates/yarm-driver-servers/Cargo.toml"),
        include_str!("../crates/yarm-fs-servers/Cargo.toml"),
        include_str!("../crates/yarm-control-plane-servers/Cargo.toml"),
    ] {
        assert!(toml.contains("\nu9rx4-reuse-witness = []\n"));
        assert!(!toml.contains("default = [\"u9rx4-reuse-witness"));
    }
    let w = include_str!("../scripts/qemu-aarch64-u9rx4-reuse-witness-smoke.sh");
    assert!(w.contains("ARTIFACT_REQUIRE=\"kernel=$FEATURE initramfs=$FEATURE\""));
    assert!(w.contains("seal \"result=fail reason=premise detail=no-reuse"));
    assert!(w.contains("(( gen <= 1 )) || (( earlier < 1 )) || (( own_earlier != 0 ))"));
    assert!(w.contains("seal \"result=fail reason=grade"));
}
