// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200D-2B1C — source-grep contract for the three ServerDies exact-commit runners.
//!
//! These pin the runner discipline itself, so a later change cannot quietly turn a live proof
//! into something weaker: exact-commit freezing, fresh logs, single-boot witnesses, the ordered
//! marker chain, the forbidden set, and the two-sided feature-off binary audit.
//!
//! Stage 200D-2B1C PREPARES these runners; it does not execute them and claims no live cell.

const COMMON: &str = include_str!("../scripts/lib/serverdies-runner-common.sh");
const X86: &str = include_str!("../scripts/qemu-x86_64-server-dies-smoke.sh");
const AARCH64: &str = include_str!("../scripts/qemu-aarch64-server-dies-smoke.sh");
const RISCV64: &str = include_str!("../scripts/qemu-riscv64-server-dies-smoke.sh");

fn arch_runners() -> [(&'static str, &'static str); 3] {
    [("x86_64", X86), ("aarch64", AARCH64), ("riscv64", RISCV64)]
}

#[test]
fn every_arch_runner_exists_and_uses_the_shared_body() {
    for (arch, src) in arch_runners() {
        assert!(
            src.contains("source \"$(dirname \"$0\")/lib/serverdies-runner-common.sh\""),
            "{arch} runner must source the shared proof body"
        );
        assert!(
            src.contains("serverdies_main"),
            "{arch} runner must invoke the shared entry point"
        );
        assert!(
            src.contains(&format!("ARCH_TAG={arch}")),
            "{arch} runner must tag its architecture"
        );
    }
}

#[test]
fn every_arch_runner_selects_server_dies_on_its_own_knob() {
    for (arch, src, knob) in [
        ("x86_64", X86, "yarm.x86_64_ipc_reply_timeout_oracle"),
        ("aarch64", AARCH64, "yarm.aarch64_ipc_reply_timeout_oracle"),
        ("riscv64", RISCV64, "yarm.riscv_ipc_reply_timeout_oracle"),
    ] {
        assert!(
            src.contains(&format!("SELECTOR={knob}")),
            "{arch} runner must use its own per-arch selector knob"
        );
        assert!(
            src.contains("${SELECTOR}=server-dies"),
            "{arch} runner must arm the ServerDies scenario"
        );
    }
}

#[test]
fn every_arch_runner_builds_its_own_feature_and_target() {
    for (arch, src, feature) in [
        ("x86_64", X86, "x86-ipc-reply-timeout-oracle"),
        ("aarch64", AARCH64, "aarch64-ipc-reply-timeout-oracle"),
        ("riscv64", RISCV64, "riscv64-ipc-reply-timeout-oracle"),
    ] {
        assert!(
            src.contains(&format!("FEATURE={feature}")),
            "{arch} runner must forward its own per-arch oracle feature"
        );
    }
    // No runner may reach for another port's feature.
    assert!(
        !X86.contains("aarch64-ipc") && !X86.contains("riscv64-ipc"),
        "the x86_64 runner must not forward another port's feature"
    );
}

#[test]
fn runner_freezes_and_rechecks_the_exact_commit() {
    assert!(
        COMMON.contains("git diff --quiet") && COMMON.contains("reason=dirty_tree"),
        "a dirty tree must fail closed before any build"
    );
    assert!(
        COMMON.contains("SHA0=$(git rev-parse HEAD)")
            && COMMON.contains("TREE0=$(git rev-parse HEAD^{tree})"),
        "the runner must freeze both the SHA and the tree hash"
    );
    for phase in ["RUN_A build", "RUN_B build", "RUN_B boot", "final"] {
        assert!(
            COMMON.contains(&format!("serverdies_recheck_commit \"{phase}\"")),
            "the runner must re-check the exact commit after: {phase}"
        );
    }
    assert!(
        COMMON.contains("SHA drifted") && COMMON.contains("tree hash drifted"),
        "drift in either identity must be named distinctly"
    );
}

#[test]
fn run_b_is_a_single_fresh_boot() {
    assert!(
        COMMON.contains("rm -rf \"$LOGDIR\"; mkdir -p \"$LOGDIR\""),
        "logs must be fresh — a stale log can never be re-graded"
    );
    assert!(
        COMMON.contains("rm -f \"$log\""),
        "the RUN_B log must be removed before the boot"
    );
    assert!(
        COMMON.contains("expected exactly one boot banner"),
        "RUN_B must witness exactly one boot"
    );
    for (arch, src) in arch_runners() {
        assert!(
            src.contains("-no-reboot -no-shutdown"),
            "{arch} must boot with -no-reboot -no-shutdown so a fault cannot look clean"
        );
    }
}

