// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — init's live futex seal cell (selector 18, `futex-seal-witness` with
//! `futex-requeue-witness`; the kernel half is `kernel::seal_witness` on the FUTEX-TIME1 markers).
//!
//! **PB — paired broadcast.** `WORKERS` workers wait for one predicate under one mutex, round
//! after round. Before each round the main thread confirms all of them are parked on the
//! condition's sequence word (an order-preserving observation), takes the mutex, advances the
//! predicate, broadcasts and unlocks. Only the broadcast strategy differs between the two arms:
//!
//! * `rq` — the production `FutexCondvar::broadcast_with` (NR 17: wake one, move the rest to the
//!   mutex word, `CONTENDED` stored first);
//! * `wa` — a witness-only wake-all control: the same Release increment of the sequence, then
//!   NR 10 on the sequence for every waiter (repeated only while it reports fewer than the cohort).
//!
//! Every futex call of the workers and of the broadcaster goes through [`Counting`], a `FutexOps`
//! over the real system calls that counts, per thread and round, waits that actually PARKED (NR 9
//! returned `Woken`) and waits that did not (`ValueChanged`), on the sequence and on the mutex word
//! separately, and the NR 10 / NR 17 calls and what they reported. Nothing here counts dispatches
//! or context switches.
//!
//! Lines: FSL_BEGIN, FSL_THREADS, FSL_PARKED, FSL_BCAST, FSL_DONE (per worker and round),
//! FSL_ROUND, FSL_FAIL, FSL_END.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::SeqCst};
use yarm_user_rt::futex_sync::{FutexCondvar, FutexMutex, FutexOps, Syscalls};
use yarm_user_rt::syscall::{
    CLOCK_MONOTONIC, FutexRequeueOutcome, FutexScope, FutexTimedWait, SyscallError, clock_gettime,
    futex_cmp_requeue, futex_wait, futex_wake, yield_now,
};

const P: FutexScope = FutexScope::Private;
pub(super) const STACK_BYTES: usize = 16384;
pub(super) const WORKERS: usize = 10;
/// Paired rounds: pair `p` runs `rq` then `wa` when `p` is even, `wa` then `rq` when odd.
const PAIRS: u32 = 4;
const ROUNDS: u32 = 2 * PAIRS;
const BOUND: u32 = 4_000_000;
/// The broadcaster's counter slot (after the workers').
const HB: usize = WORKERS;

