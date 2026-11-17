// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! FUTEX-SYNC-TIME1 — hosted tests of `futex_sync`, the production code, over two backends.
//!
//! * [`Sim`] is deterministic and single-threaded. It applies the kernel's NR 9 admission
//!   order exactly — compare the word (`ValueChanged`), then the clock (`Unavailable`), then a
//!   deadline already reached (`TimedOut`, nothing parked) — and otherwise "parks" by running the
//!   next scripted event, which plays the other threads (it may store to the words, advance the
//!   clock) and says how the kernel ended the wait. A `pre` hook runs at the start of a wait,
//!   before the comparison: the interleaving between a reading and the park. A scripted
//!   `TimedOut` before the deadline is refused (the kernel never times out early).
//! * [`Emu`] is a futex emulation for real threads, linearizable like the kernel's one critical
//!   section: compare-and-register and wake/requeue under one lock; a waiter ends either woken or
//!   timed out, never both. A waiter parked longer than [`STRANDED`] panics: a lost handoff fails
//!   the test instead of hanging it.
//!
//! Both backends refuse busy polling: a run of waits that return without parking ([`BUSY`] in a
//! row on one thread, or more than [`SIM_CALL_BUDGET`] calls in one scripted test) panics, so a
//! protocol that spins fails the test instead of exhausting the host.

extern crate std;

use crate::futex_sync::{
    CONTENDED, FutexCondvar, FutexMutex, FutexOps, LockOutcome, WaitOutcome, WaitWhileOutcome,
};
use crate::syscall::{FutexRequeueOutcome, FutexScope, FutexTimedWait, SyscallError};
use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::SeqCst};
use core::time::Duration;
use std::boxed::Box;
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

const P: FutexScope = FutexScope::Private;
const MS: u64 = 1_000_000;
/// More futex calls than any scripted test makes: beyond it the code under test is spinning.
const SIM_CALL_BUDGET: usize = 10_000;
/// Consecutive waits of one thread that returned without parking, beyond which it is spinning.
const BUSY: u32 = 100_000;

// ── Sim: the deterministic backend ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Wait {
        word: usize,
        expected: u32,
        deadline: Option<u64>,
    },
    Wake {
        word: usize,
        count: u32,
    },
    Requeue {
        from: usize,
        to: usize,
        expected: u32,
        wake: u32,
    },
    Now,
}

/// How the kernel ended a parked wait.
enum Park {
    Woken,
    TimedOut,
}

type Event = Box<dyn FnOnce(&Sim) -> Park>;
type Pre = Box<dyn FnOnce(&Sim)>;

struct Sim {
    now: Cell<u64>,
    clock: Cell<bool>,
    calls: RefCell<Vec<Call>>,
    park: RefCell<VecDeque<Event>>,
    pre: RefCell<VecDeque<Pre>>,
    wake_ret: Cell<u32>,
    requeue: RefCell<VecDeque<FutexRequeueOutcome>>,
}

impl Sim {
    fn new(now: u64) -> Self {
        Self {
            now: Cell::new(now),
            clock: Cell::new(true),
            calls: RefCell::new(Vec::new()),
            park: RefCell::new(VecDeque::new()),
            pre: RefCell::new(VecDeque::new()),
            wake_ret: Cell::new(1),
            requeue: RefCell::new(VecDeque::new()),
        }
    }
    fn on_park(&self, e: impl FnOnce(&Sim) -> Park + 'static) {
        self.park.borrow_mut().push_back(Box::new(e));
    }
    fn before_wait(&self, e: impl FnOnce(&Sim) + 'static) {
        self.pre.borrow_mut().push_back(Box::new(e));
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
    /// The deadline of every wait on `word`, in order.
    fn deadlines(&self, word: &AtomicU32) -> Vec<Option<u64>> {
        let a = word.as_ptr() as usize;
        self.calls
            .borrow()
            .iter()
            .filter_map(|c| match *c {
                Call::Wait { word, deadline, .. } if word == a => Some(deadline),
                _ => None,
            })
            .collect()
    }
    fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        self.calls.borrow().iter().filter(|c| f(c)).count()
    }
    fn wakes(&self, word: &AtomicU32) -> usize {
        let a = word.as_ptr() as usize;
        self.count(|c| matches!(*c, Call::Wake { word, .. } if word == a))
    }
    fn all_consumed(&self) -> bool {
        self.park.borrow().is_empty() && self.pre.borrow().is_empty()
    }
}

