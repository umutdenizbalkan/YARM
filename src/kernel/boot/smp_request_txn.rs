// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP1-ACCEPTANCE §4 — the x86_64 cross-CPU request oracle's TRANSACTION RECORD.
//!
//! The `ap-cross-cpu-user-consume` profile used to be graded on one-shot log lines, some of them
//! written through the asynchronous printk ring, which silently skips a record another CPU is
//! still writing when a drain overtakes it. A complete transaction could therefore fail its grade
//! (the server-blocked line lost at base and at head alike) while every step had happened.
//!
//! Here each step is recorded ONCE, at the production owner that commits it, with the exact
//! identities that owner holds, and stamped with a global step sequence so the grader can check
//! causal order:
//!
//! | step         | owner                                                   | identities            |
//! |--------------|---------------------------------------------------------|-----------------------|
//! | `Blocked`    | the blocked-server marker body, after its re-verification| server {tid, asid}, endpoint {index, generation}, ack seq |
//! | `Delivered`  | the request drain, after the accepted transaction and the remote enqueue | the same five, the client TID, sender/target CPU |
//! | `IpiSent`    | the same drain, after the ICR write                     | sender/target CPU, server TID |
//! | `IpiObserved`| the target's dispatch hook, on the IPI-driven wake      | CPU                   |
//! | `Resumed`    | the AP saved-frame resume, after its authenticated snapshot | CPU, {tid, asid}   |
//! | `Continued`  | the resumed server's `X86_AP_RECV_V2_CONTINUED` DebugLog | logging TID, CPU     |
//! | `Validated`  | the resumed server's `X86_AP_RECV_V2_USER_VALIDATED` DebugLog | logging TID, CPU |
//!
//! The IPI's hardware arrival is not recorded here: the pure-asm 0xF1 stub already counts it per
//! CPU and by origin (`percpu::remote_wake_arrivals`), and the seal reads that owner's counters.
//!
//! When `Validated` is recorded the chain has settled, and the record is reported SYNCHRONOUSLY
//! (`printk_emit_sync`) — one line per step, derived from the recorded state, then the seal.
//! Nothing is inferred from a log line, a proposed operation is never recorded (each owner records
//! after its commit), and a second record of a step is counted as a duplicate rather than
//! overwriting the first.
//!
//! Scope: only the x86_64 SMP request oracle's workload reaches the recording sites (each is gated
//! by `x86_ipccall_direct_smp_request_enabled()`); no production decision reads this record.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The transaction's steps, in causal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Step {
    Blocked = 0,
    Delivered = 1,
    IpiSent = 2,
    IpiObserved = 3,
    Resumed = 4,
    Continued = 5,
    Validated = 6,
}

pub(crate) const STEPS: usize = 7;

impl Step {
    pub(crate) const ALL: [Step; STEPS] = [
        Step::Blocked,
        Step::Delivered,
        Step::IpiSent,
        Step::IpiObserved,
        Step::Resumed,
        Step::Continued,
        Step::Validated,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Step::Blocked => "blocked",
            Step::Delivered => "delivered",
            Step::IpiSent => "ipi_sent",
            Step::IpiObserved => "ipi_observed",
            Step::Resumed => "resumed",
            Step::Continued => "continued",
            Step::Validated => "validated",
        }
    }
}

/// Up to six identity words per step; unused words stay 0.
pub(crate) const FACTS: usize = 6;

struct StepRecord {
    claimed: AtomicBool,
    seq: AtomicU64,
    facts: [AtomicU64; FACTS],
    duplicates: AtomicU32,
}

impl StepRecord {
    const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            facts: [const { AtomicU64::new(0) }; FACTS],
            duplicates: AtomicU32::new(0),
        }
    }
}

static RECORDS: [StepRecord; STEPS] = [const { StepRecord::new() }; STEPS];
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);
static SEALED: AtomicBool = AtomicBool::new(false);

/// Record `step` with `facts`, once. The identities are stored BEFORE the sequence is published,
/// so a reader that sees a nonzero sequence sees the whole record. Returns `false` (and counts a
/// duplicate) if the step was already recorded.
pub(crate) fn record(step: Step, facts: [u64; FACTS]) -> bool {
    let rec = &RECORDS[step as usize];
    if rec.claimed.swap(true, Ordering::AcqRel) {
        rec.duplicates.fetch_add(1, Ordering::AcqRel);
        return false;
    }
    for (slot, value) in rec.facts.iter().zip(facts) {
        slot.store(value, Ordering::Release);
    }
    let seq = NEXT_SEQ.fetch_add(1, Ordering::AcqRel) + 1;
    rec.seq.store(seq, Ordering::Release);
    true
}

