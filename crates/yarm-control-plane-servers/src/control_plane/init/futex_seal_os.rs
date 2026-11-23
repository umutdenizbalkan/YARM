// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — the selective-wake ORDER cell (`futex-seal-os`, with `futex-seal-witness`).
//!
//! Three workers park, in this verified order, on one word with masked timed waits (60 s
//! deadlines, so each cycle has the kernel's FUTEX-TIME1 markers): A (mask `0x1`), B (`0x2`),
//! C (`0x1`). Each is admitted only after the previous one is confirmed parked, and the
//! confirmation preserves queue order. Then the main thread wakes one waiter with mask `0x2`
//! (B must be selected; A is passed over and consumes no count), then one with mask `0x1` (A,
//! the oldest match), then one more with mask `0x1` (C). After each wake it waits for exactly
//! one completion and confirms the remaining waiters are still parked.
//!
//! The grader takes the selection from the kernel's `FUTEX_TIME_W_WAKE` (the selected tid and
//! its mask) and `FUTEX_TIME_W_BITSET_SKIP` (each waiter passed over) markers, not from the
//! order of the user lines. Lines: FSO_BEGIN, FSO_THREAD, FSO_OP, FSO_PARKED, FSO_WAKE,
//! FSO_RET, FSO_STEP, FSO_FAIL, FSO_END.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::SeqCst};
use yarm_user_rt::syscall::{
    CLOCK_MONOTONIC, FutexScope, FutexTimedWait, clock_gettime, futex_cmp_requeue, futex_wait,
    futex_wait_until, futex_wake, futex_wake_bitset, yield_now,
};

const P: FutexScope = FutexScope::Private;
pub(super) const STACK_BYTES: usize = 16384;
pub(super) const WORKERS: usize = 3;
const NAMES: [&str; WORKERS] = ["A", "B", "C"];
const MASKS: [u32; WORKERS] = [0x1, 0x2, 0x1];
const DEADLINE_NS: u64 = 60_000_000_000;
const BOUND: u32 = 4_000_000;
/// The order word's value (never changed while a wait is in flight).
const VALUE: u32 = 0x05E1;

#[repr(C, align(16))]
pub(super) struct Stacks(pub(super) [[u8; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut STACKS: Stacks = Stacks([[0; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut TLS: [[u8; 256]; WORKERS] = [[0; 256]; WORKERS];

static OW: AtomicU32 = AtomicU32::new(VALUE);
static SCRATCH: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
/// Worker k may issue its wait once TURN > k.
static TURN: AtomicU32 = AtomicU32::new(0);
/// Completions so far, and a bitmask of the workers that returned.
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
    yarm_user_rt::user_log!("FSO_FAIL what={}", what);
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
    let deadline = now + DEADLINE_NS;
    yarm_user_rt::user_log!(
        "FSO_OP role={} mask=0x{:x} deadline={} now={}",
        NAMES[k as usize],
        mask,
        deadline,
        now
    );
    let r = futex_wait_until(OW.as_ptr(), VALUE, P, Some(deadline), mask);
    let slug = match r {
        Ok(FutexTimedWait::Woken) => "woken",
        Ok(FutexTimedWait::TimedOut) => "timed_out",
        Ok(FutexTimedWait::ValueChanged) => "value_changed",
        Err(_) => "error",
    };
    RETURNED.fetch_or(1 << k, SeqCst);
    yarm_user_rt::user_log!(
        "FSO_RET role={} result={} now={}",
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

/// One masked wake of one waiter, then exactly one completion, then the rest still parked.
fn step(n: u32, mask: u32, want: u32, left: u32) {
    let woke = futex_wake_bitset(OW.as_ptr(), 1, P, mask).unwrap_or(u32::MAX);
    yarm_user_rt::user_log!("FSO_WAKE step={} mask=0x{:x} woke={}", n, mask, woke);
    if woke != 1 {
        fail("wake_count");
    }
    let one = await_at_least(&DONE, n);
    let parked = left == 0 || confirm_parked(&OW, left);
    let returned = RETURNED.load(SeqCst);
    yarm_user_rt::user_log!(
        "FSO_STEP step={} done={} returned=0x{:x} want=0x{:x} left={} parked={}",
        n,
        DONE.load(SeqCst),
        returned,
        want,
        left,
        u32::from(parked)
    );
    if !one || DONE.load(SeqCst) != n || returned != want || !parked {
        fail("step");
    }
}

pub(super) fn run_once(s_tid: u64, entry: usize) {
    yarm_user_rt::user_log!(
        "FSO_BEGIN s_tid={} word=0x{:x} workers={}",
        s_tid,
        OW.as_ptr() as usize,
        WORKERS
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
        yarm_user_rt::user_log!("FSO_THREAD role={} tid={}", NAMES[i], *tid);
    }
    // Admit A, then B, then C, each only after the previous ones are confirmed parked.
    for k in 0..WORKERS as u32 {
        bump(&TURN);
        let ok = confirm_parked(&OW, k + 1);
        yarm_user_rt::user_log!("FSO_PARKED n={} ok={}", k + 1, u32::from(ok));
        if !ok {
            fail("parked");
        }
    }
    // B (mask 0x2) first; then A and C (mask 0x1), oldest first.
    step(1, 0x2, 0b010, 2);
    step(2, 0x1, 0b011, 1);
    step(3, 0x1, 0b111, 0);
    let fails = FAILS.load(SeqCst);
    let ok = tids.iter().all(|&t| t != 0) && fails == 0;
    yarm_user_rt::user_log!(
        "FSO_END fails={} result={}",
        fails,
        if ok { "ok" } else { "fail" }
    );
}