impl FutexOps for Sim {
    fn wait(
        &self,
        word: &AtomicU32,
        expected: u32,
        _scope: FutexScope,
        deadline: Option<u64>,
    ) -> Result<FutexTimedWait, SyscallError> {
        self.calls.borrow_mut().push(Call::Wait {
            word: word.as_ptr() as usize,
            expected,
            deadline,
        });
        assert!(
            self.calls.borrow().len() < SIM_CALL_BUDGET,
            "busy polling: {SIM_CALL_BUDGET} futex calls in one scripted test"
        );
        let pre = self.pre.borrow_mut().pop_front();
        if let Some(pre) = pre {
            pre(self);
        }
        // The kernel's admission order: the word, then the clock, then the deadline.
        if word.load(SeqCst) != expected {
            return Ok(FutexTimedWait::ValueChanged);
        }
        if let Some(d) = deadline {
            if !self.clock.get() {
                return Err(SyscallError::Unavailable);
            }
            if self.now.get() >= d {
                return Ok(FutexTimedWait::TimedOut);
            }
        }
        let event = self
            .park
            .borrow_mut()
            .pop_front()
            .expect("a wait parked with no scripted event: it would block forever");
        match event(self) {
            Park::Woken => Ok(FutexTimedWait::Woken),
            Park::TimedOut => {
                let d = deadline.expect("an untimed wait cannot time out");
                assert!(self.now.get() >= d, "the kernel never times out early");
                Ok(FutexTimedWait::TimedOut)
            }
        }
    }
    fn wake(&self, word: &AtomicU32, count: u32, _scope: FutexScope) -> Result<u32, SyscallError> {
        self.calls.borrow_mut().push(Call::Wake {
            word: word.as_ptr() as usize,
            count,
        });
        Ok(self.wake_ret.get().min(count))
    }
    fn cmp_requeue(
        &self,
        source: &AtomicU32,
        _ss: FutexScope,
        target: &AtomicU32,
        _ts: FutexScope,
        expected: u32,
        wake: u32,
        _requeue: u32,
    ) -> Result<FutexRequeueOutcome, SyscallError> {
        self.calls.borrow_mut().push(Call::Requeue {
            from: source.as_ptr() as usize,
            to: target.as_ptr() as usize,
            expected,
            wake,
        });
        if source.load(SeqCst) != expected {
            return Err(SyscallError::WouldBlock);
        }
        Ok(self
            .requeue
            .borrow_mut()
            .pop_front()
            .unwrap_or(FutexRequeueOutcome {
                woken: 0,
                requeued: 0,
                capacity_limited: false,
            }))
    }
    fn monotonic_now(&self) -> Result<u64, SyscallError> {
        self.calls.borrow_mut().push(Call::Now);
        if self.clock.get() {
            Ok(self.now.get())
        } else {
            Err(SyscallError::Unavailable)
        }
    }
}

fn mutex() -> &'static FutexMutex {
    Box::leak(Box::new(FutexMutex::new(P)))
}
fn condvar() -> &'static FutexCondvar {
    Box::leak(Box::new(FutexCondvar::new(P)))
}
/// Another owner holds `m` (uncontended), as a fast-path `lock` leaves it.
fn held_by_other(m: &FutexMutex) {
    assert!(m.word().compare_exchange(0, 1, SeqCst, SeqCst).is_ok());
}
/// The other owner's unlock, as `FutexMutex::unlock` does it (the store; the wake is the
/// scripted event's outcome).
fn other_unlocks(m: &FutexMutex) -> u32 {
    m.word().swap(0, SeqCst)
}

// ── Mutex ────────────────────────────────────────────────────────────────────────────────────

/// An available mutex is acquired whatever the deadline, with no system call; the uncontended
/// word is `1`.
#[test]
fn m01_uncontended_acquires_even_with_a_passed_deadline() {
    let sim = Sim::new(1_000);
    let m = mutex();
    assert_eq!(m.lock_until_with(&sim, 0), LockOutcome::Acquired);
    assert_eq!(m.word().load(SeqCst), 1);
    assert!(sim.calls().is_empty(), "the fast path makes no call");
    assert_eq!(m.unlock_with(&sim), 0, "an uncontended unlock wakes nobody");
    assert!(sim.calls().is_empty());
}

/// Contended, deadline already passed: one wait, refused at admission (`TimedOut`, nothing
/// parked), one last attempt, `TimedOut`. Not held; the word stays `2`, so the owner's unlock
/// still wakes.
#[test]
fn m02_contended_with_a_passed_deadline_times_out_at_admission() {
    let sim = Sim::new(1_000);
    let m = mutex();
    held_by_other(m);
    assert_eq!(m.lock_until_with(&sim, 500), LockOutcome::TimedOut);
    assert_eq!(sim.deadlines(m.word()), vec![Some(500)]);
    assert_eq!(m.word().load(SeqCst), CONTENDED, "wake responsibility kept");
    assert_eq!(m.unlock_with(&sim), 1);
    assert_eq!(sim.wakes(m.word()), 1, "the owner's unlock wakes");
    assert!(sim.all_consumed());
}

/// Contended success: the owner unlocks and wakes; the waiter acquires through the slow path
/// and leaves the word CONTENDED (others may be parked behind it).
#[test]
fn m03_contended_success_keeps_the_word_contended() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    sim.on_park(move |_| {
        assert_eq!(
            other_unlocks(m),
            CONTENDED,
            "the waiter marked it contended"
        );
        Park::Woken
    });
    assert_eq!(m.lock_until_with(&sim, 10 * MS), LockOutcome::Acquired);
    assert_eq!(m.word().load(SeqCst), CONTENDED);
    assert_eq!(sim.deadlines(m.word()), vec![Some(10 * MS)]);
    assert_eq!(m.unlock_with(&sim), 1, "and its own unlock wakes the next");
}

/// A genuine timeout: the deadline claims the wait while the owner still holds the mutex.
/// Not held; the owner's lock is intact and its unlock still wakes.
#[test]
fn m04_genuine_timeout_leaves_the_owner_its_lock() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    sim.on_park(|s| {
        s.now.set(10 * MS);
        Park::TimedOut
    });
    assert_eq!(m.lock_until_with(&sim, 10 * MS), LockOutcome::TimedOut);
    assert_eq!(m.word().load(SeqCst), CONTENDED);
    assert_eq!(m.unlock_with(&sim), 1);
}

/// The boundary: the deadline claimed the wait, but the mutex is free when the waiter looks
/// again — it is acquired (the documented rule), never reported as a timeout while free.
#[test]
fn m05_mutex_free_at_the_boundary_is_acquired() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    sim.on_park(move |s| {
        other_unlocks(m);
        s.now.set(10 * MS);
        Park::TimedOut
    });
    assert_eq!(m.lock_until_with(&sim, 10 * MS), LockOutcome::Acquired);
    assert_eq!(m.word().load(SeqCst), CONTENDED);
}

