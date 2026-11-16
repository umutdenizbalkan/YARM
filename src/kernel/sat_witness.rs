// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-SAT-EVIDENCE1 — witness-only: what the home queue held when a placement was refused.
//!
//! The saturation witnesses (sched-time case 8 and its timed IPC receive, FUTEX-TIME1 sequence E)
//! must prove that the SPECIFIC waiter's home queue, at the waiter's priority, was full when its
//! deadline was collected. A spawn refusal or a filler count cannot show that: a spawn is refused
//! for its own target CPU, and rotation changes queue lengths every quantum. The only reliable
//! witness is the refusal itself, so [`record`] runs inside `reserve_enqueue_on_cpu_locked`'s
//! refusal, under the home CPU's 1L, and keeps the queue's length and promised slots at that
//! priority as the reservation saw them. The collector that was refused then emits one
//! `SATW_REFUSED` line from [`take`] (no lock: the record is published by atomics).
//!
//! Compiled only into hosted tests and the two saturation witness builds; no production path
//! reads it, and it changes no decision.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::kernel::scheduler::MAX_CPUS;

const EMPTY: u64 = u64::MAX;

/// Per home CPU: `[tid, priority, queued, reserved, reason]` of its latest refused reservation.
/// `tid` is written last (Release) and read first and last (Acquire), so a reader that sees the
/// same tid on both sides read one record's fields.
static FACTS: [[AtomicU64; 5]; MAX_CPUS] =
    [const { [const { AtomicU64::new(EMPTY) }; 5] }; MAX_CPUS];

/// Why a reservation was refused.
pub(crate) const REASON_QUEUE_FULL: u64 = 0;
/// Offline, wake-only or a plan for another CPU: not a capacity refusal.
pub(crate) const REASON_OTHER: u64 = 1;

/// The facts of one refused reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) home: usize,
    pub(crate) priority: u64,
    pub(crate) queued: u64,
    pub(crate) reserved: u64,
    pub(crate) reason: u64,
}

impl Refusal {
    /// The queue at that priority had no user slot left: `queued + reserved >= cap`.
    pub(crate) fn full(&self) -> bool {
        self.reason == REASON_QUEUE_FULL
            && self.queued + self.reserved >= crate::kernel::scheduler::MAX_RUN_QUEUE as u64
    }
}

/// rank 1L (the home's, held by the caller) — publish a refusal of `tid` on `home`.
pub(crate) fn record(
    home: usize,
    tid: u64,
    priority: u64,
    queued: usize,
    reserved: usize,
    reason: u64,
) {
    let Some(slot) = FACTS.get(home) else { return };
    slot[0].store(EMPTY, Ordering::Release);
    slot[1].store(priority, Ordering::Relaxed);
    slot[2].store(queued as u64, Ordering::Relaxed);
    slot[3].store(reserved as u64, Ordering::Relaxed);
    slot[4].store(reason, Ordering::Relaxed);
    slot[0].store(tid, Ordering::Release);
}

/// The latest refusal of `tid` on any CPU, consumed; `None` if another refusal on that CPU has
/// replaced it (the caller then reports `facts=lost`, which proves nothing).
pub(crate) fn take(tid: u64) -> Option<Refusal> {
    for (home, slot) in FACTS.iter().enumerate() {
        if slot[0].load(Ordering::Acquire) != tid {
            continue;
        }
        let r = Refusal {
            home,
            priority: slot[1].load(Ordering::Relaxed),
            queued: slot[2].load(Ordering::Relaxed),
            reserved: slot[3].load(Ordering::Relaxed),
            reason: slot[4].load(Ordering::Relaxed),
        };
        if slot[0]
            .compare_exchange(tid, EMPTY, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(r);
        }
    }
    None
}

/// Refusals logged per cycle: a cycle kept waiting on a full queue is refused on every pass of
/// every CPU's collector, so the log keeps the first few and numbers every one (`n`).
pub(crate) const LOGGED_PER_CYCLE: u64 = 4;
const CYCLES: usize = 32;
/// `[key, count]` per cycle; key = `(tid << 32 | gen as u32) | 1 << 63` (never 0).
static SEEN: [[AtomicU64; 2]; CYCLES] = [const { [const { AtomicU64::new(0) }; 2] }; CYCLES];

