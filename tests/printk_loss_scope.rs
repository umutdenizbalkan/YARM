// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! BL5d — an asynchronous one-shot line that is absent was never written, unless its loss is
//! announced.
//!
//! The printk drain used to skip a record another CPU had reserved but not finished writing and
//! advance past it, so a completed transaction's one-shot marker could vanish without trace and
//! read as a transaction that never happened. The drain now holds an in-flight record for its
//! writer's own flush and announces a genuinely lost (overwritten) record in place with
//! `PRINTK_RECORDS_LOST`. The behavioural tests of the drain live beside it in
//! `src/kernel/printk.rs`; this pins that the SMP graders which count one-shot lines treat an
//! announced loss as incomplete evidence rather than as a missing transaction.

const PRINTK: &str = include_str!("../src/kernel/printk.rs");
const GRADERS: [(&str, &str); 5] = [
    (
        "ap-recv-v2-block",
        include_str!("../scripts/qemu-x86_64-ap-recv-v2-block-smoke.sh"),
    ),
    (
        "ipccall-direct-smp-request",
        include_str!("../scripts/qemu-ipccall-direct-x86_64-smp-request-smoke.sh"),
    ),
    (
        "ap-cross-cpu-request",
        include_str!("../scripts/qemu-x86_64-ap-cross-cpu-request-smoke.sh"),
    ),
    (
        "ap-cross-cpu-reply",
        include_str!("../scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh"),
    ),
    (
        "ap-cross-cpu-user-consume",
        include_str!("../scripts/qemu-x86_64-ap-cross-cpu-user-consume-smoke.sh"),
    ),
];

#[test]
fn the_drain_announces_exactly_the_marker_the_graders_read() {
    assert!(PRINTK.contains("\"PRINTK_RECORDS_LOST first_seq={} count={} total_dropped={}\""));
    // An in-flight record stops the drain; it is not skipped.
    assert!(
        PRINTK.contains("// In flight: reserved, not yet committed. Hold it.\n            break;")
    );
}

#[test]
fn every_one_shot_grader_reports_an_announced_loss_before_counting() {
    for (name, src) in GRADERS {
        let check = "ring_lost=\"$(count \"PRINTK_RECORDS_LOST first_seq=\")\"";
        let at = src
            .find(check)
            .unwrap_or_else(|| panic!("{name}: no loss check"));
        assert!(
            src[at..].contains("this boot's one-shot evidence is incomplete"),
            "{name}: an announced loss is reported as incomplete evidence"
        );
        let have = src
            .find("have()  { rg -a -q -F \"$1\" \"$NORM\"; }")
            .unwrap();
        assert!(have < at, "{name}: checked once the log is normalized");
        // Checked before the first one-shot count it qualifies.
        if let Some(first) = src.find("IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1\")") {
            assert!(at < first, "{name}: checked before the one-shot count");
        }
    }
}