/// No monotonic clock: the timed wait is refused (`Unavailable`), never made untimed. Not
/// held. The relative form fails BEFORE touching the mutex.
#[test]
fn m06_unavailable_clock_fails_without_ownership() {
    let sim = Sim::new(0);
    sim.clock.set(false);
    let m = mutex();
    held_by_other(m);
    assert_eq!(
        m.lock_until_with(&sim, 10 * MS),
        LockOutcome::Failed(SyscallError::Unavailable)
    );
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "the owner's unlock still wakes"
    );
    let free = mutex();
    held_by_other(free);
    assert_eq!(
        free.lock_timeout_with(&sim, Duration::from_millis(5)),
        LockOutcome::Failed(SyscallError::Unavailable)
    );
    assert_eq!(
        free.word().load(SeqCst),
        1,
        "the relative form touched nothing"
    );
}

/// Repeated wakes that do not hand over the mutex (it is retaken first) are retried with the
/// SAME deadline, and the retry stops at that deadline (a clock check after each wake), with
/// no extra wait once it has passed.
#[test]
fn m07_repeated_wakes_reuse_the_original_deadline_and_stop_at_it() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    for i in 1..=3u64 {
        sim.on_park(move |s| {
            // The owner unlocks and wakes, but a barger takes the mutex first.
            other_unlocks(m);
            assert!(m.word().compare_exchange(0, 1, SeqCst, SeqCst).is_ok());
            s.now.set(i * 3 * MS);
            Park::Woken
        });
    }
    assert_eq!(m.lock_until_with(&sim, 8 * MS), LockOutcome::TimedOut);
    assert_eq!(
        sim.deadlines(m.word()),
        vec![Some(8 * MS); 3],
        "every retry passes the original instant"
    );
    assert_eq!(sim.now.get(), 9 * MS);
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "the barger's unlock will wake"
    );
    assert!(sim.all_consumed());
}

/// `ValueChanged` churn (the word moves between the waiter's swap and the kernel's
/// comparison, every time) cannot carry the lock past its deadline: the clock is checked after
/// every non-timeout return.
#[test]
fn m08_value_changed_churn_cannot_carry_the_lock_past_its_deadline() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    for i in 1..=4u64 {
        sim.before_wait(move |s| {
            // Unlock and immediate re-lock by others: the waiter's `2` is gone.
            m.word().store(1, SeqCst);
            s.now.set(i * 2 * MS);
        });
    }
    assert_eq!(m.lock_until_with(&sim, 7 * MS), LockOutcome::TimedOut);
    assert_eq!(sim.deadlines(m.word()), vec![Some(7 * MS); 4]);
    assert!(sim.all_consumed());
}

/// THE HANDOFF CASE: the owner's unlock gives its only wake to this waiter, which resumes
/// after its deadline. It must not abandon a free mutex (that would strand everyone parked
/// behind it): it acquires, in the CONTENDED state, so its own unlock wakes the next.
#[test]
fn m09_sole_wake_after_the_deadline_acquires_instead_of_abandoning() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    sim.on_park(move |s| {
        assert_eq!(other_unlocks(m), CONTENDED);
        s.now.set(50 * MS); // resumed late, long after its deadline
        Park::Woken
    });
    assert_eq!(m.lock_until_with(&sim, 10 * MS), LockOutcome::Acquired);
    assert_eq!(m.word().load(SeqCst), CONTENDED);
    assert_eq!(
        sim.count(|c| *c == Call::Now),
        0,
        "the free mutex is taken before any deadline check"
    );
    assert_eq!(
        m.unlock_with(&sim),
        1,
        "the chain continues: its unlock wakes"
    );
}

/// The handoff case, lost: after the sole wake a barger takes the mutex first, and the deadline
/// has passed. The waiter gives up — but only after a swap that left the word CONTENDED, so the
/// barger's unlock carries the wake on to the remaining waiters.
#[test]
fn m10_sole_wake_lost_to_a_barger_transfers_wake_responsibility() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    sim.on_park(move |s| {
        other_unlocks(m);
        assert!(
            m.word().compare_exchange(0, 1, SeqCst, SeqCst).is_ok(),
            "barger"
        );
        s.now.set(50 * MS);
        Park::Woken
    });
    assert_eq!(m.lock_until_with(&sim, 10 * MS), LockOutcome::TimedOut);
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "the barger now owes the wake"
    );
    assert_eq!(
        m.unlock_with(&sim),
        1,
        "the barger's unlock wakes the next waiter"
    );
}

/// The relative form reads the clock once and passes that one instant to every retry; an
/// overflowing duration is refused before anything is attempted.
#[test]
fn m11_relative_lock_reads_the_clock_once_and_refuses_overflow() {
    let sim = Sim::new(1_000);
    let m = mutex();
    held_by_other(m);
    sim.on_park(|s| {
        s.now.set(2_000);
        Park::Woken
    });
    sim.on_park(move |s| {
        other_unlocks(m);
        s.now.set(3_000);
        Park::Woken
    });
    assert_eq!(
        m.lock_timeout_with(&sim, Duration::from_nanos(5 * MS)),
        LockOutcome::Acquired
    );
    assert_eq!(
        sim.deadlines(m.word()),
        vec![Some(1_000 + 5 * MS); 2],
        "one deadline, computed from the first reading"
    );
    let first_now = sim.calls().iter().position(|c| *c == Call::Now).unwrap();
    let first_wait = sim
        .calls()
        .iter()
        .position(|c| matches!(c, Call::Wait { .. }))
        .unwrap();
    assert!(first_now < first_wait, "read once, before the first wait");
    let o = mutex();
    held_by_other(o);
    let before = sim.calls().len();
    assert_eq!(
        o.lock_timeout_with(&sim, Duration::MAX),
        LockOutcome::Failed(SyscallError::InvalidArgs)
    );
    sim.now.set(u64::MAX - 10);
    assert_eq!(
        o.lock_timeout_with(&sim, Duration::from_nanos(11)),
        LockOutcome::Failed(SyscallError::InvalidArgs),
        "an overflowing sum is refused, never saturated into a wait without end"
    );
    assert_eq!(o.word().load(SeqCst), 1, "nothing was attempted");
    assert!(
        sim.calls()[before..].iter().all(|c| *c == Call::Now),
        "only the clock was read"
    );
}

