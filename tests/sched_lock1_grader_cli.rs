// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK1-ACCEPTANCE — the scheduler-lock witness grader judges what it claims to.
//!
//! Runs the REAL grader CLI (`scripts/grade-sched-lock1-witness.py`) over the corpus written by
//! `tests/fixtures/sched_lock1/mkcases.py` (independent of the grader): healthy traces, valid
//! uncredited attempts followed by credited ones, the four original false passes (a third lock
//! instance, a substituted claim ASID, an invalid uncredited attempt, net-no-op "commits"), and
//! substituted lock instances, cells, commits, incarnations, installations and outcomes. Every
//! accepted case must exit 0; every rejected one must exit 1 naming its obligation.
//!
//! Mutation controls undo ONE repair at a time — every check tagged `# repair:<name>` in the
//! grader is disabled — and require the corpus to catch it: at least one case must change verdict,
//! every case that changes must be one expecting that repair's obligation, the named original
//! reproduction (where the repair has one) must be among them, and the healthy traces must still
//! pass.
//!
//! Source pins keep the producer facts the grader relies on: no net-no-op section is recorded, a
//! claim never looks a TID up after the commit, and the cell map comes from the domain's cells.

use std::path::{Path, PathBuf};
use std::process::Command;

const GRADER: &str = "scripts/grade-sched-lock1-witness.py";
const WITNESS: &str = include_str!("../src/kernel/schedlock1_witness.rs");
const SCHEDULER: &str = include_str!("../src/kernel/scheduler.rs");

fn repo() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("yarm-sl1g-{tag}-{}", std::process::id()));
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

struct Case {
    file: String,
    expect_pass: bool,
    obligation: String,
    name: String,
}

fn corpus(dir: &Path) -> Vec<Case> {
    let st = Command::new("python3")
        .args(["-I", "tests/fixtures/sched_lock1/mkcases.py", "--out"])
        .arg(dir)
        .current_dir(repo())
        .status()
        .expect("python3 is required to build the SL1W fixtures");
    assert!(st.success(), "fixture builder failed");
    std::fs::read_to_string(dir.join("cases.tsv"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 4, "cases.tsv line `{l}`");
            Case {
                file: f[0].into(),
                expect_pass: f[1] == "pass",
                obligation: f[2].into(),
                name: f[3].into(),
            }
        })
        .collect()
}

/// Runs the grader at `grader` on `log` through its CLI: (exit code, the `reason=` text or the
/// pass line).
fn grade(grader: &Path, log: &Path) -> (i32, String) {
    let out = Command::new("python3")
        .arg("-I")
        .arg(grader)
        .arg("--expect-cpus")
        .arg("2")
        .arg(log)
        .output()
        .expect("python3");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let reason = text
        .lines()
        .find_map(|l| l.split_once("reason=").map(|(_, r)| r.to_string()))
        .unwrap_or_else(|| text.trim().to_string());
    (out.status.code().unwrap_or(-1), reason)
}

fn verdict_ok(c: &Case, code: i32, reason: &str) -> bool {
    if c.expect_pass {
        code == 0
    } else {
        code == 1 && reason.starts_with(&c.obligation)
    }
}

#[test]
fn every_corpus_case_gets_its_verdict_and_obligation() {
    let s = Scratch::new("corpus");
    let cases = corpus(&s.0);
    assert!(cases.len() >= 40, "corpus too small: {}", cases.len());
    let grader = Path::new(repo()).join(GRADER);
    let mut bad = Vec::new();
    for c in &cases {
        let (code, reason) = grade(&grader, &s.0.join(&c.file));
        if !verdict_ok(c, code, &reason) {
            bad.push(format!(
                "{} (expected {} {}) got exit {code}: {reason}",
                c.name,
                if c.expect_pass { "pass" } else { "fail" },
                c.obligation
            ));
        }
    }
    assert!(bad.is_empty(), "grader verdicts wrong:\n{}", bad.join("\n"));
    // The positive controls are really there.
    for name in [
        "healthy",
        "valid uncredited attempt (bound) then credited retry",
        "valid uncredited attempt (interference) then credited retry",
        "late observer progress (accepted)",
    ] {
        assert!(
            cases.iter().any(|c| c.name == name && c.expect_pass),
            "positive case `{name}` missing"
        );
    }
}

#[test]
fn the_grader_self_test_passes() {
    let out = Command::new("python3")
        .args(["-I", GRADER, "--self-test"])
        .current_dir(repo())
        .output()
        .expect("python3");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "self-test failed:\n{text}");
    assert!(text.contains(", 0 wrong"), "{text}");
}

