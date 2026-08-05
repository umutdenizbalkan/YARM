// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 199D — behavioural contract for the RISC-V core smoke's capacity rejection.
//!
//! The smoke script used to reject any line containing the bare word `capacity`. That pattern was
//! added at Stage 181 (`2a30515d`) beside `Vm\(Full\)` and `\boom\b` as a proxy for resource
//! exhaustion, when no kernel marker printed the word benignly. Stage 199D (`fcfc55e3`) replaced
//! the magic ack-store capacity of 8 with a structural bound and added an independent waiter
//! census, and those reporters print `capacity=256`, `ack_capacity=256` and `capacity_refused=0`
//! on **every** healthy boot — so the bare word stopped discriminating and failed a healthy
//! RISC-V core boot.
//!
//! These tests are **behavioural, not textual**: each fixture line is evaluated against the
//! script's own `REJECT_PATTERNS`, parsed out of the script and applied with `rg` exactly as the
//! script applies them (`rg -n "$pat"` — regex, not fixed-string). A pattern that stops
//! discriminating, or a benign reporter that starts being rejected, fails here rather than in
//! QEMU.

use std::io::Write;
use std::process::{Command, Stdio};

const SMOKE: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");

/// The script's `REJECT_PATTERNS` array, in order, with comments and quoting stripped.
fn reject_patterns() -> Vec<String> {
    let start = SMOKE
        .find("REJECT_PATTERNS=(")
        .expect("the smoke script must declare REJECT_PATTERNS");
    let body = &SMOKE[start + "REJECT_PATTERNS=(".len()..];
    let end = body
        .find("\n)")
        .expect("REJECT_PATTERNS must be closed on its own line");
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_matches('\'').to_string())
        .collect()
}

/// True iff any reject pattern matches `line` — i.e. the smoke would fail on it.
///
/// Uses `rg` with the pattern as a REGEX, matching the script's `rg -n "$pat"` invocation.
fn is_rejected(line: &str) -> Option<String> {
    for pat in reject_patterns() {
        let mut child = Command::new("rg")
            .arg("-n")
            .arg(&pat)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("rg must be available — the smoke script itself depends on it");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("write fixture");
        if child.wait().expect("rg exit").success() {
            return Some(pat);
        }
    }
    None
}

fn assert_accepted(line: &str, why: &str) {
    if let Some(pat) = is_rejected(line) {
        panic!("{why}\n  line:    {line}\n  matched: {pat}");
    }
}

fn assert_rejected(line: &str, why: &str) {
    assert!(
        is_rejected(line).is_some(),
        "{why}\n  line: {line}\n  no reject pattern matched it"
    );
}

// ── Benign structural / quiescent reporting must be ACCEPTED ────────────────────────────────

#[test]
fn capacity_256_is_accepted() {
    assert_accepted(
        "IPC_DIRECT_ACK_COUNTERS phase=settled dir=nr6 reserve=0 commit=0 consume=0 release=0 \
         release_after_consume=0 cancel=0 live=0 spent_released=0 high_watermark=0 capacity=256",
        "the structural ack-store bound is benign quiescent reporting",
    );
    assert_accepted(
        "IPC_DIRECT_PRODUCTION_ACK_QUIESCENT dir=nr7 reserve=0 commit=0 consume=0 release=0 \
         rel_after_consume=0 cancel=0 live=0 spent=0 high_watermark=0 capacity=256",
        "the production ack quiescent line is benign",
    );
}

#[test]
fn ack_capacity_256_is_accepted() {
    assert_accepted(
        "IPC_DIRECT_WAITER_CENSUS dir=nr6 waiters_current=10 waiters_high_watermark=10 \
         ack_capacity=256 eligible=0 live_leases=0",
        "the independent waiter census is benign quiescent reporting",
    );
}

#[test]
fn capacity_refused_zero_is_accepted() {
    assert_accepted(
        "IPC_DIRECT_ACK_FUSES phase=settled dir=nr6 capacity_refused=0 overwrite_fuse=0 stale=0 \
         foreign=0 duplicate=0 dup_release=0 crossed=0 not_committed=0",
        "a zero capacity-refusal fuse is the healthy state",
    );
}

#[test]
fn the_benign_deferred_capacity_ok_form_is_accepted() {
    assert_accepted(
        "EXIT_TASK_PREFLIGHT_OK tid=7 asid=3 deferred_capacity=ok result=ok",
        "`deferred_capacity=ok` is a success attestation, not an exhaustion report",
    );
}

// ── Genuine exhaustion must still be REJECTED ───────────────────────────────────────────────

#[test]
fn capacity_refused_one_is_rejected() {
    assert_rejected(
        "IPC_DIRECT_ACK_FUSES phase=settled dir=nr6 capacity_refused=1 overwrite_fuse=0 stale=0 \
         foreign=0 duplicate=0 dup_release=0 crossed=0 not_committed=0",
        "a real capacity refusal must still fail the smoke",
    );
}

#[test]
fn multi_digit_capacity_refusals_are_rejected() {
    for n in ["2", "9", "10", "42", "256"] {
        assert_rejected(
            &format!(
                "IPC_DIRECT_ACK_FUSES phase=settled dir=nr7 capacity_refused={n} \
                      overwrite_fuse=0 stale=0"
            ),
            "every non-zero refusal count must be rejected, not just single digits",
        );
    }
}