/// A wait error after the waiter marked the word: not held, the owner's lock intact.
#[test]
fn m12_untimed_and_timed_waiters_interoperate_on_one_word() {
    let sim = Sim::new(0);
    let m = mutex();
    held_by_other(m);
    // A timed waiter times out while an untimed one (modelled by the word staying 2) waits.
    sim.on_park(|s| {
        s.now.set(5 * MS);
        Park::TimedOut
    });
    assert_eq!(m.lock_until_with(&sim, 5 * MS), LockOutcome::TimedOut);
    // The owner's unlock still wakes (the untimed waiter), which then acquires.
    assert_eq!(m.unlock_with(&sim), 1);
    m.lock_contended_with(&sim);
    assert_eq!(m.word().load(SeqCst), CONTENDED);
    assert_eq!(sim.deadlines(m.word()), vec![Some(5 * MS)]);
}

// ── Condition variable: raw wait ─────────────────────────────────────────────────────────────

/// Take `m` as the waiter (fast path).
fn lock(sim: &Sim, m: &FutexMutex) {
    m.lock_with(sim);
    assert_ne!(m.word().load(SeqCst), 0);
}

/// A timeout on the sequence returns `TimedOut` with the mutex held again.
#[test]
fn c01_timeout_returns_with_the_mutex_held() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.on_park(|s| {
        s.now.set(4 * MS);
        Park::TimedOut
    });
    assert_eq!(cv.wait_until_with(&sim, m, 4 * MS), WaitOutcome::TimedOut);
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "held, through the contended path"
    );
    assert_eq!(sim.deadlines(cv.seq()), vec![Some(4 * MS)]);
}

/// An error (no clock) returns `Failed` with the mutex held again — never ambiguous.
#[test]
fn c02_error_returns_with_the_mutex_held() {
    let sim = Sim::new(0);
    sim.clock.set(false);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    assert_eq!(
        cv.wait_until_with(&sim, m, 4 * MS),
        WaitOutcome::Failed(SyscallError::Unavailable)
    );
    assert_ne!(m.word().load(SeqCst), 0, "held");
    // The relative form cannot compute a deadline: it never releases the mutex at all.
    let before = m.word().load(SeqCst);
    assert_eq!(
        cv.wait_timeout_with(&sim, m, Duration::from_millis(1)),
        WaitOutcome::Failed(SyscallError::Unavailable)
    );
    assert_eq!(m.word().load(SeqCst), before, "never released");
    assert_eq!(sim.wakes(m.word()), 0);
}

/// A waiter a broadcast moved onto the mutex times out there while the broadcaster still holds
/// the mutex: it reacquires — with an UNTIMED wait on the mutex, not bounded by the expired
/// deadline — and returns `TimedOut` with the mutex held.
#[test]
fn c03_timeout_after_requeue_reacquires_without_a_deadline() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.on_park(move |s| {
        // The broadcaster takes the mutex, broadcasts (this waiter is moved to the mutex word),
        // and holds it past the deadline: the timeout claims the moved wait.
        assert!(m.word().compare_exchange(0, 1, SeqCst, SeqCst).is_ok());
        cv.seq().fetch_add(1, SeqCst);
        m.word().store(CONTENDED, SeqCst);
        s.now.set(6 * MS);
        Park::TimedOut
    });
    sim.on_park(move |_| {
        // The reacquisition parks on the mutex; the broadcaster finally unlocks.
        assert_eq!(other_unlocks(m), CONTENDED);
        Park::Woken
    });
    assert_eq!(cv.wait_until_with(&sim, m, 6 * MS), WaitOutcome::TimedOut);
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "held, contended for those behind"
    );
    assert_eq!(
        sim.deadlines(m.word()),
        vec![None],
        "the reacquisition is not bounded by the expired deadline"
    );
    assert!(sim.all_consumed());
}

/// A notification between the sequence reading and the park is not lost: the kernel's
/// comparison sees it (`ValueChanged`) and the wait returns `Notified` at once.
#[test]
fn c04_notification_between_observation_and_park_is_not_lost() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.before_wait(move |_| {
        cv.seq().fetch_add(1, SeqCst); // a signal after the unlock, before the park
    });
    assert_eq!(cv.wait_until_with(&sim, m, 9 * MS), WaitOutcome::Notified);
    assert_ne!(m.word().load(SeqCst), 0);
    assert!(sim.all_consumed());
}

// ── Condition variable: predicate wait ───────────────────────────────────────────────────────

/// Predicate false initially; a spurious return (nothing changed) re-waits with the SAME
/// deadline; the real notification makes it true; `Satisfied` with the mutex held. The
/// predicate is only evaluated with the mutex held.
#[test]
fn c05_predicate_false_then_true_across_a_spurious_return() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    let ready: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    lock(&sim, m);
    sim.on_park(|s| {
        s.now.set(MS);
        Park::Woken // spurious
    });
    sim.on_park(move |s| {
        // The notifier: takes the mutex, sets the flag, signals, unlocks.
        assert!(m.word().compare_exchange(0, 1, SeqCst, SeqCst).is_ok());
        ready.store(true, SeqCst);
        cv.seq().fetch_add(1, SeqCst);
        other_unlocks(m);
        s.now.set(2 * MS);
        Park::Woken
    });
    let mut evaluations = 0;
    let out = cv.wait_while_until_with(&sim, m, 9 * MS, || {
        evaluations += 1;
        assert_ne!(m.word().load(SeqCst), 0, "evaluated only under the mutex");
        !ready.load(SeqCst)
    });
    assert_eq!(out, WaitWhileOutcome::Satisfied);
    assert_eq!(
        evaluations, 3,
        "initially, after the spurious return, after the notification"
    );
    assert_eq!(sim.deadlines(cv.seq()), vec![Some(9 * MS); 2]);
    assert_ne!(m.word().load(SeqCst), 0);
}