/// The ordinal (1-based) of this refusal of cycle `(tid, generation)`. A full table counts every
/// further cycle as its first refusal (logged): nothing is ever suppressed for lack of a slot.
pub(crate) fn ordinal(tid: u64, generation: u64) -> u64 {
    let key = (tid << 32) | (generation & 0xFFFF_FFFF) | (1 << 63);
    for slot in &SEEN {
        let k = slot[0].load(Ordering::Acquire);
        if k == key {
            return slot[1].fetch_add(1, Ordering::AcqRel) + 1;
        }
        if k == 0
            && slot[0]
                .compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return slot[1].fetch_add(1, Ordering::AcqRel) + 1;
        }
        if slot[0].load(Ordering::Acquire) == key {
            return slot[1].fetch_add(1, Ordering::AcqRel) + 1;
        }
    }
    1
}

/// Emit the `SATW_REFUSED` line of a refused placement: `class` names the obligation kept
/// (`sleep`, `futex`, `recv`), `generation` its cycle, `deadline` / `now` in the class's own unit,
/// `n` this refusal's ordinal within the cycle (only the first [`LOGGED_PER_CYCLE`] are logged;
/// the facts of every refusal are consumed either way).
#[cfg(any(feature = "sched-time-witness", feature = "futex-time-witness"))]
pub(crate) fn emit(class: &str, cpu: u8, tid: u64, generation: u64, deadline: u64, now: u64) {
    let facts = take(tid);
    let n = ordinal(tid, generation);
    if n > LOGGED_PER_CYCLE {
        return;
    }
    match facts {
        Some(r) => crate::yarm_log!(
            "SATW_REFUSED class={} cpu={} home={} tid={} gen={} n={} prio={} queued={} reserved={} cap={} full={} deadline={} now={} facts=ok",
            class,
            cpu,
            r.home,
            tid,
            generation,
            n,
            r.priority,
            r.queued,
            r.reserved,
            crate::kernel::scheduler::MAX_RUN_QUEUE,
            u8::from(r.full()),
            deadline,
            now
        ),
        None => crate::yarm_log!(
            "SATW_REFUSED class={} cpu={} home=0 tid={} gen={} n={} prio=0 queued=0 reserved=0 cap={} full=0 deadline={} now={} facts=lost",
            class,
            cpu,
            tid,
            generation,
            n,
            crate::kernel::scheduler::MAX_RUN_QUEUE,
            deadline,
            now
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Distinct tids per test: the table is process-global and hosted tests share it.
    #[test]
    fn a_recorded_refusal_is_taken_once_with_its_facts() {
        record(1, 900_001, 1, 64, 0, REASON_QUEUE_FULL);
        let r = take(900_001).expect("recorded");
        assert_eq!((r.home, r.priority, r.queued, r.reserved), (1, 1, 64, 0));
        assert!(r.full());
        assert_eq!(take(900_001), None, "consumed");
    }

    #[test]
    fn a_later_refusal_on_the_same_cpu_replaces_it() {
        record(2, 900_002, 1, 63, 1, REASON_QUEUE_FULL);
        record(2, 900_003, 1, 64, 0, REASON_QUEUE_FULL);
        assert_eq!(take(900_002), None, "replaced: facts lost, proves nothing");
        assert!(take(900_003).expect("latest").full());
    }

    #[test]
    fn refusals_of_one_cycle_are_numbered_and_another_cycle_starts_at_one() {
        assert_eq!(ordinal(900_010, 7), 1);
        assert_eq!(ordinal(900_010, 7), 2);
        assert_eq!(ordinal(900_010, 8), 1, "a new generation is a new cycle");
        assert_eq!(ordinal(900_011, 7), 1, "another task");
        for want in 3..=LOGGED_PER_CYCLE + 2 {
            assert_eq!(ordinal(900_010, 7), want);
        }
    }

    #[test]
    fn only_a_capacity_refusal_at_the_cap_is_full() {
        record(3, 900_004, 1, 62, 1, REASON_QUEUE_FULL);
        assert!(!take(900_004).unwrap().full(), "room left");
        record(3, 900_005, 1, 64, 0, REASON_OTHER);
        assert!(!take(900_005).unwrap().full(), "not a capacity refusal");
        record(3, 900_006, 1, 63, 1, REASON_QUEUE_FULL);
        assert!(take(900_006).unwrap().full(), "promised slots count");
    }
}
