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
//! causal order (`IpiSent` is recorded once the ICR write has returned, which the target may have
//! answered already: it follows `Delivered` but is not ordered against the target's steps):
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
//! QEMU-SMP1-SEAL — COMPLETION, not one named step, authorizes the report. `Validated` is not the
//! last step in time: the sender records `IpiSent` after its ICR write returns, which the target
//! may already have answered and validated. Protocol:
//!
//! 1. **publish** (`record`): claim the step's slot (a repeat only counts a duplicate), store its
//!    facts, store its sequence, and only then count it into `PUBLISHED` with one read-modify-write.
//!    A claimed slot whose facts are unfinished is not counted, so it never makes the record
//!    complete.
//! 2. **attempt**, after every successful publication: ready iff `PUBLISHED == STEPS`. The
//!    publication whose increment completes the count always observes it afterwards (coherence of
//!    one atomic), so the final report is never missed, however the last publications interleave;
//!    every increment is a release in one RMW chain, so the acquiring reader sees every record whole.
//! 3. **claim**: only a ready attempt swaps the one-time `SEALED` latch, so two final publications
//!    completing together cannot both report; a permanently missing step never consumes it.
//! 4. **report**, synchronously (`printk_emit_sync`), by whichever CPU won: one line per step from
//!    its snapshot, then the seal, whose target CPU is the one the recorded delivery names — never
//!    the reporting CPU. Nothing waits, and nothing is printed before the record is complete.
//!
//! Nothing is inferred from a log line, a proposed operation is never recorded (each owner records
//! after its commit), a published record is never rewritten, and invalid facts are reported as
//! recorded — the grader, not the reporter, rejects them.
//!
//! Scope: only the x86_64 SMP request oracle's workload reaches the recording sites (each is gated
//! by `x86_ipccall_direct_smp_request_enabled()`); no production decision reads this record.
// Only the x86_64 request oracle records and reports; the other ports compile the shared owners'
// call sites but never reach the report.
#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]

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
/// How many steps are fully published (facts and sequence stored): the completion count.
static PUBLISHED: AtomicU32 = AtomicU32::new(0);
/// The one-time report latch, claimed only by an attempt that found the record complete.
static SEALED: AtomicBool = AtomicBool::new(false);

/// Record `step` with `facts`, once, then attempt the report. Returns `false` (and counts a
/// duplicate) if the step was already recorded; a duplicate attempts nothing.
pub(crate) fn record(step: Step, facts: [u64; FACTS]) -> bool {
    if !publish(step, facts) {
        return false;
    }
    if let Some(report) = attempt_seal() {
        emit(&report);
    }
    true
}

/// Protocol step 1. The identities and the sequence are stored BEFORE the step is counted, so an
/// attempt that sees the count complete sees every record whole.
fn publish(step: Step, facts: [u64; FACTS]) -> bool {
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
    PUBLISHED.fetch_add(1, Ordering::AcqRel);
    true
}

/// The settled transaction, as the winning attempt read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SealReport {
    /// `(seq, facts, duplicates)` per step, in [`Step::ALL`] order.
    pub(crate) steps: [(u64, [u64; FACTS], u32); STEPS],
    pub(crate) recorded: usize,
    pub(crate) duplicates: u32,
    /// The wake target the recorded delivery names.
    pub(crate) target_cpu: u8,
}

