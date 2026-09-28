// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2-ACCEPTANCE §3 — behavioural controls for the RISC-V core smoke's retired-set census.
//!
//! The census omitted NR 0 (`Yield`), a default-on retired class since Stage 196G, so any strict
//! core boot of a `riscv64-ipc-reply-timeout-oracle` kernel in which init's oracle lanes actually
//! yielded failed with "serviced a syscall outside the retired set". The correction widens the sum by exactly that one term and pins it:
//! every NR 0 line must be the committed queue-advance disposition with its continuation captured,
//! paired one-to-one on its CPU with the `YIELD_SPLIT_COMMITTED` of the same tid.
//!
//! These tests are behavioural: they extract `riscv64_split_census` from the smoke script and run
//! THAT code with `bash` over fixture logs — the same function the smoke runs on a boot's log.

use std::process::Command;

const SMOKE: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");

fn census_fn() -> &'static str {
    let start = SMOKE
        .find("riscv64_split_census() {")
        .expect("the smoke defines riscv64_split_census");
    let end = SMOKE[start..]
        .find("\n# END riscv64_split_census")
        .expect("the census function is closed by its END marker");
    &SMOKE[start..start + end]
}

/// Run the census over `log` under the smoke's own shell options (`set -euo pipefail`, which a
/// command in the census must survive on every log, including one with no matches at all);
/// returns (failures, output).
fn census(log: &str) -> (u32, String) {
    let dir = std::env::temp_dir().join(format!(
        "yarm-riscv-census-{}-{}",
        std::process::id(),
        log.len() ^ log.lines().count() << 16 ^ fnv(log)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("boot.log");
    std::fs::write(&path, log).expect("fixture");
    let script = format!(
        "set -euo pipefail\n{}\nfailures=0\nTERMINAL_FAULT_ORACLE=0\nriscv64_split_census \"$1\"\necho \"FAILURES=$failures\"\n",
        census_fn()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .arg("census")
        .arg(&path)
        .output()
        .expect("bash and rg must be available — the smoke itself depends on them");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let n = text
        .lines()
        .find_map(|l| l.strip_prefix("FAILURES="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("census did not report: {text}"));
    (n, text)
}

fn fnv(s: &str) -> usize {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    }) as usize
}

const D: &str = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64";

/// Every retired class of a healthy default boot, in its pinned disposition.
fn healthy() -> String {
    let mut v = vec![
        format!("{D} nr=15 cpu=0 result=ok"),
        format!("{D} nr=10 cpu=0 result=ok"),
        format!("{D} nr=9 cpu=0 result=queue_advance_committed outgoing=3 captured=1"),
        format!("{D} nr=5 cpu=0 result=queue_advance_committed outgoing=3 captured=1"),
        format!("{D} nr=2 cpu=0 result=queue_advance_committed outgoing=4 captured=1"),
        format!("{D} nr=1 cpu=0 result=post_work_committed finalized=1"),
    ];
    v.extend((0..3).map(|_| format!("{D} nr=23 cpu=0 result=ok")));
    v.extend((0..5).map(|_| format!("{D} nr=29 cpu=0 result=ok")));
    v.join("\n") + "\n"
}

/// One oracle-lane yield (timeout-wins: the client waiting for the server's rejected late NR 7),
/// exactly as the bridge prints it.
fn yield_of(tid: u64) -> String {
    format!(
        "USER_LOG tid={tid} msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_TIMED_OUT\n\
         RISCV_YIELD_DISPATCH_DEFER_BEGIN cpu=0 outgoing={tid}\n\
         YIELD_SPLIT_COMMITTED cpu=0 tid={tid}\n\
         {D} nr=0 cpu=0 result=queue_advance_committed outgoing={tid} captured=1\n\
         QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed\n"
    )
}

#[test]
fn legitimate_nr0_traffic_passes() {
    let (n, out) = census(&(healthy() + &yield_of(1)));
    assert_eq!(n, 0, "{out}");
    assert!(
        out.contains("RISC-V NR 0 (Yield) split dispatches: 1,"),
        "{out}"
    );
    // Two yields in one boot (measured live) are two paired dispatches.
    let (n, out) = census(&(healthy() + &yield_of(1) + &yield_of(1)));
    assert_eq!(n, 0, "{out}");
    // A boot that never yields is still a healthy census (a regression smoke, not a witness).
    let (n, out) = census(&healthy());
    assert_eq!(n, 0, "{out}");
    assert!(out.contains("split dispatches: 0,"), "{out}");
}

#[test]
fn an_unexpected_nr_still_fails() {
    let (n, out) = census(&(healthy() + &yield_of(1) + &format!("{D} nr=3 cpu=0 result=ok\n")));
    assert_eq!(n, 1, "{out}");
    assert!(
        out.contains("serviced a syscall outside the retired set"),
        "{out}"
    );
    assert!(out.contains("nr0=1"), "{out}");
}

#[test]
fn missing_or_inconsistent_nr0_accounting_still_fails() {
    let wrong = |mutate: &dyn Fn(&str) -> String| {
        let (n, out) = census(&(healthy() + &mutate(&yield_of(1))));
        assert!(n >= 1, "accepted:\n{out}");
        assert!(
            out.contains("RISC-V Yield split dispatch is not reconciled"),
            "{out}"
        );
    };
    // A dispatch with no committed yield behind it.
    wrong(&|y| y.replace("YIELD_SPLIT_COMMITTED cpu=0 tid=1\n", ""));
    // A committed yield whose dispatch is missing from the accounting.
    wrong(&|y| {
        y.lines()
            .filter(|l| !l.contains(" nr=0 "))
            .map(|l| format!("{l}\n"))
            .collect()
    });
    // An excess dispatch: two NR 0 lines for one commit.
    wrong(&|y| format!("{y}{D} nr=0 cpu=0 result=queue_advance_committed outgoing=1 captured=1\n"));
    // Another disposition, a lost continuation, another task's dispatch, another CPU's commit.
    wrong(&|y| {
        y.replace(
            "result=queue_advance_committed outgoing=1",
            "result=ok outgoing=1",
        )
    });
    wrong(&|y| y.replace("outgoing=1 captured=1", "outgoing=1 captured=0"));
    wrong(&|y| {
        y.replace(
            "nr=0 cpu=0 result=queue_advance_committed outgoing=1",
            "nr=0 cpu=0 result=queue_advance_committed outgoing=2",
        )
    });
    wrong(&|y| {
        y.replace(
            "YIELD_SPLIT_COMMITTED cpu=0 tid=1",
            "YIELD_SPLIT_COMMITTED cpu=1 tid=1",
        )
    });
}

#[test]
fn the_sum_is_widened_by_exactly_nr0() {
    let f = census_fn();
    assert!(f.contains(
        "riscv_split_nr2 + riscv_split_nr1 + riscv_split_nr23 + riscv_split_nr29 + riscv_split_nr0 )); then"
    ));
    assert_eq!(f.matches("if (( riscv_split_total != ").count(), 1);
    // The smoke runs the census once, on its own boot log.
    assert_eq!(
        SMOKE.matches("\nriscv64_split_census \"$LOGFILE\"").count(),
        1
    );
}