/// The boundary rule: the notification lands as the deadline claims the wait. The condition,
/// evaluated under the reacquired mutex, holds: `Satisfied` wins over the timeout.
#[test]
fn c06_predicate_satisfied_at_the_timeout_boundary_wins() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    let ready: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    lock(&sim, m);
    sim.on_park(move |s| {
        ready.store(true, SeqCst);
        s.now.set(9 * MS);
        Park::TimedOut
    });
    let out = cv.wait_while_until_with(&sim, m, 9 * MS, || !ready.load(SeqCst));
    assert_eq!(out, WaitWhileOutcome::Satisfied);
    assert_ne!(m.word().load(SeqCst), 0);
}

/// Still false at the timeout: `TimedOut`, the mutex held, the condition evaluated once more
/// under it.
#[test]
fn c07_predicate_still_false_at_the_timeout_returns_timed_out_with_the_mutex() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.on_park(|s| {
        s.now.set(9 * MS);
        Park::TimedOut
    });
    let mut evaluations = 0;
    let out = cv.wait_while_until_with(&sim, m, 9 * MS, || {
        evaluations += 1;
        true
    });
    assert_eq!(out, WaitWhileOutcome::TimedOut);
    assert_eq!(evaluations, 2);
    assert_ne!(m.word().load(SeqCst), 0);
}

/// Sequence churn: every kernel comparison returns `ValueChanged` (unrelated signals). The
/// predicate wait still ends at its ORIGINAL deadline, with the mutex held.
#[test]
fn c08_sequence_churn_cannot_carry_the_predicate_wait_past_its_deadline() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    for i in 1..=5u64 {
        sim.before_wait(move |s| {
            cv.seq().fetch_add(1, SeqCst);
            s.now.set(i * 2 * MS);
        });
    }
    let out = cv.wait_while_until_with(&sim, m, 9 * MS, || true);
    assert_eq!(out, WaitWhileOutcome::TimedOut);
    assert_eq!(sim.deadlines(cv.seq()), vec![Some(9 * MS); 5]);
    assert_ne!(m.word().load(SeqCst), 0);
    assert!(sim.all_consumed());
}

/// The relative predicate wait computes ONE deadline, before the first evaluation, and passes
/// it to every wait; an overflow is refused with the mutex never released.
#[test]
fn c09_relative_predicate_wait_computes_one_deadline() {
    let sim = Sim::new(7_000);
    let (m, cv) = (mutex(), condvar());
    let n: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
    lock(&sim, m);
    for i in 1..=3u64 {
        sim.on_park(move |s| {
            s.now.set(7_000 + i * MS);
            if i == 3 {
                n.store(1, SeqCst);
            }
            Park::Woken
        });
    }
    let out =
        cv.wait_while_timeout_with(&sim, m, Duration::from_millis(50), || n.load(SeqCst) == 0);
    assert_eq!(out, WaitWhileOutcome::Satisfied);
    assert_eq!(sim.deadlines(cv.seq()), vec![Some(7_000 + 50 * MS); 3]);
    assert_eq!(
        sim.calls().iter().position(|c| *c == Call::Now),
        Some(0),
        "the clock is read first"
    );
    let before = m.word().load(SeqCst);
    assert_eq!(
        cv.wait_while_timeout_with(&sim, m, Duration::MAX, || true),
        WaitWhileOutcome::Failed(SyscallError::InvalidArgs)
    );
    assert_eq!(m.word().load(SeqCst), before, "never released");
}

/// A condition that already holds needs no wait (and no clock).
#[test]
fn c10_predicate_satisfied_initially_needs_no_wait() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    let before = m.word().load(SeqCst);
    assert_eq!(
        cv.wait_while_until_with(&sim, m, 0, || false),
        WaitWhileOutcome::Satisfied
    );
    assert!(sim.calls().is_empty());
    assert_eq!(m.word().load(SeqCst), before);
}

/// An error inside the predicate loop is reported (never taken as satisfaction or as a
/// notification), with the mutex held.
#[test]
fn c11_predicate_wait_reports_an_error_with_the_mutex_held() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.on_park(|s| {
        s.clock.set(false); // the clock disappears while parked
        Park::Woken
    });
    assert_eq!(
        cv.wait_while_until_with(&sim, m, 9 * MS, || true),
        WaitWhileOutcome::Failed(SyscallError::Unavailable)
    );
    assert_ne!(m.word().load(SeqCst), 0);
}

// ── Broadcast ────────────────────────────────────────────────────────────────────────────────

/// The production broadcast: the sequence is bumped and the mutex marked CONTENDED before
/// NR 17, which wakes one and requeues the rest; a capacity-limited round is followed by one
/// `wake = 0` round. Never a wake-all, unless the sequence moved (`WouldBlock`).
#[test]
fn b01_broadcast_marks_contended_first_and_continues_after_a_capacity_stop() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    sim.requeue.borrow_mut().push_back(FutexRequeueOutcome {
        woken: 0,
        requeued: 3,
        capacity_limited: true,
    });
    sim.requeue.borrow_mut().push_back(FutexRequeueOutcome {
        woken: 0,
        requeued: 1,
        capacity_limited: false,
    });
    let r = cv.broadcast_with(&sim, m);
    assert_eq!(
        (r.woken, r.requeued, r.calls, r.fell_back_to_wake_all),
        (0, 4, 2, false)
    );
    assert_eq!(
        m.word().load(SeqCst),
        CONTENDED,
        "the unlock that follows must wake"
    );
    let (s, w) = (cv.seq().as_ptr() as usize, m.word().as_ptr() as usize);
    assert_eq!(
        sim.calls(),
        vec![
            Call::Requeue {
                from: s,
                to: w,
                expected: 1,
                wake: 1
            },
            Call::Requeue {
                from: s,
                to: w,
                expected: 1,
                wake: 0
            }
        ]
    );
    assert_eq!(m.unlock_with(&sim), 1);
}