#[test]
fn required_marker_chain_is_ordered_and_complete() {
    for marker in [
        "IPC_SERVER_DEATH_EXIT_ENTERED nr=16 role=server",
        "IPC_SERVER_DEATH_LINK_CAPTURED",
        "IPC_SERVER_DEATH_DEFERRED_PUBLISHED",
        "EXIT_TASK_DISPOSITION_CONSUMED arch=${ARCH_TAG}",
        "IPC_SERVER_DEATH_BROAD_LOCK_RELEASED",
        "IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN",
        "IPC_SERVER_DEATH_TERMINAL_CLAIM terminal=PeerDeath result=won",
        "IPC_SERVER_DEATH_COMPLETION_COMMITTED",
        "IPC_SERVER_DEATH_CALLER_ENQUEUED",
        "IPC_SERVER_DEATH_USER_VALIDATED result=ServerDied code=10",
        "IPC_SERVER_DEATH_OK",
    ] {
        assert!(
            COMMON.contains(marker),
            "required marker chain missing: {marker}"
        );
    }
    assert!(
        COMMON.contains("marker out of order"),
        "the chain must be ORDERED, not merely present"
    );
    // Exactly one wake and one winner — a duplicate is as bad as none.
    assert!(
        COMMON.contains("expected exactly one caller enqueue")
            && COMMON.contains("expected exactly one PeerDeath winner"),
        "the runner must require exactly one caller wake and one terminal winner"
    );
}

#[test]
fn forbidden_markers_cover_every_hard_fail() {
    for forbidden in [
        "IPC_SERVER_DEATH_EXIT_RETURNED",
        "IPC_SERVER_DEATH_WRONG_SERVER_IDENTITY",
        "IPC_SERVER_DEATH_WRONG_CALLER_IDENTITY",
        "IPC_SERVER_DEATH_WRONG_TIMEOUT_GENERATION",
        "IPC_SERVER_DEATH_DUPLICATE_WAKE",
        "IPC_SERVER_DEATH_LINK_LEAK",
        "IPC_SERVER_DEATH_TIMEOUT_WON",
        "IPC_SERVER_DEATH_LATE_REPLY_ACCEPTED",
        "IPC_SERVER_DEATH_STALE_AUTHORITY_RESTORED",
        // The architecture return contract's own failures are fatal in a live run too.
        "EXIT_TASK_EXITING_STILL_CURRENT",
        "EXIT_TASK_WRONG_IDENTITY",
        "EXIT_TASK_RESELECTED_EXITING_TASK",
    ] {
        assert!(
            COMMON.contains(forbidden),
            "forbidden set missing a hard-fail literal: {forbidden}"
        );
    }
}

#[test]
fn feature_off_audit_is_two_sided() {
    // Oracle literals absent...
    assert!(
        COMMON.contains("RUN_A oracle literal present feature-off"),
        "RUN_A must reject an oracle literal in a feature-off image"
    );
    // ...AND production literals present, which is what stops the audit being vacuous.
    assert!(
        COMMON.contains("RUN_A production literal MISSING feature-off"),
        "RUN_A must require the production server-death literals to survive feature-off"
    );
    assert!(
        COMMON.contains("--no-default-features \\\n    -p yarm --bin kernel_boot"),
        "RUN_A must build the feature-off kernel with no default features"
    );
}

#[test]
fn runner_prepares_but_does_not_claim_a_live_cell_in_this_stage() {
    for (arch, src) in arch_runners() {
        assert!(
            src.contains("does NOT execute it") || src.contains("claims no live cell"),
            "{arch} runner must state that Stage 200D-2B1C only prepares it"
        );
    }
    // The seal the runner emits is the LIVE one, distinct from the readiness seal the stage
    // itself emits — so a prepared-but-unrun runner can never be mistaken for a live proof.
    assert!(
        COMMON.contains("STAGE_200D2B1C_${ARCH_TAG^^}_SERVER_DIES_SEAL"),
        "the runner's own seal must be per-arch and live-scoped"
    );
}
