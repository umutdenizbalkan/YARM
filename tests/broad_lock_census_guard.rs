// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Broad-lock census guard — keeps canonical Stage 204A from silently going stale.
//!
//! Stage 204A ("broad-lock callsite census") is the one canonical stage recorded COMPLETE.
//! Its deliverable is an authoritative, classified list of every runtime acquisition of the
//! broad `SpinLock<KernelState>`, in `doc/KERNEL_UNLOCK_AUDIT.md` §1. A census is only
//! complete while it matches the tree, so the moment someone adds or deletes a production
//! `SharedKernel::with` / `with_cpu` callsite without re-classifying it, 204A quietly stops
//! being true and every downstream count (§1.2, §1.4a, `doc/KERNEL_LOCKING.md` §0.2,
//! `doc/STATUS.md` §0) becomes wrong.
//!
//! This guard recomputes the census from source with the same method the audit used and
//! fails if it drifts from the pinned expectation.
//!
//! **Adding a broad-lock callsite is not forbidden** — the retirement work legitimately
//! moves them around. What is forbidden is doing so without updating the census. When this
//! test fails, update [`EXPECTED_WITH_CPU`] / [`EXPECTED_WITH_BROAD`] /
//! [`EXPECTED_STATE_LOCK`] here **and** the classification and totals in the documents
//! named above, in the same change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-file count of production `SharedKernel::with_cpu` callsites.
const EXPECTED_WITH_CPU: &[(&str, usize)] = &[
    ("src/arch/riscv64/boot.rs", 1),
    ("src/arch/riscv64/trap.rs", 8),
    ("src/arch/trap_entry.rs", 12),
    ("src/arch/x86_64/descriptor_tables.rs", 2),
    ("src/arch/x86_64/smp.rs", 4),
    ("src/kernel/boot/thread_state.rs", 1),
    ("src/runtime.rs", 13),
];

/// Per-file count of production broad `SharedKernel::with(|state| …)` callsites.
///
/// `src/kernel/boot/orchestrator_state.rs` is listed because it matches the same textual
/// pattern, but its single hit is `LOCK_ORDER_LAST_RANK.with(|last| …)` — a `thread_local!`
/// accessor, **not** a broad-lock acquisition. It is counted here so the guard stays purely
/// mechanical, and subtracted by [`THREAD_LOCAL_FALSE_POSITIVES`] to reach the audited
/// figure.
const EXPECTED_WITH_BROAD: &[(&str, usize)] = &[
    ("src/arch/x86_64/smp.rs", 2),
    ("src/kernel/boot/orchestrator_state.rs", 1),
    ("src/runtime.rs", 8),
];

/// Per-file count of raw `self.state.lock()` sites. All three are the bodies of
/// `SharedKernel::lock` / `with` / `with_cpu` themselves.
const EXPECTED_STATE_LOCK: &[(&str, usize)] = &[("src/runtime.rs", 3)];

/// Textual matches in [`EXPECTED_WITH_BROAD`] that are not broad-lock acquisitions.
const THREAD_LOCAL_FALSE_POSITIVES: usize = 1;

/// The audited totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.2.
///
/// The acquisition total is `with_cpu + with_broad` only. The three raw `self.state.lock()`
/// sites are the **bodies** of `SharedKernel::lock` / `with` / `with_cpu` — the
/// implementations that every callsite goes through, not callsites themselves. Adding them
/// would double-count the lock.
const AUDITED_WITH_CPU_TOTAL: usize = 41;
const AUDITED_WITH_BROAD_TOTAL: usize = 10;
const AUDITED_STATE_LOCK_TOTAL: usize = 3;
const AUDITED_ACQUISITION_TOTAL: usize = AUDITED_WITH_CPU_TOTAL + AUDITED_WITH_BROAD_TOTAL;

/// Stage 204A classification totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a.
const CLASS_BOOT_ONLY: usize = 0;
const CLASS_TEST_ONLY: usize = 3;
const CLASS_OBSOLETE: usize = 2;
const CLASS_RUNTIME_REQUIRED: usize = 46;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every production `.rs` file: everything under `src/` and `crates/`, minus whole-file
/// test modules (`tests.rs`).
fn production_rs_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    walk(&root.join("crates"), &mut out);
    out.sort();
    out
}

/// Index of the `mod tests {` line that follows a `#[cfg(test)]` attribute, i.e. where
/// production code ends in a file that carries an inline test module.
fn test_module_cutoff(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if (trimmed.starts_with("mod tests") || trimmed.starts_with("pub mod tests"))
            && trimmed.ends_with('{')
            && i > 0
            && lines[i - 1].contains("#[cfg(test)]")
        {
            return i;
        }
    }
    lines.len()
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with('*') || t.starts_with("/*")
}

/// Count production occurrences of `needle_pred` per file, relative to the repo root.
fn census(needle_pred: fn(&str) -> bool) -> BTreeMap<String, usize> {
    let root = repo_root();
    let mut counts = BTreeMap::new();
    for path in production_rs_files() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let cutoff = test_module_cutoff(&lines);
        let mut n = 0usize;
        for line in lines.iter().take(cutoff) {
            if !is_comment(line) && needle_pred(line) {
                n += 1;
            }
        }
        if n > 0 {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            counts.insert(rel, n);
        }
    }
    counts
}