#[repr(C, align(16))]
pub(super) struct Stacks(pub(super) [[u8; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut STACKS: Stacks = Stacks([[0; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut TLS: [[u8; 256]; WORKERS] = [[0; 256]; WORKERS];

static M: FutexMutex = FutexMutex::new(P);
static CV: FutexCondvar = FutexCondvar::new(P);
/// The predicate: the round the workers wait for, written only with `M` held.
static PRED: AtomicU32 = AtomicU32::new(0);
/// The confirmation's scratch word (never waited on directly).
static SCRATCH: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
/// Completions in the current round.
static DONE: AtomicU32 = AtomicU32::new(0);
/// Per worker: the last round it completed.
static COMPLETED: [AtomicU32; WORKERS] = [const { AtomicU32::new(0) }; WORKERS];
static FAILS: AtomicU32 = AtomicU32::new(0);
static PARK: AtomicU32 = AtomicU32::new(0);
pub(super) static ENTRY: AtomicUsize = AtomicUsize::new(0);

// Per-thread counters (workers 0..WORKERS, then the broadcaster), reset by their owner per round.
const SLOTS: usize = WORKERS + 1;
static SEQ_PARKED: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static SEQ_VC: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static MTX_PARKED: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static MTX_VC: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static WAKE_CALLS: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static WAKE_WOKEN: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static RQ_CALLS: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];

fn reset(slot: usize) {
    for c in [
        &SEQ_PARKED,
        &SEQ_VC,
        &MTX_PARKED,
        &MTX_VC,
        &WAKE_CALLS,
        &WAKE_WOKEN,
        &RQ_CALLS,
    ] {
        c[slot].store(0, SeqCst);
    }
}

/// The real system calls, counted for one thread's slot.
struct Counting {
    slot: usize,
}

impl FutexOps for Counting {
    fn wait(
        &self,
        word: &AtomicU32,
        expected: u32,
        scope: FutexScope,
        deadline_ns: Option<u64>,
    ) -> Result<FutexTimedWait, SyscallError> {
        let r = Syscalls.wait(word, expected, scope, deadline_ns);
        let on_seq = core::ptr::eq(word, CV.seq());
        let (parked, vc) = if on_seq {
            (&SEQ_PARKED, &SEQ_VC)
        } else {
            (&MTX_PARKED, &MTX_VC)
        };
        match r {
            Ok(FutexTimedWait::Woken) => {
                parked[self.slot].fetch_add(1, SeqCst);
            }
            Ok(FutexTimedWait::ValueChanged) => {
                vc[self.slot].fetch_add(1, SeqCst);
            }
            _ => {}
        }
        r
    }
    fn wake(&self, word: &AtomicU32, count: u32, scope: FutexScope) -> Result<u32, SyscallError> {
        let r = Syscalls.wake(word, count, scope);
        WAKE_CALLS[self.slot].fetch_add(1, SeqCst);
        if let Ok(n) = r {
            WAKE_WOKEN[self.slot].fetch_add(n, SeqCst);
        }
        r
    }
    fn cmp_requeue(
        &self,
        source: &AtomicU32,
        source_scope: FutexScope,
        target: &AtomicU32,
        target_scope: FutexScope,
        expected: u32,
        wake: u32,
        requeue: u32,
    ) -> Result<FutexRequeueOutcome, SyscallError> {
        RQ_CALLS[self.slot].fetch_add(1, SeqCst);
        Syscalls.cmp_requeue(
            source,
            source_scope,
            target,
            target_scope,
            expected,
            wake,
            requeue,
        )
    }
    fn monotonic_now(&self) -> Result<u64, SyscallError> {
        Syscalls.monotonic_now()
    }
}

fn mono() -> u64 {
    clock_gettime(CLOCK_MONOTONIC)
        .ok()
        .and_then(|t| t.as_ns())
        .unwrap_or(0)
}

fn fail(what: &str) {
    FAILS.fetch_add(1, SeqCst);
    yarm_user_rt::user_log!("FSL_FAIL what={}", what);
}

fn park() -> ! {
    loop {
        let v = PARK.load(SeqCst);
        let _ = futex_wait(PARK.as_ptr(), v, P);
    }
}

/// Block (not poll) until `word` reaches at least `n`.
fn await_at_least(word: &AtomicU32, n: u32) -> bool {
    let mut k = 0;
    loop {
        let v = word.load(SeqCst);
        if v >= n {
            return true;
        }
        if k >= BOUND {
            return false;
        }
        let _ = futex_wait(word.as_ptr(), v, P);
        k += 1;
    }
}

fn bump(word: &AtomicU32) {
    word.fetch_add(1, SeqCst);
    let _ = futex_wake(word.as_ptr(), u32::MAX, P);
}

/// Confirm that `n` waiters are parked on `word`: move them to SCRATCH as they arrive and, once
/// all `n` are there, back in one call. Later arrivals are moved later, behind earlier ones, so the
/// order on `word` afterwards is the order they parked (the FUTEX-SYNC-TIME1 form).
fn confirm_parked(word: &AtomicU32, n: u32) -> bool {
    let mut have = 0;
    for _ in 0..BOUND {
        let v = word.load(SeqCst);
        if let Ok(o) = futex_cmp_requeue(word.as_ptr(), P, SCRATCH.as_ptr(), P, v, 0, n - have) {
            have += o.requeued;
            if have == n {
                let back = futex_cmp_requeue(SCRATCH.as_ptr(), P, word.as_ptr(), P, 0, 0, n);
                return matches!(back, Ok(b) if b.requeued == n);
            }
        }
        let _ = yield_now();
    }
    let _ = futex_cmp_requeue(SCRATCH.as_ptr(), P, word.as_ptr(), P, 0, 0, n);
    false
}

fn arm_of(r: u32) -> &'static str {
    let pair = (r - 1) / 2;
    let first = (r - 1) % 2 == 0;
    if (pair % 2 == 0) == first { "rq" } else { "wa" }
}

/// The witness-only wake-all control: the production broadcast's sequence update, then NR 10 on
/// the sequence until the whole cohort was reported woken (or a wake reports nobody).
fn wake_all(ops: &Counting, cohort: u32) {
    CV.seq().fetch_add(1, core::sync::atomic::Ordering::Release);
    let mut woken = 0;
    for _ in 0..8 {
        let n = ops.wake(CV.seq(), u32::MAX, P).unwrap_or(0);
        woken += n;
        if n == 0 || woken >= cohort {
            break;
        }
    }
}

fn worker(k: u32) {
    let ops = Counting { slot: k as usize };
    for r in 1..=ROUNDS {
        reset(k as usize);
        M.lock_with(&ops);
        while PRED.load(SeqCst) < r {
            CV.wait_with(&ops, &M);
        }
        let seen = PRED.load(SeqCst);
        if seen != r {
            fail("predicate");
        }
        let prev = COMPLETED[k as usize].swap(r, SeqCst);
        if prev + 1 != r {
            fail("completion_order");
        }
        let held = u32::from(M.word().load(SeqCst) != 0);
        let s = k as usize;
        yarm_user_rt::user_log!(
            "FSL_DONE round={} arm={} worker={} seen={} held={} seq_parked={} seq_vc={} mtx_parked={} mtx_vc={}",
            r,
            arm_of(r),
            k,
            seen,
            held,
            SEQ_PARKED[s].load(SeqCst),
            SEQ_VC[s].load(SeqCst),
            MTX_PARKED[s].load(SeqCst),
            MTX_VC[s].load(SeqCst)
        );
        let before = WAKE_CALLS[s].load(SeqCst);
        // The unlock's wake call (if the word was CONTENDED) and what that call reported woken:
        // `0` may also be a capacity refusal, not only an empty queue.
        let woke = M.unlock_with(&ops);
        let unlock_wakes = WAKE_CALLS[s].load(SeqCst) - before;
        yarm_user_rt::user_log!(
            "FSL_UNLOCK round={} worker={} wakes={} woke={}",
            r,
            k,
            unlock_wakes,
            woke
        );
        bump(&DONE);
    }
}

pub(super) extern "C" fn child_body() -> ! {
    let k = STARTED.fetch_add(1, SeqCst);
    let _ = futex_wake(STARTED.as_ptr(), u32::MAX, P);
    worker(k);
    park();
}

pub(super) fn run_once(s_tid: u64, entry: usize) {
    yarm_user_rt::user_log!(
        "FSL_BEGIN s_tid={} mutex=0x{:x} seq=0x{:x} workers={} rounds={}",
        s_tid,
        M.word().as_ptr() as usize,
        CV.seq().as_ptr() as usize,
        WORKERS,
        ROUNDS
    );
    let stacks = core::ptr::addr_of_mut!(STACKS) as usize;
    let tls = core::ptr::addr_of_mut!(TLS) as usize;
    let mut tids = [0u64; WORKERS];
    for (i, tid) in tids.iter_mut().enumerate() {
        let top = (stacks + (i + 1) * STACK_BYTES) & !0xF;
        // SAFETY: a valid entry; the static stack and TLS outlive the thread.
        *tid =
            unsafe { yarm_user_rt::syscall::spawn_thread(tls + i * 256, top, entry) }.unwrap_or(0);
        await_at_least(&STARTED, i as u32 + 1);
        yarm_user_rt::user_log!("FSL_THREADS worker={} tid={}", i, *tid);
    }
    let ops = Counting { slot: HB };
    let cohort = WORKERS as u32;
    for r in 1..=ROUNDS {
        let arm = arm_of(r);
        DONE.store(0, SeqCst);
        let parked = confirm_parked(CV.seq(), cohort);
        yarm_user_rt::user_log!(
            "FSL_PARKED round={} arm={} n={} ok={}",
            r,
            arm,
            cohort,
            u32::from(parked)
        );
        if !parked {
            fail("parked");
        }
        reset(HB);
        let t0 = mono();
        M.lock_with(&ops);
        PRED.store(r, SeqCst);
        let (woken, requeued, calls, fallback) = if arm == "rq" {
            let rep = CV.broadcast_with(&ops, &M);
            (
                rep.woken,
                rep.requeued,
                rep.calls,
                u32::from(rep.fell_back_to_wake_all),
            )
        } else {
            wake_all(&ops, cohort);
            (
                WAKE_WOKEN[HB].load(SeqCst),
                0,
                WAKE_CALLS[HB].load(SeqCst),
                0,
            )
        };
        let rq_calls = RQ_CALLS[HB].load(SeqCst);
        let bcast_wake_calls = WAKE_CALLS[HB].load(SeqCst);
        let before = WAKE_CALLS[HB].load(SeqCst);
        let unlock_woke = M.unlock_with(&ops);
        let unlock_wakes = WAKE_CALLS[HB].load(SeqCst) - before;
        let all = await_at_least(&DONE, cohort);
        let t1 = mono();
        yarm_user_rt::user_log!(
            "FSL_BCAST round={} arm={} woken={} requeued={} calls={} fallback={} rq_calls={} wake_calls={} unlock_wakes={} unlock_woke={} elapsed_ns={}",
            r,
            arm,
            woken,
            requeued,
            calls,
            fallback,
            rq_calls,
            bcast_wake_calls,
            unlock_wakes,
            unlock_woke,
            t1.saturating_sub(t0)
        );
        yarm_user_rt::user_log!(
            "FSL_ROUND round={} arm={} done={} all={}",
            r,
            arm,
            DONE.load(SeqCst),
            u32::from(all)
        );
        if !all {
            fail("round_incomplete");
        }
    }
    let fails = FAILS.load(SeqCst);
    let ok = tids.iter().all(|&t| t != 0) && fails == 0;
    yarm_user_rt::user_log!(
        "FSL_END fails={} result={}",
        fails,
        if ok { "ok" } else { "fail" }
    );
}