/// The grader with every `# repair:<name>` check disabled: a tagged `if` becomes `if False:`, a
/// tagged assignment becomes `= True`.
fn without_repair(src: &str, name: &str) -> String {
    let tag = format!("# repair:{name}");
    let mut hits = 0;
    let out: Vec<String> = src
        .lines()
        .map(|l| {
            if !l.trim_end().ends_with(&tag) {
                return l.to_string();
            }
            hits += 1;
            let indent = &l[..l.len() - l.trim_start().len()];
            if l.trim_start().starts_with("if ") {
                format!("{indent}if False:  {tag}")
            } else {
                let (lhs, _) = l.split_once(" = ").expect("tagged assignment");
                format!("{lhs} = True  {tag}")
            }
        })
        .collect();
    assert!(hits > 0, "no check tagged {tag}");
    out.join("\n") + "\n"
}

#[test]
fn undoing_each_repair_lets_its_own_defect_through() {
    let s = Scratch::new("controls");
    let cases = corpus(&s.0);
    let src = std::fs::read_to_string(Path::new(repo()).join(GRADER)).unwrap();
    // (repair, the obligation its cases expect, the original reproduction it must re-admit)
    let controls: &[(&str, &str, Option<&str>)] = &[
        (
            "lock-instance",
            "lock-instance:",
            Some("A every section names a third lock instance"),
        ),
        (
            "cell-map",
            "cell-map:",
            Some("a CPU's cell changes inside the window"),
        ),
        (
            "no-op-section",
            "no-op-section:",
            Some("D no scheduling mutation (net-no-op sections)"),
        ),
        (
            "claim-commit",
            "claim-commit:",
            Some("claim names another commit"),
        ),
        (
            "claim-incarnation",
            "claim-incarnation:",
            Some("B claim ASID changed"),
        ),
        (
            "run",
            "run:",
            Some("one installation observed running twice"),
        ),
        (
            "progress",
            "progress:",
            Some("progress linked to the wrong installation (a queue-only commit)"),
        ),
        (
            "attempt-ownership",
            "attempt:",
            Some("C invalid uncredited attempt before a healthy one"),
        ),
        (
            "attempt-order",
            "attempt:",
            Some("attempt number out of order"),
        ),
        (
            "attempt-outcome",
            "attempt:",
            Some("invalid uncredited attempt: failure with neither interference nor bound"),
        ),
        (
            "commit-count",
            "coverage:",
            Some("missing genuine commits (one in the window)"),
        ),
    ];
    let mut report = Vec::new();
    for (repair, obligation, original) in controls {
        let mutated = s.0.join(format!("grader-without-{repair}.py"));
        std::fs::write(&mutated, without_repair(&src, repair)).unwrap();
        let mut changed = Vec::new();
        for c in &cases {
            let (code, reason) = grade(&mutated, &s.0.join(&c.file));
            if !verdict_ok(c, code, &reason) {
                changed.push((c, code, reason));
            }
        }
        assert!(
            !changed.is_empty(),
            "undoing `{repair}` changed no verdict — the corpus does not test it"
        );
        for (c, code, reason) in &changed {
            assert!(
                !c.expect_pass && c.obligation == *obligation,
                "undoing `{repair}` changed `{}` (expected {} {}), now exit {code}: {reason}",
                c.name,
                if c.expect_pass { "pass" } else { "fail" },
                c.obligation
            );
        }
        if let Some(orig) = original {
            assert!(
                changed.iter().any(|(c, _, _)| c.name == *orig),
                "undoing `{repair}` did not re-admit `{orig}`"
            );
        }
        report.push(format!("{repair}: {} cases", changed.len()));
    }
    eprintln!("controls: {}", report.join(", "));
}

#[test]
fn producer_facts_the_grader_relies_on() {
    // A section with the same current, no addition and no removal is never recorded.
    assert!(
        WITNESS
            .contains("if before_cur == after_cur && n_add == 0 && n_rem == 0 {\n        return;"),
        "the producer must suppress net-no-op sections"
    );
    // The claim carries the owner-staged incarnation and the exact commit — no TID lookup.
    let claim = WITNESS
        .split("pub(crate) fn claim(")
        .nth(1)
        .and_then(|b| b.split("\n}\n").next())
        .expect("claim body");
    assert!(
        !claim.contains("task_asid_for_tid")
            && claim.contains("STAGED")
            && claim.contains("LAST_ADD"),
        "a claim must not reconstruct the incarnation from a bare TID"
    );
    // The cell map comes from the domain's own cell array, under the coherent view.
    assert!(SCHEDULER.contains("pub(crate) fn witness_cell_addrs(&self, idx: usize)"));
    assert!(WITNESS.contains("domain.witness_cell_addrs(cpu)"));
    // Every owner whose commit is claimed stages its incarnation (cfg-gated, witness only).
    let runtime = include_str!("../src/runtime.rs");
    let enqueue = include_str!("../src/kernel/task_enqueue.rs");
    let staged = runtime.matches("schedlock1_witness::stage_").count()
        + enqueue.matches("schedlock1_witness::stage_").count();
    let claims = runtime.matches("schedlock1_witness::claim(").count();
    assert_eq!(claims, 5, "claim sites");
    assert!(staged >= 6, "stage sites: {staged}");
}