#[test]
fn b02_a_moved_sequence_falls_back_to_waking_every_sequence_waiter() {
    let sim = Sim::new(0);
    let (m, cv) = (mutex(), condvar());
    lock(&sim, m);
    // A signal lands between the bump and NR 17: the comparison fails.
    struct Bump<'a>(&'a Sim, &'a FutexCondvar);
    impl FutexOps for Bump<'_> {
        fn wait(
            &self,
            w: &AtomicU32,
            e: u32,
            s: FutexScope,
            d: Option<u64>,
        ) -> Result<FutexTimedWait, SyscallError> {
            self.0.wait(w, e, s, d)
        }
        fn wake(&self, w: &AtomicU32, c: u32, s: FutexScope) -> Result<u32, SyscallError> {
            self.0.wake(w, c, s)
        }
        #[allow(clippy::too_many_arguments)]
        fn cmp_requeue(
            &self,
            a: &AtomicU32,
            sa: FutexScope,
            b: &AtomicU32,
            sb: FutexScope,
            e: u32,
            w: u32,
            r: u32,
        ) -> Result<FutexRequeueOutcome, SyscallError> {
            self.1.seq().fetch_add(1, SeqCst);
            self.0.cmp_requeue(a, sa, b, sb, e, w, r)
        }
        fn monotonic_now(&self) -> Result<u64, SyscallError> {
            self.0.monotonic_now()
        }
    }
    let r = cv.broadcast_with(&Bump(&sim, cv), m);
    assert!(r.fell_back_to_wake_all);
    assert!(sim.calls().contains(&Call::Wake {
        word: cv.seq().as_ptr() as usize,
        count: u32::MAX
    }));
}

// ── Emu: real threads ────────────────────────────────────────────────────────────────────────

/// How long an emulated waiter may stay parked before the test calls it stranded.
const STRANDED: Duration = Duration::from_secs(20);

std::thread_local! {
    /// Consecutive waits of this thread that returned without parking.
    static UNPARKED: Cell<u32> = const { Cell::new(0) };
}

/// A wait returned without parking: count it, and refuse a spin.
fn unparked() {
    UNPARKED.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        assert!(
            n < BUSY,
            "busy polling: {n} waits in a row returned without parking"
        );
    });
}

struct Waiter {
    id: u64,
    word: usize,
    woken: bool,
}

#[derive(Default)]
struct Table {
    next: u64,
    waiters: Vec<Waiter>,
    timeouts: u64,
}

struct Emu {
    t: std::sync::Mutex<Table>,
    cv: std::sync::Condvar,
    t0: std::time::Instant,
}

