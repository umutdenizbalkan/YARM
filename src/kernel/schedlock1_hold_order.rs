// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK1-ORDER1 — the SCHED-LOCK1 witness's in-hold membership rule.
//!
//! The witness's records carry one global order (their 1-based record numbers). The grader places
//! a section inside a hold exactly when its records lie strictly between the hold's `HOLD_BEGIN`
//! and `HOLD_END`. The producer must decide credit by the same order: an observer section reserves
//! its records first and checks for an open hold afterwards, so a hold can open between the two —
//! that section's records then PRECEDE `HOLD_BEGIN`, and crediting it (counting its commit,
//! offering its installation to `PROGRESS`) made a correct trace grade as failed.
//! Witness-only instrumentation: compiled with `sched-lock1-witness` and in hosted tests only.

/// Whether a section whose first record is `section_seq` earns credit in the hold whose
/// `HOLD_BEGIN` is record `hold_begin_seq`. `0` means the section's records could not be reserved
/// (overflow, so nothing of it is in the trace); `u64::MAX` means no hold's begin is known.
#[must_use]
pub(crate) const fn credited_in_hold(section_seq: u64, hold_begin_seq: u64) -> bool {
    section_seq != 0 && hold_begin_seq != u64::MAX && section_seq > hold_begin_seq
}

#[cfg(test)]
mod tests {
    use super::credited_in_hold;

    /// The boundary the defect lived on: a section reserved just before the hold's begin is not
    /// in the hold, even if it observes the hold open; the first section after it is.
    #[test]
    fn a_section_whose_records_precede_hold_begin_earns_no_credit() {
        // FUTEX-TIME1's failing trace: observer SEC 18 / REM 19, HOLD_BEGIN 20.
        assert!(!credited_in_hold(18, 20));
        assert!(!credited_in_hold(19, 20));
        assert!(!credited_in_hold(20, 20), "a record cannot be its own hold's begin");
        assert!(credited_in_hold(21, 20), "the first record after the begin is in the hold");
        assert!(credited_in_hold(22, 20));
    }

    #[test]
    fn unknown_begin_or_unrecorded_section_credits_nothing() {
        assert!(!credited_in_hold(0, 20), "a section with no records is not in the trace");
        assert!(!credited_in_hold(25, u64::MAX), "no known begin, no credit");
        assert!(!credited_in_hold(u64::MAX, u64::MAX));
    }
}
