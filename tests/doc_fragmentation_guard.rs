// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Documentation-fragmentation guard.
//!
//! `doc/` has been re-fragmented into per-stage report files three times. Each time the
//! canonical owner docs went stale while the real state lived in dozens of
//! `STAGE_*.md` files that nothing linked and nothing validated. Consolidation Pass 6
//! removed the third such wave (inventory: `doc/DOCUMENTATION_MAP.md`).
//!
//! This test makes the fourth wave fail CI instead of merging quietly.
//!
//! **A new per-stage report requires an explicit, reviewable code change**: adding the
//! filename to [`APPROVED_FRAGMENTS`] below *and* registering it in
//! `doc/DOCUMENTATION_MAP.md`. Both are required, so approval cannot be granted by
//! accident and cannot be granted without the ownership map recording who owns the
//! topic. The intended answer to "where does my stage report go?" is **not** here — it
//! is the canonical owner doc named in `doc/DOCUMENTATION_MAP.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-stage / fragment documents that have been explicitly approved to exist.
///
/// **Empty is the correct steady state.** Adding an entry here is a deliberate,
/// reviewable act; it must be accompanied by a row in `doc/DOCUMENTATION_MAP.md`
/// explaining why the canonical owner doc could not hold the content.
const APPROVED_FRAGMENTS: &[&str] = &[];

/// Filename shapes that indicate a per-stage / per-milestone / per-PR fragment.
///
/// These are *prefixes* checked case-sensitively against the file name.
const FRAGMENT_PREFIXES: &[&str] = &[
    "STAGE_",
    "PHASE0_",
    "PHASE1_",
    "PHASE2A_",
    "PHASE2B_",
    "PHASE3A_",
    "PHASE3B_",
    "PHASE4_",
    "PHASE5_",
    "PHASE6_",
    "MILESTONE_",
    "KERNEL_UNLOCKING_MILESTONE_",
    "KERNEL_UNLOCKING_NEXT_CONTEXT",
    "KERNEL_UNLOCKING_STAGE",
];

/// Filename substrings that indicate a duplicate status / audit / plan / checklist doc.
///
/// The canonical owners for these topics already exist; a second file with one of these
/// shapes is by definition a duplicate. `KERNEL_UNLOCK_AUDIT.md` is the single approved
/// audit and is exempted by name.
const DUPLICATE_SUFFIXES: &[&str] = &[
    "_REPORT.md",
    "_CHECKLIST.md",
    "-checklist.md",
    "_PR_PLAN.md",
    "_PR_BOARD.md",
    "-promotion-plan.md",
    "-readiness-evidence.md",
    "_NEXT_CONTEXT.md",
    "_STATUS_2026_05.md",
];

/// Docs that must exist, because other docs, source files and scripts point at them.
const CANONICAL_DOCS: &[&str] = &[
    "STATUS.md",
    "KERNEL_UNLOCKING.md",
    "KERNEL_UNLOCK_AUDIT.md",
    "KERNEL_LOCKING.md",
    "IPC.md",
    "SYSCALL_ABI.md",
    "KERNEL_TEST_RULES.md",
    "PROJECT_HISTORY.md",
    "DOCUMENTATION_MAP.md",
    "ARCH_X86_64.md",
    "ARCH_AARCH64.md",
    "ARCH_RISCV64.md",
];

/// Audit / evidence docs that are exempt from the duplicate-shape rule by name.
const EXEMPT_BY_NAME: &[&str] = &[
    // The single approved kernel-unlock audit.
    "KERNEL_UNLOCK_AUDIT.md",
    // Accepted cross-architecture retirement seals. These are permanent evidence
    // records pinned by `include_str!` from the hosted corpus, not stage narratives.
    "FIRST_COHORT_RETIREMENT_SEAL.md",
    "SECOND_COHORT_RETIREMENT_SEAL.md",
    "SECOND_COHORT_PLAIN_SEAL.md",
    "SECOND_COHORT_ORDINARY_CAP_SEAL.md",
    // Long-lived service contracts pinned by `include_str!` from production crate
    // sources. Retiring these requires editing those crates; see
    // `doc/DOCUMENTATION_MAP.md` "Deferred deletions".
    "process-manager-restart-contract.md",
    "supervisor-pm-restart-contract.md",
    "supervisor-runtime-state.md",
    "supervisor-audit.md",
    "driver-layering-audit.md",
    "driver-manager-pm-spawn-contract.md",
    "pm-restart-live-implementation-checklist.md",
    "pm-restart-live-promotion-plan.md",
    "pm-restart-live-readiness-evidence.md",
];

fn doc_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("doc")
}

fn doc_file_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(doc_dir())
        .expect("doc/ must be readable")
        .map(|e| e.expect("readable dir entry"))
        .filter(|e| e.file_type().expect("file type").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

fn approved() -> BTreeSet<&'static str> {
    APPROVED_FRAGMENTS.iter().copied().collect()
}

fn exempt() -> BTreeSet<&'static str> {
    EXEMPT_BY_NAME.iter().copied().collect()
}