impl Emu {
    fn new() -> Self {
        Self {
            t: std::sync::Mutex::new(Table::default()),
            cv: std::sync::Condvar::new(),
            t0: std::time::Instant::now(),
        }
    }
    fn now(&self) -> u64 {
        self.t0.elapsed().as_nanos() as u64
    }
    fn parked_on(&self, word: &AtomicU32) -> usize {
        let a = word.as_ptr() as usize;
        let t = self.t.lock().unwrap();
        t.waiters.iter().filter(|w| w.word == a && !w.woken).count()
    }
    fn timeouts(&self) -> u64 {
        self.t.lock().unwrap().timeouts
    }
    fn wait_for(&self, mut f: impl FnMut(&Emu) -> bool) {
        let start = std::time::Instant::now();
        while !f(self) {
            assert!(
                start.elapsed() < STRANDED,
                "the awaited state never arrived"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl FutexOps for Emu {
    fn wait(
        &self,
        word: &AtomicU32,
        expected: u32,
        _scope: FutexScope,
        deadline: Option<u64>,
    ) -> Result<FutexTimedWait, SyscallError> {
        let mut t = self.t.lock().unwrap();
        if word.load(SeqCst) != expected {
            unparked();
            return Ok(FutexTimedWait::ValueChanged);
        }
        if deadline.is_some_and(|d| self.now() >= d) {
            unparked();
            return Ok(FutexTimedWait::TimedOut);
        }
        UNPARKED.with(|c| c.set(0));
        let id = t.next;
        t.next += 1;
        t.waiters.push(Waiter {
            id,
            word: word.as_ptr() as usize,
            woken: false,
        });
        let start = std::time::Instant::now();
        loop {
            let i = t.waiters.iter().position(|w| w.id == id).unwrap();
            if t.waiters[i].woken {
                t.waiters.remove(i);
                return Ok(FutexTimedWait::Woken);
            }
            if deadline.is_some_and(|d| self.now() >= d) {
                t.waiters.remove(i);
                t.timeouts += 1;
                return Ok(FutexTimedWait::TimedOut);
            }
            assert!(
                start.elapsed() < STRANDED,
                "a waiter was stranded on a futex word"
            );
            t = self.cv.wait_timeout(t, Duration::from_millis(2)).unwrap().0;
        }
    }
    fn wake(&self, word: &AtomicU32, count: u32, _scope: FutexScope) -> Result<u32, SyscallError> {
        let a = word.as_ptr() as usize;
        let mut t = self.t.lock().unwrap();
        let mut n = 0;
        for w in t.waiters.iter_mut().filter(|w| w.word == a && !w.woken) {
            if n == count {
                break;
            }
            w.woken = true;
            n += 1;
        }
        self.cv.notify_all();
        Ok(n)
    }
    fn cmp_requeue(
        &self,
        source: &AtomicU32,
        _ss: FutexScope,
        target: &AtomicU32,
        _ts: FutexScope,
        expected: u32,
        wake: u32,
        requeue: u32,
    ) -> Result<FutexRequeueOutcome, SyscallError> {
        let (a, b) = (source.as_ptr() as usize, target.as_ptr() as usize);
        let mut t = self.t.lock().unwrap();
        if source.load(SeqCst) != expected {
            return Err(SyscallError::WouldBlock);
        }
        let mut woken = 0;
        let mut moved = Vec::new();
        let mut i = 0;
        while i < t.waiters.len() {
            let w = &mut t.waiters[i];
            if w.word == a && !w.woken {
                if woken < wake {
                    w.woken = true;
                    woken += 1;
                } else if (moved.len() as u32) < requeue {
                    // Moved behind the target's existing waiters, in source order.
                    let mut w = t.waiters.remove(i);
                    w.word = b;
                    moved.push(w);
                    continue;
                }
            }
            i += 1;
        }
        let requeued = moved.len() as u32;
        t.waiters.extend(moved);
        self.cv.notify_all();
        Ok(FutexRequeueOutcome {
            woken,
            requeued,
            capacity_limited: false,
        })
    }
    fn monotonic_now(&self) -> Result<u64, SyscallError> {
        Ok(self.now())
    }
}

/// Protected data written only under the mutex (two fields, so a torn or unordered view shows).
struct Shared(core::cell::UnsafeCell<(u64, u64)>);
// SAFETY: accessed only while holding the test's FutexMutex (that is what is under test).
unsafe impl Sync for Shared {}

/// Payload visibility under Acquire/Release, with ordinary and timed lockers mixed on one
/// mutex, and timed lockers that time out and retry interleaved: every critical section sees
/// the previous one's writes, and the final count is exact.
#[test]
fn e01_protected_data_under_mixed_timed_and_ordinary_lockers() {
    let emu = Emu::new();
    let m = FutexMutex::new(P);
    let data = Shared(core::cell::UnsafeCell::new((0, 0)));
    let owners = AtomicU32::new(0);
    let timeouts = AtomicU64::new(0);
    const THREADS: u64 = 6;
    const ROUNDS: u64 = 1500;
    std::thread::scope(|s| {
        for k in 0..THREADS {
            let (emu, m, data, owners, timeouts) = (&emu, &m, &data, &owners, &timeouts);
            s.spawn(move || {
                let mut done = 0;
                while done < ROUNDS {
                    let held = match k % 3 {
                        0 => {
                            m.lock_with(emu);
                            true
                        }
                        // A deadline so short it often expires under contention: retry.
                        1 => match m.lock_until_with(emu, emu.now() + 20_000) {
                            LockOutcome::Acquired => true,
                            LockOutcome::TimedOut => {
                                timeouts.fetch_add(1, SeqCst);
                                false
                            }
                            LockOutcome::Failed(e) => panic!("{e:?}"),
                        },
                        _ => m
                            .lock_timeout_with(emu, Duration::from_secs(30))
                            .is_acquired(),
                    };
                    if !held {
                        continue;
                    }
                    assert_eq!(owners.fetch_add(1, SeqCst), 0, "mutual exclusion");
                    // SAFETY: the mutex is held.
                    unsafe {
                        let d = &mut *data.0.get();
                        assert_eq!(d.0, d.1, "the previous owner's writes are visible");
                        d.0 += 1;
                        d.1 += 1;
                    }
                    owners.fetch_sub(1, SeqCst);
                    m.unlock_with(emu);
                    done += 1;
                }
            });
        }
    });
    // SAFETY: every thread has joined.
    let d = unsafe { *data.0.get() };
    assert_eq!(d, (THREADS * ROUNDS, THREADS * ROUNDS));
    assert_eq!(m.word().load(SeqCst), 0);
    std::println!("e01 timed-lock timeouts: {}", timeouts.load(SeqCst));
}

/// A holder keeps the mutex while timed waiters time out (genuinely, on the kernel side) and
/// untimed waiters stay parked; once it unlocks, every untimed waiter still acquires: the
/// timeouts consumed no wake and stranded nobody.
#[test]
fn e02_timeouts_under_a_held_lock_strand_no_ordinary_waiter() {
    let emu = Emu::new();
    let m = FutexMutex::new(P);
    let acquired = AtomicU32::new(0);
    m.lock_with(&emu);
    std::thread::scope(|s| {
        for _ in 0..3 {
            let (emu, m, acquired) = (&emu, &m, &acquired);
            s.spawn(move || {
                m.lock_with(emu);
                acquired.fetch_add(1, SeqCst);
                m.unlock_with(emu);
            });
        }
        emu.wait_for(|e| e.parked_on(m.word()) == 3);
        let timed: Vec<_> = (0..3)
            .map(|_| {
                let (emu, m) = (&emu, &m);
                s.spawn(move || m.lock_until_with(emu, emu.now() + 50 * MS))
            })
            .collect();
        for h in timed {
            assert_eq!(h.join().unwrap(), LockOutcome::TimedOut);
        }
        assert_eq!(
            emu.timeouts(),
            3,
            "each ended by its deadline, none by a wake"
        );
        assert_eq!(m.word().load(SeqCst), CONTENDED);
        m.unlock_with(&emu);
    });
    assert_eq!(acquired.load(SeqCst), 3, "every ordinary waiter acquired");
    assert_eq!(m.word().load(SeqCst), 0);
}

/// The broadcast with mixed waiters: untimed, timed with a far deadline, and one timed waiter X
/// whose deadline passes after it was requeued onto the mutex (the broadcaster holds the mutex
/// until X has timed out there). Every waiter returns holding the mutex (mutual exclusion is
/// checked), X with `TimedOut`, the others `Notified`; the waiters behind X still progress.
#[test]
fn e03_broadcast_with_mixed_waiters_and_a_timeout_after_requeue() {
    let emu = Emu::new();
    let m = FutexMutex::new(P);
    let cv = FutexCondvar::new(P);
    let owners = AtomicU32::new(0);
    let returned = AtomicU32::new(0);
    let in_cs = |owners: &AtomicU32| {
        assert_eq!(
            owners.fetch_add(1, SeqCst),
            0,
            "held on return: mutual exclusion"
        );
        owners.fetch_sub(1, SeqCst);
    };
    std::thread::scope(|s| {
        let mut hs = Vec::new();
        for k in 0..6u32 {
            let (emu, m, cv, owners, returned) = (&emu, &m, &cv, &owners, &returned);
            hs.push(s.spawn(move || {
                m.lock_with(emu);
                let out = if k % 2 == 0 {
                    cv.wait_with(emu, m);
                    WaitOutcome::Notified
                } else {
                    cv.wait_until_with(emu, m, emu.now() + 60_000 * MS)
                };
                in_cs(owners);
                returned.fetch_add(1, SeqCst);
                m.unlock_with(emu);
                out
            }));
            emu.wait_for(|e| e.parked_on(cv.seq()) == k as usize + 1);
        }
        let (emu2, m2, cv2, owners2, returned2) = (&emu, &m, &cv, &owners, &returned);
        let x = s.spawn(move || {
            m2.lock_with(emu2);
            let out = cv2.wait_until_with(emu2, m2, emu2.now() + 300 * MS);
            in_cs(owners2);
            returned2.fetch_add(1, SeqCst);
            m2.unlock_with(emu2);
            out
        });
        emu.wait_for(|e| e.parked_on(cv.seq()) == 7);
        m.lock_with(&emu);
        let r = cv.broadcast_with(&emu, &m);
        assert_eq!(
            (r.woken, r.requeued, r.fell_back_to_wake_all),
            (1, 6, false)
        );
        // Hold the mutex until X's deadline has claimed its wait ON THE MUTEX WORD.
        emu.wait_for(|e| e.timeouts() == 1);
        assert_eq!(returned.load(SeqCst), 0, "nobody returns without the mutex");
        m.unlock_with(&emu);
        assert_eq!(x.join().unwrap(), WaitOutcome::TimedOut);
        for h in hs {
            assert_eq!(h.join().unwrap(), WaitOutcome::Notified);
        }
    });
    assert_eq!(returned.load(SeqCst), 7, "all progressed after the timeout");
    assert_eq!(m.word().load(SeqCst), 0);
}

/// Producer / consumer with the predicate wait and one deadline: spurious signals (no item)
/// never satisfy it; the item does; every wait passes the consumer's one deadline.
#[test]
fn e04_predicate_wait_with_one_deadline_across_spurious_signals() {
    let emu = Emu::new();
    let m = FutexMutex::new(P);
    let cv = FutexCondvar::new(P);
    let items = AtomicU32::new(0);
    struct Rec<'a>(&'a Emu, RefCell<Vec<Option<u64>>>);
    impl FutexOps for Rec<'_> {
        fn wait(
            &self,
            w: &AtomicU32,
            e: u32,
            s: FutexScope,
            d: Option<u64>,
        ) -> Result<FutexTimedWait, SyscallError> {
            if d.is_some() {
                self.1.borrow_mut().push(d);
            }
            self.0.wait(w, e, s, d)
        }
        fn wake(&self, w: &AtomicU32, c: u32, s: FutexScope) -> Result<u32, SyscallError> {
            self.0.wake(w, c, s)
        }
        #[allow(clippy::too_many_arguments)]
        fn cmp_requeue(
            &self,
            a: &AtomicU32,
            sa: FutexScope,
            b: &AtomicU32,
            sb: FutexScope,
            e: u32,
            w: u32,
            r: u32,
        ) -> Result<FutexRequeueOutcome, SyscallError> {
            self.0.cmp_requeue(a, sa, b, sb, e, w, r)
        }
        fn monotonic_now(&self) -> Result<u64, SyscallError> {
            self.0.monotonic_now()
        }
    }
    std::thread::scope(|s| {
        let (emu, m, cv, items) = (&emu, &m, &cv, &items);
        let consumer = s.spawn(move || {
            let rec = Rec(emu, RefCell::new(Vec::new()));
            m.lock_with(&rec);
            let out = cv.wait_while_timeout_with(&rec, m, Duration::from_secs(30), || {
                items.load(SeqCst) == 0
            });
            let got = items.load(SeqCst);
            m.unlock_with(&rec);
            (out, got, rec.1.into_inner())
        });
        for _ in 0..3 {
            emu.wait_for(|e| e.parked_on(cv.seq()) == 1);
            cv.signal_with(emu); // spurious: no item
        }
        emu.wait_for(|e| e.parked_on(cv.seq()) == 1);
        m.lock_with(emu);
        items.store(1, SeqCst);
        cv.signal_with(emu);
        m.unlock_with(emu);
        let (out, got, deadlines) = consumer.join().unwrap();
        assert_eq!((out, got), (WaitWhileOutcome::Satisfied, 1));
        assert!(deadlines.len() >= 4, "{deadlines:?}");
        assert!(
            deadlines.windows(2).all(|w| w[0] == w[1]),
            "one deadline: {deadlines:?}"
        );
    });
}