/// Protocol steps 2 and 3: readiness, then the one-time claim, then the snapshot.
fn attempt_seal() -> Option<SealReport> {
    if PUBLISHED.load(Ordering::Acquire) as usize != STEPS {
        return None;
    }
    if SEALED.swap(true, Ordering::AcqRel) {
        return None;
    }
    let mut steps = [(0u64, [0u64; FACTS], 0u32); STEPS];
    let mut recorded = 0usize;
    let mut duplicates = 0u32;
    for (out, step) in steps.iter_mut().zip(Step::ALL) {
        *out = read(step);
        recorded += usize::from(out.0 != 0);
        duplicates += out.2;
    }
    let target_cpu = (steps[Step::Delivered as usize].1[5] & 0xFF) as u8;
    Some(SealReport {
        steps,
        recorded,
        duplicates,
        target_cpu,
    })
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
/// own TID and CPU. Like every other step, recording one attempts the report.
pub(crate) fn observe_user_marker(msg: &str, tid: u64, cpu: u8) {
    if msg.starts_with("X86_AP_RECV_V2_CONTINUED") {
        record(Step::Continued, [tid, cpu as u64, 0, 0, 0, 0]);
    } else if msg.starts_with("X86_AP_RECV_V2_USER_VALIDATED") {
        record(Step::Validated, [tid, cpu as u64, 0, 0, 0, 0]);
    }
}

/// Protocol step 4: report the settled chain synchronously, from the winning snapshot.
fn emit(report: &SealReport) {
    #[cfg(test)]
    {
        REPORTS.fetch_add(1, Ordering::AcqRel);
        *LAST_REPORT.lock().unwrap_or_else(|e| e.into_inner()) = Some(*report);
    }
    for (step, (seq, f, dup)) in Step::ALL.into_iter().zip(report.steps) {
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
    let (total, kernel_origin, user_origin) = wake_arrivals(report.target_cpu);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "X86_SMP_REQUEST_TXN_SEAL steps={} recorded={} duplicates={} target_cpu={} target_arrivals={} kernel_origin={} user_origin={} result=reported",
        STEPS,
        report.recorded,
        report.duplicates,
        report.target_cpu,
        total,
        kernel_origin,
        user_origin
    ));
}