#[test]
fn no_unapproved_per_stage_report_files() {
    let approved = approved();
    let offenders: Vec<String> = doc_file_names()
        .into_iter()
        .filter(|n| FRAGMENT_PREFIXES.iter().any(|p| n.starts_with(p)))
        .filter(|n| !approved.contains(n.as_str()))
        .collect();

    assert!(
        offenders.is_empty(),
        "per-stage report files are forbidden in doc/.\n\
         Offending files: {offenders:?}\n\n\
         Stage findings belong in the canonical owner doc named in \
         doc/DOCUMENTATION_MAP.md — for kernel unlocking that is doc/KERNEL_UNLOCKING.md \
         (stages + roadmap), doc/KERNEL_LOCKING.md (lock architecture + census), \
         doc/IPC.md (IPC / reply / timeout / ServerDies), doc/STATUS.md (current state \
         + blockers) and doc/PROJECT_HISTORY.md (accepted seals).\n\n\
         If a per-stage file is genuinely unavoidable, it must be added to \
         APPROVED_FRAGMENTS in tests/doc_fragmentation_guard.rs AND registered in \
         doc/DOCUMENTATION_MAP.md in the same change."
    );
}

#[test]
fn no_duplicate_status_audit_or_plan_documents() {
    let approved = approved();
    let exempt = exempt();
    let offenders: Vec<String> = doc_file_names()
        .into_iter()
        .filter(|n| DUPLICATE_SUFFIXES.iter().any(|s| n.ends_with(s)))
        .filter(|n| !approved.contains(n.as_str()) && !exempt.contains(n.as_str()))
        .collect();

    assert!(
        offenders.is_empty(),
        "duplicate status / audit / readiness / checklist / PR-plan documents are \
         forbidden in doc/.\nOffending files: {offenders:?}\n\n\
         Update the canonical owner doc instead (doc/DOCUMENTATION_MAP.md lists them). \
         Deliberate exceptions go in EXEMPT_BY_NAME or APPROVED_FRAGMENTS in \
         tests/doc_fragmentation_guard.rs, with a matching entry in \
         doc/DOCUMENTATION_MAP.md."
    );
}

#[test]
fn approved_fragments_are_registered_in_the_documentation_map() {
    if APPROVED_FRAGMENTS.is_empty() {
        return;
    }
    let map = fs::read_to_string(doc_dir().join("DOCUMENTATION_MAP.md"))
        .expect("doc/DOCUMENTATION_MAP.md must exist");
    for name in APPROVED_FRAGMENTS {
        assert!(
            map.contains(name),
            "{name} is in APPROVED_FRAGMENTS but is not registered in \
             doc/DOCUMENTATION_MAP.md. Approval requires both: the allowlist entry and \
             an ownership-map row saying why the canonical owner doc cannot hold it."
        );
    }
}

#[test]
fn approved_fragments_that_no_longer_exist_are_pruned() {
    let present: BTreeSet<String> = doc_file_names().into_iter().collect();
    let stale: Vec<&str> = APPROVED_FRAGMENTS
        .iter()
        .copied()
        .filter(|n| !present.contains(*n))
        .collect();
    assert!(
        stale.is_empty(),
        "APPROVED_FRAGMENTS lists files that no longer exist: {stale:?}. \
         Remove them so the allowlist cannot silently pre-authorize a future file \
         that happens to reuse the name."
    );
}

#[test]
fn canonical_owner_documents_exist() {
    for name in CANONICAL_DOCS {
        let path = doc_dir().join(name);
        assert!(
            path.is_file(),
            "canonical document doc/{name} is missing. Other docs, source `include_str!` \
             pins and CI scripts point at it; deleting it breaks them."
        );
    }
}

#[test]
fn documentation_map_records_the_consolidation_and_the_guard() {
    let map = fs::read_to_string(doc_dir().join("DOCUMENTATION_MAP.md"))
        .expect("doc/DOCUMENTATION_MAP.md must exist");
    assert!(
        map.contains("tests/doc_fragmentation_guard.rs"),
        "doc/DOCUMENTATION_MAP.md must name the guard that enforces it, so a reader who \
         hits the guard can find the rule and a reader of the rule can find the guard."
    );
    assert!(
        map.contains("Consolidation Pass 6"),
        "doc/DOCUMENTATION_MAP.md must retain the Pass 6 deleted-fragment inventory; it \
         is the only in-tree record of where each deleted document's content went."
    );
}

#[test]
fn kernel_unlock_audit_states_its_audited_commit() {
    let audit = fs::read_to_string(doc_dir().join("KERNEL_UNLOCK_AUDIT.md"))
        .expect("doc/KERNEL_UNLOCK_AUDIT.md must exist");
    assert!(
        audit.contains("757993b699b309dafdb3d17c428380c08d7fc9f7"),
        "the audit must state the exact commit it was taken against; an audit without a \
         baseline cannot be checked for staleness."
    );
    assert!(
        audit.contains("1118b61b74588e73b0dc235dc96086ec7488257c"),
        "the audit must state the exact tree it was taken against."
    );
}
