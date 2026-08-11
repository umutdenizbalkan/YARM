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
    // U3 (203C): 8 -> 7 -> 6 -> 3. Two read-only post-lock drains moved to the rank-1
    // scheduler seam (`current_tid_split_read`), then the three homologous switch/restore
    // drains (queue-switch foundation, FutexWait switch-success, Yield switch-success) moved
    // to the exact-token rank-2 transaction: `direct_dispatch_activate_asid_split` (which
    // performs the real RISC-V map/root/`write_satp`+`sfence.vma` on this target) plus
    // `direct_dispatch_restore_context_split` and `direct_dispatch_take_completion_split`.
    // U3 (203C): 3 -> 2. The post-lock `CurrentTaskExited` validation snapshot moved to
    // `post_lock_exit_validation_split`, one coherent rank-1 (scheduler) transaction with the
    // rank-2 (task) acquisition nested inside it. The two remaining acquisitions are the
    // canonical broad trap phase and the deferred terminal-idle predicate; this file is NOT
    // fully drained.
    ("src/arch/riscv64/trap.rs", 2),
    // Stage 199D: 12 -> 11. The AArch64 handled-split return path no longer reacquires the
    // broad lock to finalize a syscall; it uses two bounded rank-2 task-domain transactions
    // (exact-incarnation TLS take, exact-incarnation context commit) instead.
    // U3 (203C): 11 -> 9. The AArch64 FutexWait and Yield switch-success restores now run the
    // neutral exact-token resume core (`direct_dispatch_resume_incoming_core`). The FutexWait
    // no-incoming idle check was briefly retired too and then RESTORED: its only live gate (the
    // Stage 195F idle oracle) is unreachable behind the pre-existing SpawnV5 stall, so that one
    // substitution could not be live-proven and was not shipped.
    // U3 (203C): 9 -> 7. The two homologous x86_64 D2 switch-success restores (blocking send
    // and blocking receive) were byte-identical broad re-acquires; both now run one neutral
    // exact-token transaction, `x86_post_lock_resume_marked_incoming`.
    ("src/arch/trap_entry.rs", 7),
    ("src/arch/x86_64/descriptor_tables.rs", 2),
    ("src/arch/x86_64/smp.rs", 4),
    ("src/kernel/boot/thread_state.rs", 1),
    // U1: 13 -> 12. The obsolete `SharedKernel::handle_trap_with_cpu` wrapper had no
    // in-tree caller at all and was deleted.
    ("src/runtime.rs", 12),
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
    // U1: 8 -> 7. The obsolete `SharedKernel::run_reply_timeout_completion` wrapper had no
    // production caller and was deleted; the single completion body
    // (`KernelState::run_reply_timeout_completion_locked`) is unchanged.
    // U2: 7 -> 4. The two test-only helpers were relocated into test-only modules:
    // `ipc_recv_with_deadline_split_bridge` (2 acquisitions) and the
    // `SharedKernel::control_plane_set_process_cnode_slots_via_syscall` wrapper (1).
    ("src/runtime.rs", 4),
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
const AUDITED_WITH_CPU_TOTAL: usize = 28; // U3: 38 -> 33 -> 31 -> 30 -> 28 (six RISC-V, two AArch64, two x86_64)
const AUDITED_WITH_BROAD_TOTAL: usize = 6; // U2: 9 -> 6 (three test-only acquisitions relocated)
const AUDITED_STATE_LOCK_TOTAL: usize = 3;
const AUDITED_ACQUISITION_TOTAL: usize = AUDITED_WITH_CPU_TOTAL + AUDITED_WITH_BROAD_TOTAL;

/// Stage 204A classification totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a.
const CLASS_BOOT_ONLY: usize = 0;
const CLASS_TEST_ONLY: usize = 0; // U2: 3 -> 0 (test-only helpers left the production census)
const CLASS_OBSOLETE: usize = 0; // U1: 2 -> 0 (both obsolete acquisitions deleted)
const CLASS_RUNTIME_REQUIRED: usize = 34; // U3: 44 -> 39 -> 37 -> 36 -> 34 (ten drains retired onto their seams)

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

/// Stage 199D (AArch64 blocker 3): the post-lock direct dispatch drain must not have added a
/// broad-lock acquisition site. The requirement is that the census stays at 50 or DECREASES —
/// a drain that reached for `with_cpu` to do its ASID activation or frame restore, as the
/// FutexWait and Yield drains legitimately do, would push it to 51 and fail here.
#[test]
fn stage199d_blocker3_added_no_broad_lock_acquisition_site() {
    assert!(
        AUDITED_ACQUISITION_TOTAL <= 50,
        "the broad-lock census must stay at 50 or decrease; it is {AUDITED_ACQUISITION_TOTAL}"
    );
    // The drain and the arch resume primitive it calls contain no acquisition of their own.
    let trap_entry = std::fs::read_to_string("src/arch/trap_entry.rs").expect("trap_entry.rs");
    let drain = trap_entry
        .split("if crate::kernel::direct_dispatch::is_pending(cpu_idx) {")
        .nth(1)
        .and_then(|s| s.split("\n    // Stage 192A").next())
        .expect("the direct dispatch drain");
    assert!(
        !drain.contains("with_cpu(") && !drain.contains("shared.with("),
        "the post-lock direct dispatch drain must acquire no broad lock"
    );
}
