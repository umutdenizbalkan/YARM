// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 200D-2B1C — source-grep contract for the three ServerDies exact-commit runners.
//!
//! These pin the runner discipline itself, so a later change cannot quietly turn a live proof
//! into something weaker: exact-commit freezing, fresh logs, single-boot witnesses, the ordered
//! marker chain, the forbidden set, and the two-sided feature-off binary audit.
//!
//! Stage 200D-2B1C PREPARED these runners. Stage 200D-2B1D-x86 first executed the x86_64 one
//! and QEMU-BASELINE1 §4 the AArch64 and RISC-V ones; each records its live stage.

const COMMON: &str = include_str!("../scripts/lib/serverdies-runner-common.sh");
const X86: &str = include_str!("../scripts/qemu-x86_64-server-dies-smoke.sh");
const AARCH64: &str = include_str!("../scripts/qemu-aarch64-server-dies-smoke.sh");
const RISCV64: &str = include_str!("../scripts/qemu-riscv64-server-dies-smoke.sh");
// POST-U9-STABILIZATION §1 — the production owners whose literals the chain grades. Every
// exit-lifecycle marker the runner requires must be one these still EMIT, in that format; a
// runner that requires an obsolete marker, or a marker whose owner changed its text, fails here
// before a live boot ever reports it missing.
const EXIT_TXN: &str = include_str!("../src/kernel/syscall/exit_txn.rs");
const SYSCALL_SPLIT: &str = include_str!("../src/kernel/syscall_split.rs");
const SYSCALL: &str = include_str!("../src/kernel/syscall.rs");

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
        // U9-EXIT1: the dying server's NR 16 is served by the split exit transaction, so the
        // in-lock consumer's `EXIT_TASK_DISPOSITION_CONSUMED` is not emitted for it. The
        // lifecycle is graded through what the split owners emit for the same incarnation:
        // the route's entry edge, then the retired claim.
        "EXIT_TASK_SPLIT_ENTER tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID} result=ok",
        "EXIT_TASK_CLAIM_RETIRED tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID}",
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
    // The obsolete in-lock marker is not REQUIRED anywhere in the chain: requiring it would
    // make every live boot fail on a marker the current lifecycle never emits.
    let required = COMMON
        .split("serverdies_required_markers() {")
        .nth(1)
        .and_then(|s| s.split("\nMARKERS\n").next())
        .expect("the required-marker heredoc");
    assert!(
        !required.contains("EXIT_TASK_DISPOSITION_CONSUMED"),
        "the chain must not require the in-lock consumer's marker"
    );
    // Order inside the chain: entry edge, server-death reservation and publication, then the
    // retired claim, then the post-lock drain that settles the caller.
    let at = |m: &str| {
        required
            .find(m)
            .unwrap_or_else(|| panic!("chain lacks {m}"))
    };
    assert!(
        at("EXIT_TASK_SPLIT_ENTER") < at("IPC_SERVER_DEATH_DEFERRED_RESERVED")
            && at("IPC_SERVER_DEATH_DEFERRED_PUBLISHED") < at("EXIT_TASK_CLAIM_RETIRED")
            && at("EXIT_TASK_CLAIM_RETIRED") < at("IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN")
            && at("IPC_SERVER_DEATH_TERMINAL_CLAIM") < at("IPC_SERVER_DEATH_CALLER_ENQUEUED")
            && at("IPC_SERVER_DEATH_CALLER_ENQUEUED") < at("IPC_SERVER_DEATH_USER_VALIDATED")
            && at("IPC_SERVER_DEATH_USER_VALIDATED") < at("IPC_SERVER_DEATH_SURVIVOR_PROGRESS_OK"),
        "the causal order: exit, publication, retirement, drain, one winner, one wake, \
         userspace validation, then subsequent progress"
    );
    // Exactly one wake and one winner — a duplicate is as bad as none.
    assert!(
        COMMON.contains("expected exactly one caller enqueue")
            && COMMON.contains("expected exactly one PeerDeath winner"),
        "the runner must require exactly one caller wake and one terminal winner"
    );
}