fn with_cpu_line(line: &str) -> bool {
    line.contains(".with_cpu(")
}

/// `.with(` immediately followed by a closure — the broad `&mut KernelState` form.
fn with_broad_line(line: &str) -> bool {
    let Some(idx) = line.find(".with(") else {
        return false;
    };
    line[idx + ".with(".len()..].trim_start().starts_with('|')
}

fn state_lock_line(line: &str) -> bool {
    line.contains(".state.lock()")
}

fn expected_map(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(f, n)| ((*f).to_string(), *n)).collect()
}

fn diff_report(
    kind: &str,
    actual: &BTreeMap<String, usize>,
    expected: &BTreeMap<String, usize>,
) -> String {
    let mut out = format!("\n{kind} census drift:\n");
    for (file, n) in actual {
        match expected.get(file) {
            Some(e) if e == n => {}
            Some(e) => out.push_str(&format!("  CHANGED {file}: expected {e}, found {n}\n")),
            None => out.push_str(&format!(
                "  NEW     {file}: found {n} (not in the census)\n"
            )),
        }
    }
    for (file, e) in expected {
        if !actual.contains_key(file) {
            out.push_str(&format!("  GONE    {file}: expected {e}, found 0\n"));
        }
    }
    out.push_str(
        "\nA broad-lock callsite was added or removed without updating the Stage 204A census.\n\
         Adding one is allowed; leaving the census stale is not — Stage 204A is recorded\n\
         COMPLETE and stops being true the moment this drifts.\n\n\
         Update, in the same change:\n\
           * the pinned tables in tests/broad_lock_census_guard.rs\n\
           * doc/KERNEL_UNLOCK_AUDIT.md §1.2, §1.3, §1.4 and the §1.4a classification\n\
           * doc/KERNEL_LOCKING.md §0.2\n\
           * doc/STATUS.md §0 (broad-lock position table)\n",
    );
    out
}

#[test]
fn with_cpu_callsites_match_the_census() {
    let actual = census(with_cpu_line);
    let expected = expected_map(EXPECTED_WITH_CPU);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("SharedKernel::with_cpu", &actual, &expected)
    );
}

#[test]
fn broad_with_callsites_match_the_census() {
    let actual = census(with_broad_line);
    let expected = expected_map(EXPECTED_WITH_BROAD);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("broad SharedKernel::with", &actual, &expected)
    );
}

#[test]
fn raw_state_lock_sites_match_the_census() {
    let actual = census(state_lock_line);
    let expected = expected_map(EXPECTED_STATE_LOCK);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("raw state.lock()", &actual, &expected)
    );
}

#[test]
fn census_totals_are_internally_consistent() {
    let with_cpu: usize = census(with_cpu_line).values().sum();
    let with_broad_raw: usize = census(with_broad_line).values().sum();
    let state_lock: usize = census(state_lock_line).values().sum();
    let with_broad = with_broad_raw - THREAD_LOCAL_FALSE_POSITIVES;

    assert_eq!(with_cpu, AUDITED_WITH_CPU_TOTAL, "with_cpu total drifted");
    assert_eq!(
        with_broad, AUDITED_WITH_BROAD_TOTAL,
        "broad `with` total drifted (after subtracting {THREAD_LOCAL_FALSE_POSITIVES} \
         thread_local false positive)"
    );
    assert_eq!(
        state_lock, AUDITED_STATE_LOCK_TOTAL,
        "raw state.lock() total drifted — these are the three `SharedKernel` method bodies; \
         a fourth means the broad lock grew a new entry point"
    );
    assert_eq!(
        with_cpu + with_broad,
        AUDITED_ACQUISITION_TOTAL,
        "total broad-lock acquisition sites drifted (state.lock() bodies are deliberately \
         excluded — counting them would double-count the lock)"
    );
}

#[test]
fn stage_204a_classification_covers_every_runtime_required_site() {
    assert_eq!(
        CLASS_BOOT_ONLY + CLASS_TEST_ONLY + CLASS_OBSOLETE + CLASS_RUNTIME_REQUIRED,
        AUDITED_ACQUISITION_TOTAL,
        "the Stage 204A classification must account for every acquisition site; \
         boot-only + test-only + obsolete + runtime-required must equal the census total"
    );
}

#[test]
fn documents_publish_the_same_census_totals() {
    let doc = |name: &str| {
        fs::read_to_string(repo_root().join("doc").join(name))
            .unwrap_or_else(|_| panic!("doc/{name} must exist"))
    };
    let audit = doc("KERNEL_UNLOCK_AUDIT.md");
    let locking = doc("KERNEL_LOCKING.md");
    let status = doc("STATUS.md");

    let total = AUDITED_ACQUISITION_TOTAL.to_string();
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
        ("STATUS.md", &status),
    ] {
        assert!(
            text.contains(&format!("**{total}**")),
            "doc/{name} must publish the broad-lock acquisition total ({total}); \
             the census and the documents must not drift apart"
        );
    }

    // The classification counts are published only by the audit and the locking doc.
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
    ] {
        assert!(
            text.contains(&format!("**{CLASS_RUNTIME_REQUIRED}**")),
            "doc/{name} must publish the runtime-required count ({CLASS_RUNTIME_REQUIRED})"
        );
    }
}