#[test]
fn every_named_exhaustion_form_is_rejected() {
    for (line, what) in [
        (
            "SHARED_REGION_CANCEL_FUSE_SET reason=capacity_exhausted result=fail_closed",
            "shared-region capacity exhaustion",
        ),
        (
            "IPC_SERVER_REPLY_LINK_REGISTER_FAIL server_tid=9 record_index=2 reason=capacity \
             result=rolled_back",
            "reply-link install capacity failure",
        ),
        (
            "FORK_COW_FAIL reason=cow_capacity kernel_error=Vm(Full) syscall_code=12",
            "fork COW capacity failure",
        ),
        (
            "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid=4 active_asid=2 va=0x1000 \
             reason=page_table_capacity",
            "kernel stack-map page-table exhaustion",
        ),
        (
            "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid=4 active_asid=2 va=0x1000 \
             reason=user_vm_capacity",
            "user VM capacity failure",
        ),
        (
            "EXIT_TASK_SYSCALL_DECLINED tid=5 reason=deferred_capacity link_retained=1 \
             result=would_block",
            "exit-deferral capacity decline",
        ),
        (
            "IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL waiter_tid=3 reason=CapacityFull cnode_used=8 \
             cnode_capacity=8",
            "reply-cap materialisation failure",
        ),
    ] {
        assert_rejected(line, what);
    }
}

// ── Pre-existing rejection semantics must remain active ─────────────────────────────────────

#[test]
fn existing_full_oom_and_fatal_rejections_remain_active() {
    // `Vm(Full)` is asserted on a line carrying NO other rejectable token, so the match
    // provably comes from the `Vm\(Full\)` pattern and not from a capacity term beside it.
    assert_eq!(
        is_rejected("FORK_COW_FAIL reason=asid_full kernel_error=Vm(Full) syscall_code=12")
            .as_deref(),
        Some(r"Vm\(Full\)"),
        "Vm(Full) must be rejected BY the Vm(Full) pattern, not incidentally"
    );
    for (line, what) in [
        ("ALLOC_FAIL reason=oom bytes=4096", "OOM"),
        ("kernel PANIC at src/foo.rs:1", "PANIC"),
        ("kernel FATAL: unrecoverable state", "FATAL"),
        ("ASSERT failed: invariant", "ASSERT"),
        ("PAGE_FAULT_UNHANDLED addr=0x0", "unhandled page fault"),
        (
            "RISCV_EARLY_TRAP scause=0x2 sepc=0x0 stval=0x0",
            "early trap",
        ),
        (
            "RISCV_TRAP_UNHANDLED scause=0x2 reason=trap_from_s_mode",
            "S-mode trap",
        ),
        ("UNEXPECTED_INLOCK_DISPATCH nr=6", "in-lock dispatch"),
        (
            "UNLOCK_GRADUATED_FALLBACK class=IpcCall",
            "graduated fallback",
        ),
        ("D2_PUBLISH_RACE_UNWIND tid=3", "publish-race unwind"),
    ] {
        assert_rejected(line, &format!("{what} rejection must remain active"));
    }
}

/// `CapabilityFull` and `TaskTableFull` surface only as the Debug field `kernel_error={:?}` of
/// `FORK_COW_FAIL`. They never contained the substring "capacity", so the retired word match
/// never covered them — they are now named explicitly.
#[test]
fn capability_full_and_task_table_full_are_rejected() {
    for err in ["CapabilityFull", "TaskTableFull"] {
        assert_rejected(
            &format!("FORK_COW_FAIL reason=cap_full kernel_error={err} syscall_code=12"),
            &format!("{err} exhaustion must fail the smoke"),
        );
    }
}

// ── The generic pattern must not come back ──────────────────────────────────────────────────

/// No broad `\bcapacity\b` rejection survives — as a PATTERN. (The script keeps a comment
/// explaining the retirement, which is not a pattern.)
#[test]
fn no_broad_generic_capacity_rejection_survives() {
    let pats = reject_patterns();
    assert!(
        !pats.iter().any(|p| p == r"\bcapacity\b"),
        "the generic word match must not return: {pats:?}"
    );
    for p in &pats {
        assert!(
            !(p.contains("capacity") && !p.contains('=')),
            "a capacity reject must be anchored to a field assignment, not a bare word: `{p}`"
        );
    }
    // …and the narrowed set is actually present.
    for required in [
        "capacity_refused=[1-9][0-9]*",
        "reason=capacity_exhausted",
        r"reason=capacity\b",
        "reason=deferred_capacity",
    ] {
        assert!(
            pats.iter().any(|p| p == required),
            "the narrowed set must contain `{required}`: {pats:?}"
        );
    }
}

/// The parse itself must be meaningful — a silently empty list would make every accept-test
/// pass vacuously.
#[test]
fn the_reject_pattern_list_parses_and_is_non_trivial() {
    let pats = reject_patterns();
    assert!(
        pats.len() >= 20,
        "expected the full reject list, parsed {}: {pats:?}",
        pats.len()
    );
    assert!(pats.iter().any(|p| p == r"Vm\(Full\)"));
    assert!(pats.iter().any(|p| p == r"\boom\b"));
}