/// Reports emitted since the last reset, and the last one (tests only).
#[cfg(test)]
static REPORTS: AtomicU32 = AtomicU32::new(0);
#[cfg(test)]
static LAST_REPORT: std::sync::Mutex<Option<SealReport>> = std::sync::Mutex::new(None);

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
    PUBLISHED.store(0, Ordering::Release);
    SEALED.store(false, Ordering::Release);
    REPORTS.store(0, Ordering::Release);
    *LAST_REPORT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record is process-global; its tests run one at a time.
    static TXN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn txn_guard() -> std::sync::MutexGuard<'static, ()> {
        TXN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    const SRV: u64 = 20205;
    const ASID: u64 = 5;
    const CLIENT: u64 = 21205;

    fn facts(step: Step, target: u8) -> [u64; FACTS] {
        let ep = endpoint_word(6, 1);
        let pair = cpu_pair(0, target);
        match step {
            Step::Blocked => [SRV, ASID, ep, 1, 0, 0],
            Step::Delivered => [SRV, ASID, ep, 1, CLIENT, pair],
            Step::IpiSent => [pair, SRV, 0, 0, 0, 0],
            Step::IpiObserved => [u64::from(target), 0, 0, 0, 0, 0],
            Step::Resumed => [u64::from(target), SRV, ASID, 0, 0, 0],
            Step::Continued | Step::Validated => [SRV, u64::from(target), 0, 0, 0, 0],
        }
    }

    fn reports() -> u32 {
        REPORTS.load(Ordering::Acquire)
    }

    fn last_report() -> SealReport {
        LAST_REPORT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .expect("a report was emitted")
    }

    /// A report is complete when all seven steps carry a sequence and the facts they recorded.
    fn assert_complete(r: &SealReport, target: u8) {
        assert_eq!(r.recorded, STEPS);
        for (step, (seq, f, _)) in Step::ALL.into_iter().zip(r.steps) {
            assert_ne!(seq, 0, "{step:?} reported unpublished");
            assert_eq!(f, facts(step, target), "{step:?} reported with other facts");
        }
    }

    /// QEMU-SMP1-SEAL §1 — the target CPU validates (and the chain looks settled) while the sender
    /// has sent the IPI but not yet recorded `IpiSent`. Nothing may be sealed until that record is
    /// published, and it must then be sealed exactly once, complete. (At `e14c914a` this failed:
    /// `Validated` sealed a six-step record and the late `IpiSent` was never reported.)
    #[test]
    fn a_late_sender_record_is_not_sealed_over() {
        let _g = txn_guard();
        reset_for_test();
        for step in [
            Step::Blocked,
            Step::Delivered,
            Step::IpiObserved,
            Step::Resumed,
        ] {
            assert!(record(step, facts(step, 1)));
        }
        observe_user_marker("X86_AP_RECV_V2_CONTINUED cpu=1", SRV, 1);
        observe_user_marker("X86_AP_RECV_V2_USER_VALIDATED cpu=1", SRV, 1);
        assert_eq!(read(Step::IpiSent).0, 0, "the sender has not recorded yet");
        assert!(
            !SEALED.load(Ordering::Acquire),
            "sealed while IpiSent was unpublished: the report is incomplete and final"
        );
        assert_eq!(
            reports(),
            0,
            "nothing is printed before the record is complete"
        );
        assert!(record(Step::IpiSent, facts(Step::IpiSent, 1)));
        assert!(
            SEALED.load(Ordering::Acquire),
            "the late record completes the transaction"
        );
        assert_eq!(reports(), 1);
        let r = last_report();
        assert_complete(&r, 1);
        assert!(r.steps[Step::IpiSent as usize].0 > r.steps[Step::Validated as usize].0);
        reset_for_test();
    }

    /// §3 — the ordinary order reports once, only at the seventh publication.
    #[test]
    fn the_ordinary_order_is_sealed_once_at_completion() {
        let _g = txn_guard();
        reset_for_test();
        for (i, step) in Step::ALL.into_iter().enumerate() {
            assert!(record(step, facts(step, 1)));
            let want = u32::from(i + 1 == STEPS);
            assert_eq!(reports(), want, "after {step:?}");
        }
        let r = last_report();
        assert_complete(&r, 1);
        assert_eq!((r.duplicates, r.target_cpu), (0, 1));
        reset_for_test();
    }

    /// §3 — a step that never arrives: no report, and the latch is not consumed; later
    /// duplicates of recorded steps neither complete the record nor report.
    #[test]
    fn a_permanently_missing_step_never_seals_or_consumes_the_latch() {
        let _g = txn_guard();
        reset_for_test();
        for step in Step::ALL {
            if step != Step::IpiObserved {
                assert!(record(step, facts(step, 1)));
            }
        }
        assert!(!record(Step::Validated, facts(Step::Validated, 1)));
        assert!(!record(Step::Blocked, [7; FACTS]));
        assert!(attempt_seal().is_none());
        assert_eq!(reports(), 0);
        assert!(
            !SEALED.load(Ordering::Acquire),
            "the latch is still unclaimed"
        );
        assert_eq!(PUBLISHED.load(Ordering::Acquire) as usize, STEPS - 1);
        reset_for_test();
    }

    fn publish_all_but(last_two: [Step; 2], target: u8) {
        for step in Step::ALL {
            if !last_two.contains(&step) {
                assert!(record(step, facts(step, target)));
            }
        }
        assert_eq!(reports(), 0);
    }

    /// §3 — two final publications complete together: both see the record complete and attempt;
    /// the latch lets exactly one report.
    #[test]
    fn two_final_publications_attempting_together_report_once() {
        let _g = txn_guard();
        reset_for_test();
        let last = [Step::IpiSent, Step::Validated];
        publish_all_but(last, 1);
        assert!(publish(last[0], facts(last[0], 1)));
        assert!(publish(last[1], facts(last[1], 1)));
        let a = attempt_seal();
        let b = attempt_seal();
        assert_eq!(
            u32::from(a.is_some()) + u32::from(b.is_some()),
            1,
            "exactly one of the competing attempts reports"
        );
        assert_complete(&a.or(b).expect("one report"), 1);
        reset_for_test();
    }

    /// §3 — the other interleaving of the same two publications: the first attempt finds the
    /// record incomplete, the second (whose increment completed it) reports. No completion is lost.
    #[test]
    fn the_publication_that_completes_the_count_always_reports() {
        let _g = txn_guard();
        reset_for_test();
        let last = [Step::Validated, Step::IpiSent];
        publish_all_but(last, 1);
        assert!(publish(last[0], facts(last[0], 1)));
        assert!(attempt_seal().is_none(), "six of seven: not ready");
        assert!(!SEALED.load(Ordering::Acquire));
        assert!(publish(last[1], facts(last[1], 1)));
        let r = attempt_seal().expect("the completing publication reports");
        assert_complete(&r, 1);
        reset_for_test();
    }

    /// §3 — the same race on real threads through `record`: every round reports exactly once, and
    /// completely.
    #[test]
    fn concurrent_final_records_report_exactly_once() {
        let _g = txn_guard();
        for round in 0..400 {
            reset_for_test();
            let last = [Step::IpiSent, Step::Validated];
            publish_all_but(last, 1);
            let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
            let workers: std::vec::Vec<_> = last
                .into_iter()
                .map(|step| {
                    let gate = gate.clone();
                    std::thread::spawn(move || {
                        gate.wait();
                        assert!(record(step, facts(step, 1)));
                    })
                })
                .collect();
            for w in workers {
                w.join().expect("worker");
            }
            assert_eq!(reports(), 1, "round {round}");
            assert_complete(&last_report(), 1);
        }
        reset_for_test();
    }

    /// §3 — the sender CPU publishes last and reports: the seal names the target the recorded
    /// delivery names, not the reporting CPU (the old seal used the validating CPU).
    #[test]
    fn the_seal_names_the_recorded_target_whoever_reports() {
        let _g = txn_guard();
        for target in [1u8, 3] {
            reset_for_test();
            for step in Step::ALL {
                if step != Step::IpiSent {
                    assert!(record(step, facts(step, target)));
                }
            }
            assert_eq!(reports(), 0);
            assert!(record(Step::IpiSent, facts(Step::IpiSent, target)));
            let r = last_report();
            assert_eq!(r.target_cpu, target);
            assert_complete(&r, target);
        }
        reset_for_test();
    }

    /// §3 — a complete but INVALID transaction is reported as recorded (the grader rejects it);
    /// duplicates are counted into the report and a published record is never rewritten.
    #[test]
    fn invalid_or_duplicated_facts_are_reported_as_recorded() {
        let _g = txn_guard();
        reset_for_test();
        let substituted = [u64::from(1u8), 4242, ASID, 0, 0, 0];
        for step in Step::ALL {
            if step == Step::Validated {
                assert!(!record(Step::Resumed, facts(Step::Resumed, 1)), "duplicate");
            }
            let f = if step == Step::Resumed {
                substituted
            } else {
                facts(step, 1)
            };
            assert!(record(step, f));
        }
        assert_eq!(reports(), 1);
        let r = last_report();
        assert_eq!(r.recorded, STEPS);
        assert_eq!(
            r.steps[Step::Resumed as usize].1,
            substituted,
            "first record kept"
        );
        assert_eq!((r.steps[Step::Resumed as usize].2, r.duplicates), (1, 1));
        assert!(
            !record(Step::Blocked, [9; FACTS]),
            "after the seal: still a duplicate"
        );
        assert_eq!(reports(), 1, "and no second report");
        assert_eq!(read(Step::Blocked).1, facts(Step::Blocked, 1));
        reset_for_test();
    }

    #[test]
    fn a_step_is_recorded_once_in_sequence_and_a_repeat_is_a_duplicate() {
        let _g = txn_guard();
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
