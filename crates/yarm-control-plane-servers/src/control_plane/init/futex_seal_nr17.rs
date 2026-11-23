// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — the NR 17 cell (`futex-seal-nr17`, with `futex-seal-witness`).
//!
//! Masked timed waits (each cycle has the kernel's FUTEX-TIME1 markers) and NR 17:
//!
//! 1. **Unfiltered wake.** V (mask `0x8`) parks on N1. NR 17 from N1 with `wake = 1,
//!    requeue = 0` wakes it: the requeue's wake portion ignores masks.
//! 2. **Moves keep mask and deadline.** V2 (mask `0x4`, 60 s) and then X (mask `0x2`, a short
//!    deadline) park on N1, are confirmed in that order, and NR 17 moves both to N2 without a wake.
//! 3. **Wake after the move.** NR 17 from N2 with `wake = 1, requeue = 0` wakes the oldest there,
//!    V2, whatever its mask; X stays parked.
//! 4. **Expiry after the move.** X times out ON N2 with the deadline and mask it registered on N1.
//!
//! The grader takes identities, words, masks and deadlines from the kernel's markers
//! (`REGISTER`, `WAKE`, `TIMEOUT`). Lines: FSN_BEGIN, FSN_THREAD, FSN_OP, FSN_PARKED, FSN_RQ,
//! FSN_RET, FSN_STEP, FSN_FAIL, FSN_END.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::SeqCst};
use yarm_user_rt::syscall::{
    CLOCK_MONOTONIC, FutexScope, FutexTimedWait, clock_gettime, futex_cmp_requeue, futex_wait,
    futex_wait_until, futex_wake, yield_now,
};

const P: FutexScope = FutexScope::Private;
pub(super) const STACK_BYTES: usize = 16384;
pub(super) const WORKERS: usize = 3;
const NAMES: [&str; WORKERS] = ["V", "V2", "X"];
const MASKS: [u32; WORKERS] = [0x8, 0x4, 0x2];
const LONG_NS: u64 = 60_000_000_000;
/// X's deadline lead: long enough to be admitted, confirmed and moved before it passes.
const X_NS: u64 = 4_000_000_000;
const BOUND: u32 = 4_000_000;
const VALUE: u32 = 0x05E7;