/// 199D-SD3 (§4) — the chain is graded against the WITNESSED transaction, not against
/// identity-free literals that any other task's exit also satisfies.
#[test]
fn required_chain_is_scoped_to_the_witnessed_transaction() {
    // The identity is resolved from the log, from two lines that must each be unique and
    // must name the same reply record — a causal join, not an assumption.
    assert!(
        COMMON.contains("serverdies_resolve_identity"),
        "the runner must resolve the witnessed transaction's identity"
    );
    for anchor in [
        "expected exactly one captured reverse link",
        "expected exactly one committed completion",
        "completion names a different reply record than the captured link",
        "could not resolve the witnessed identity",
    ] {
        assert!(
            COMMON.contains(anchor),
            "identity resolution must fail closed on: {anchor}"
        );
    }
    // It is resolved BEFORE the chain is graded, and a boot that cannot name its own
    // scenario stops rather than falling back to an unscoped chain.
    let resolve = COMMON
        .find("serverdies_resolve_identity \"$log\" || return")
        .expect("resolution is wired into RUN_B and fails closed");
    let ordered = COMMON
        .find("RUN_B marker out of order")
        .expect("the ordered chain");
    assert!(resolve < ordered, "the identity is resolved before grading");

    // The two markers that EVERY task exit emits carry the witnessed server's identity, so
    // an unrelated earlier exit can neither satisfy nor fail the chain.
    for scoped in [
        "IPC_SERVER_DEATH_DEFERRED_RESERVED server_tid=${SD_SERVER_TID} server_asid=${SD_SERVER_ASID}",
        "EXIT_TASK_SPLIT_ENTER tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID} result=ok",
        "EXIT_TASK_CLAIM_RETIRED tid=${SD_SERVER_TID} asid=${SD_SERVER_ASID}",
    ] {
        assert!(COMMON.contains(scoped), "unscoped shared marker: {scoped}");
    }
    // Each scoped exit literal is the PRODUCTION owner's own format, field for field — derived
    // from the emitter rather than restated, so the runner cannot drift from what boots print.
    assert!(
        SYSCALL_SPLIT.contains("\"EXIT_TASK_SPLIT_ENTER tid={} asid={} result=ok\""),
        "the split exit route emits the entry edge the chain requires"
    );
    assert!(
        EXIT_TXN.contains("\"EXIT_TASK_CLAIM_RETIRED tid={} asid={} pid={}")
            && EXIT_TXN.contains("server_death={}\""),
        "the exit owner emits the retired claim, carrying the server-death handoff bit"
    );
    // The two facts the obsolete marker used to imply are asserted DIRECTLY: the exit never
    // reached the terminal broad dispatcher (whose edge marker still exists in the broad arm,
    // so counting zero is meaningful), and the retired claim attests the server-death handoff.
    assert!(
        SYSCALL.contains("\"EXIT_TASK_BROAD_ENTER tid={} asid={} result=ok\"")
            && COMMON.contains("broad_edges=$(grep -c -F \"EXIT_TASK_BROAD_ENTER\" \"$log\"")
            && COMMON.contains("[[ \"$broad_edges\" == \"0\" ]]"),
        "the dying server's NR 16 must be shown never to reach the broad dispatcher"
    );
    assert!(
        COMMON.contains("*\"server_death=1\"*) ;;")
            && COMMON.contains("the retired claim does not attest the server-death handoff"),
        "the retired claim must attest that this exit owed and handed off a completion"
    );
    // The completion half is scoped by the caller and the reply record's generation.
    for scoped in [
        "caller_tid=${SD_CALLER_TID} caller_asid=${SD_CALLER_ASID}",
        "record_index=${SD_RECORD_INDEX} record_generation=${SD_RECORD_GENERATION}",
    ] {
        assert!(
            COMMON.contains(scoped),
            "unscoped completion marker: {scoped}"
        );
    }

    // Scoping must not cost duplicate detection, from either side.
    assert!(
        COMMON.contains("scoped marker seen $dup times (expected 1)"),
        "a genuine repeat of the scenario's own exit must still fail"
    );
    assert!(
        COMMON.contains(
            "IPC_SERVER_DEATH_TRANSITION_AUDIT vector=[1, 1, 1, 1, 1, 1, 1, 1, 1] \
             result_before_enqueue=1 result=ok"
        ),
        "the transition audit must be required to PASS, not merely to be present"
    );
    for forbidden in [
        "IPC_SERVER_DEATH_DUPLICATE_TRANSITION",
        "IPC_SERVER_DEATH_TRANSITION_COUNT",
        "IPC_SERVER_DEATH_SCOPE_CONFLICT",
        "IPC_SERVER_DEATH_SCOPE_UNARMED",
    ] {
        assert!(
            COMMON.contains(forbidden),
            "the accounting hard-fails must be forbidden: {forbidden}"
        );
    }
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
fn every_runner_records_its_live_stage_and_boots_the_staged_initramfs() {
    // Stage 200D-2B1D-x86 executed the x86_64 runner; QEMU-BASELINE1 §4 executed the other two,
    // after correcting the invocation that booted a bare kernel with no initramfs (the scenario
    // is a userspace task inside init_server, so that boot died at BOOT_FATAL_INITRAMFS_MISSING
    // before it could start). Each port's status is asserted explicitly rather than the whole set
    // being relaxed to whichever is loosest.
    assert!(
        X86.contains("Stage 200D-2B1D-x86 is the first stage to run it"),
        "the x86_64 runner must record which stage executed it"
    );
    for (arch, src) in [("aarch64", AARCH64), ("riscv64", RISCV64)] {
        assert!(
            src.contains("QEMU-BASELINE1 is the first stage to run it"),
            "{arch} runner must record which stage executed it"
        );
        assert!(
            !src.contains("does NOT execute it") && !src.contains("claims no live cell"),
            "{arch} runner must not still claim to be prepare-only"
        );
        // It boots what the port's artifact script publishes, with the matching initramfs,
        // after re-staging the kernel oracle-on and refusing an image without the oracle.
        assert!(
            src.contains(&format!(
                "serverdies_stage_raw_image {arch} \"$BOOT_KERNEL\" \"$BOOT_INITRD\""
            )),
            "{arch} runner must stage its boot image through the shared, verifying helper"
        );
        assert!(
            src.contains(&format!(
                "BOOT_KERNEL=${{BOOT_KERNEL:-build-{arch}/yarm-{arch}.bin}}"
            )) && src.contains(&format!(
                "BOOT_INITRD=${{BOOT_INITRD:-build-{arch}/initramfs-core.cpio}}"
            )),
            "{arch} runner must boot the published raw image and its initramfs"
        );
        assert!(
            src.contains("-kernel \"$BOOT_KERNEL\" -initrd \"$BOOT_INITRD\""),
            "{arch} runner must pass the initramfs to the boot"
        );
    }
    assert!(
        COMMON.contains("./scripts/build-qemu-${arch}-artifacts.sh")
            && COMMON.contains(
                "--features \"$FEATURE\" -p yarm --bin kernel_boot >>\"$LOGDIR/stage.log\""
            )
            && COMMON.contains("staged image is not oracle-on"),
        "the staging helper rebuilds oracle-on after the artifact script and verifies the image"
    );
    // The seal the runner emits is the LIVE one, distinct from the readiness seal the stage
    // itself emits — so a prepared-but-unrun runner can never be mistaken for a live proof.
    assert!(
        COMMON.contains("STAGE_200D2B1C_${ARCH_TAG^^}_SERVER_DIES_SEAL"),
        "the runner's own seal must be per-arch and live-scoped"
    );
}