/// One step as recorded: `(seq, facts, duplicates)`; `seq == 0` means never recorded.
pub(crate) fn read(step: Step) -> (u64, [u64; FACTS], u32) {
    let rec = &RECORDS[step as usize];
    let seq = rec.seq.load(Ordering::Acquire);
    let mut facts = [0u64; FACTS];
    for (out, slot) in facts.iter_mut().zip(rec.facts.iter()) {
        *out = slot.load(Ordering::Acquire);
    }
    (seq, facts, rec.duplicates.load(Ordering::Acquire))
}

/// Pack an endpoint `{index, generation}` into one identity word.
pub(crate) const fn endpoint_word(index: usize, generation: u64) -> u64 {
    ((index as u64) << 32) | (generation & 0xFFFF_FFFF)
}

/// Pack a sender/target CPU pair into one identity word.
pub(crate) const fn cpu_pair(sender: u8, target: u8) -> u64 {
    ((sender as u64) << 8) | target as u64
}

/// The resumed server's userspace markers, observed on the DebugLog path with the logging task's
/// own TID and CPU. `Validated` is the last step: recording it settles the chain and reports it.
pub(crate) fn observe_user_marker(msg: &str, tid: u64, cpu: u8) {
    if msg.starts_with("X86_AP_RECV_V2_CONTINUED") {
        record(Step::Continued, [tid, cpu as u64, 0, 0, 0, 0]);
    } else if msg.starts_with("X86_AP_RECV_V2_USER_VALIDATED") {
        record(Step::Validated, [tid, cpu as u64, 0, 0, 0, 0]);
        emit_seal_once(cpu);
    }
}

/// Report the settled chain from the record, synchronously, exactly once.
fn emit_seal_once(target_cpu: u8) {
    if SEALED.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut recorded = 0usize;
    let mut duplicates = 0u32;
    for step in Step::ALL {
        let (seq, f, dup) = read(step);
        recorded += usize::from(seq != 0);
        duplicates += dup;
        crate::kernel::printk::printk_emit_sync(format_args!(
            "X86_SMP_REQUEST_TXN step={} seq={} f0={} f1={} f2={} f3={} f4={} f5={} dup={}",
            step.name(),
            seq,
            f[0],
            f[1],
            f[2],
            f[3],
            f[4],
            f[5],
            dup
        ));
    }
    let (total, kernel_origin, user_origin) = wake_arrivals(target_cpu);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "X86_SMP_REQUEST_TXN_SEAL steps={} recorded={} duplicates={} target_cpu={} target_arrivals={} kernel_origin={} user_origin={} result=reported",
        STEPS, recorded, duplicates, target_cpu, total, kernel_origin, user_origin
    ));
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn wake_arrivals(cpu: u8) -> (u32, u32, u32) {
    crate::arch::x86_64::percpu::remote_wake_arrivals(crate::kernel::scheduler::CpuId(cpu))
}

#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
fn wake_arrivals(_cpu: u8) -> (u32, u32, u32) {
    (0, 0, 0)
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    for rec in &RECORDS {
        rec.claimed.store(false, Ordering::Release);
        rec.seq.store(0, Ordering::Release);
        for f in &rec.facts {
            f.store(0, Ordering::Release);
        }
        rec.duplicates.store(0, Ordering::Release);
    }
    NEXT_SEQ.store(0, Ordering::Release);
    SEALED.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_recorded_once_in_sequence_and_a_repeat_is_a_duplicate() {
        reset_for_test();
        assert!(record(Step::Blocked, [5, 6, endpoint_word(3, 1), 9, 0, 0]));
        assert!(record(
            Step::Delivered,
            [5, 6, endpoint_word(3, 1), 9, 21, cpu_pair(0, 1)]
        ));
        assert!(
            !record(Step::Blocked, [7, 7, 7, 7, 7, 7]),
            "never overwritten"
        );
        let (s0, f0, d0) = read(Step::Blocked);
        let (s1, _, d1) = read(Step::Delivered);
        assert_eq!((s0, s1), (1, 2), "causal order is the record order");
        assert_eq!(f0, [5, 6, (3 << 32) | 1, 9, 0, 0]);
        assert_eq!((d0, d1), (1, 0));
        assert_eq!(
            read(Step::Validated).0,
            0,
            "an unrecorded step reads as absent"
        );
        reset_for_test();
    }
}