#[repr(C, align(16))]
pub(super) struct Stacks(pub(super) [[u8; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut STACKS: Stacks = Stacks([[0; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut TLS: [[u8; 256]; WORKERS] = [[0; 256]; WORKERS];

static N1: AtomicU32 = AtomicU32::new(VALUE);
static N2: AtomicU32 = AtomicU32::new(VALUE);
static N3: AtomicU32 = AtomicU32::new(VALUE);
static SCRATCH: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
static TURN: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicU32 = AtomicU32::new(0);
static RETURNED: AtomicU32 = AtomicU32::new(0);
static FAILS: AtomicU32 = AtomicU32::new(0);
static PARK: AtomicU32 = AtomicU32::new(0);
pub(super) static ENTRY: AtomicUsize = AtomicUsize::new(0);

fn mono() -> u64 {
    clock_gettime(CLOCK_MONOTONIC)
        .ok()
        .and_then(|t| t.as_ns())
        .unwrap_or(0)
}

fn fail(what: &str) {
    FAILS.fetch_add(1, SeqCst);
    yarm_user_rt::user_log!("FSN_FAIL what={}", what);
}

fn park() -> ! {
    loop {
        let v = PARK.load(SeqCst);
        let _ = futex_wait(PARK.as_ptr(), v, P);
    }
}

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

/// The order-preserving parked-waiter observation of the FUTEX-SYNC-TIME1 cell.
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

fn worker(k: u32) {
    await_at_least(&TURN, k + 1);
    let mask = MASKS[k as usize];
    let now = mono();
    let deadline = now + if k == 2 { X_NS } else { LONG_NS };
    yarm_user_rt::user_log!(
        "FSN_OP role={} mask=0x{:x} deadline={} now={} word=0x{:x}",
        NAMES[k as usize],
        mask,
        deadline,
        now,
        N1.as_ptr() as usize
    );
    let r = futex_wait_until(N1.as_ptr(), VALUE, P, Some(deadline), mask);
    let slug = match r {
        Ok(FutexTimedWait::Woken) => "woken",
        Ok(FutexTimedWait::TimedOut) => "timed_out",
        Ok(FutexTimedWait::ValueChanged) => "value_changed",
        Err(_) => "error",
    };
    RETURNED.fetch_or(1 << k, SeqCst);
    yarm_user_rt::user_log!(
        "FSN_RET role={} result={} now={}",
        NAMES[k as usize],
        slug,
        mono()
    );
    bump(&DONE);
}

pub(super) extern "C" fn child_body() -> ! {
    let k = STARTED.fetch_add(1, SeqCst);
    let _ = futex_wake(STARTED.as_ptr(), u32::MAX, P);
    worker(k);
    park();
}

/// NR 17 from `src` to `dst` with `wake` and `requeue`, logged with its outcome.
fn rq(label: &str, src: &AtomicU32, dst: &AtomicU32, wake: u32, requeue: u32) -> (u32, u32) {
    let r = futex_cmp_requeue(src.as_ptr(), P, dst.as_ptr(), P, VALUE, wake, requeue);
    let (w, q) = match r {
        Ok(o) => (o.woken, o.requeued),
        Err(_) => (u32::MAX, u32::MAX),
    };
    yarm_user_rt::user_log!(
        "FSN_RQ step={} src=0x{:x} dst=0x{:x} wake={} requeue={} woken={} requeued={}",
        label,
        src.as_ptr() as usize,
        dst.as_ptr() as usize,
        wake,
        requeue,
        w,
        q
    );
    (w, q)
}

fn check(step: &str, done: u32, returned: u32, ok: bool) {
    let d = DONE.load(SeqCst);
    let r = RETURNED.load(SeqCst);
    yarm_user_rt::user_log!(
        "FSN_STEP step={} done={} returned=0x{:x} want_done={} want=0x{:x} ok={}",
        step,
        d,
        r,
        done,
        returned,
        u32::from(ok)
    );
    if !ok || d != done || r != returned {
        fail(step);
    }
}

pub(super) fn run_once(s_tid: u64, entry: usize) {
    yarm_user_rt::user_log!(
        "FSN_BEGIN s_tid={} n1=0x{:x} n2=0x{:x} n3=0x{:x}",
        s_tid,
        N1.as_ptr() as usize,
        N2.as_ptr() as usize,
        N3.as_ptr() as usize
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
        yarm_user_rt::user_log!("FSN_THREAD role={} tid={}", NAMES[i], *tid);
    }
    // 1. NR 17's wake portion ignores V's mask.
    bump(&TURN);
    let ok = confirm_parked(&N1, 1);
    yarm_user_rt::user_log!("FSN_PARKED word=n1 n=1 ok={}", u32::from(ok));
    let (w, q) = rq("1", &N1, &N2, 1, 0);
    let one = await_at_least(&DONE, 1);
    check("1", 1, 0b001, ok && w == 1 && q == 0 && one);
    // 2. V2 then X park on N1, in that order, and are moved to N2 without a wake.
    bump(&TURN);
    let ok1 = confirm_parked(&N1, 1);
    bump(&TURN);
    let ok2 = confirm_parked(&N1, 2);
    yarm_user_rt::user_log!("FSN_PARKED word=n1 n=2 ok={}", u32::from(ok1 && ok2));
    let (w, q) = rq("2", &N1, &N2, 0, 2);
    let moved = confirm_parked(&N2, 2);
    check("2", 1, 0b001, ok1 && ok2 && w == 0 && q == 2 && moved);
    // 3. NR 17's wake on N2 takes the oldest there, V2 (mask 0x4); X stays parked.
    let (w, q) = rq("3", &N2, &N3, 1, 0);
    let two = await_at_least(&DONE, 2);
    let x_parked = confirm_parked(&N2, 1);
    check("3", 2, 0b011, w == 1 && q == 0 && two && x_parked);
    // 4. X expires on N2 (nobody wakes it).
    let three = await_at_least(&DONE, 3);
    check("4", 3, 0b111, three);
    // Nobody is left on any word.
    let mut left = 0;
    for word in [&N1, &N2, &N3] {
        left += futex_wake(word.as_ptr(), u32::MAX, P).unwrap_or(1);
    }
    yarm_user_rt::user_log!("FSN_LEFT woken={}", left);
    if left != 0 {
        fail("left");
    }
    let fails = FAILS.load(SeqCst);
    let ok = tids.iter().all(|&t| t != 0) && fails == 0;
    yarm_user_rt::user_log!(
        "FSN_END fails={} result={}",
        fails,
        if ok { "ok" } else { "fail" }
    );
}
