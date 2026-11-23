// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — the late sole-wake HANDOFF cell (`futex-seal-lh`, with `futex-seal-witness`).
//!
//! Each round: the main thread holds the mutex; W calls `lock_until` with a deadline `D` and
//! parks (its cycle has the kernel's FUTEX-TIME1 markers); then F calls the untimed `lock` and
//! parks behind it (both confirmed, in that order). The main thread reads the clock until it
//! reaches `D - eps` (this round's offset) and unlocks: the unlock's one wake goes to W, the
//! oldest waiter, unless the collector timed W out first.
//!
//! W's calls go through [`Timing`], a `FutexOps` over the real system calls that records the
//! first clock reading after a wait returns: W's RESUME instant. A round is a LATE handoff when
//! the kernel ended W's cycle with a WAKE and W resumed at or after `D`: W must then acquire (the
//! unlock left the word free) and F must still complete after W's unlock. Rounds where W resumed
//! before `D` (an early handoff) or where the collector won (`TIMEOUT`) are classified, and also
//! must leave F completing; with `eps = 0` the main thread's own clock reads cross `D`, so the
//! collector should win those. Ownership is checked on every return by an exclusive owner slot.
//!
//! Lines: FSH_BEGIN, FSH_THREAD, FSH_OP, FSH_PARKED, FSH_UNLOCK, FSH_RET, FSH_WUNLOCK, FSH_FRET,
//! FSH_ROUND, FSH_FAIL, FSH_END.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use yarm_user_rt::futex_sync::{FutexMutex, FutexOps, LockOutcome, Syscalls};
use yarm_user_rt::syscall::{
    CLOCK_MONOTONIC, FutexRequeueOutcome, FutexScope, FutexTimedWait, SyscallError, clock_gettime,
    futex_cmp_requeue, futex_wait, futex_wake, yield_now,
};

const P: FutexScope = FutexScope::Private;
pub(super) const STACK_BYTES: usize = 16384;
pub(super) const WORKERS: usize = 2;
const NAMES: [&str; WORKERS] = ["W", "F"];
/// W's deadline lead: room to confirm both waiters parked before `D`.
const LEAD_NS: u64 = 2_000_000_000;
/// The main thread unlocks at `D - EPS[r]`.
const EPS_NS: [u64; 10] = [
    2_000_000, 1_000_000, 500_000, 200_000, 100_000, 50_000, 20_000, 0, 0, 0,
];
const ROUNDS: u32 = EPS_NS.len() as u32;
const BOUND: u32 = 4_000_000;

#[repr(C, align(16))]
pub(super) struct Stacks(pub(super) [[u8; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut STACKS: Stacks = Stacks([[0; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut TLS: [[u8; 256]; WORKERS] = [[0; 256]; WORKERS];

static M: FutexMutex = FutexMutex::new(P);
static SCRATCH: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
static GO_W: AtomicU32 = AtomicU32::new(0);
static GO_F: AtomicU32 = AtomicU32::new(0);
static W_READY: AtomicU32 = AtomicU32::new(0);
static W_DONE: AtomicU32 = AtomicU32::new(0);
static F_DONE: AtomicU32 = AtomicU32::new(0);
static DEADLINE: AtomicU64 = AtomicU64::new(0);
/// W's first clock reading after a wait returned, this round (0: none yet), and its wait count.
static RESUME: AtomicU64 = AtomicU64::new(0);
static W_WAITS: AtomicU32 = AtomicU32::new(0);
static OWNER: AtomicU32 = AtomicU32::new(0);
static FAILS: AtomicU32 = AtomicU32::new(0);
static PARK: AtomicU32 = AtomicU32::new(0);
pub(super) static ENTRY: AtomicUsize = AtomicUsize::new(0);

fn mono() -> u64 {
    clock_gettime(CLOCK_MONOTONIC)
        .ok()
        .and_then(|t| t.as_ns())
        .unwrap_or(0)
}

/// The real system calls; records W's resume instant after each wait returns.
struct Timing;

impl FutexOps for Timing {
    fn wait(
        &self,
        word: &AtomicU32,
        expected: u32,
        scope: FutexScope,
        deadline_ns: Option<u64>,
    ) -> Result<FutexTimedWait, SyscallError> {
        let r = Syscalls.wait(word, expected, scope, deadline_ns);
        W_WAITS.fetch_add(1, SeqCst);
        let _ = RESUME.compare_exchange(0, mono(), SeqCst, SeqCst);
        r
    }
    fn wake(&self, word: &AtomicU32, count: u32, scope: FutexScope) -> Result<u32, SyscallError> {
        Syscalls.wake(word, count, scope)
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

fn fail(what: &str) {
    FAILS.fetch_add(1, SeqCst);
    yarm_user_rt::user_log!("FSH_FAIL what={}", what);
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

/// Exclusive ownership on return: take and release an owner slot while holding the mutex.
fn owns(role: u32) -> u32 {
    let held = M.word().load(SeqCst) != 0;
    let mine = OWNER.compare_exchange(0, role + 1, SeqCst, SeqCst).is_ok();
    if mine {
        OWNER.store(0, SeqCst);
    }
    if !held || !mine {
        fail("ownership");
    }
    u32::from(held && mine)
}

fn w_loop() {
    for r in 1..=ROUNDS {
        await_at_least(&GO_W, r);
        RESUME.store(0, SeqCst);
        W_WAITS.store(0, SeqCst);
        let now = mono();
        let d = now + LEAD_NS;
        DEADLINE.store(d, SeqCst);
        yarm_user_rt::user_log!("FSH_OP round={} role=W deadline={} now={}", r, d, now);
        bump(&W_READY);
        let out = M.lock_until_with(&Timing, d);
        let resume = RESUME.load(SeqCst);
        let waits = W_WAITS.load(SeqCst);
        match out {
            LockOutcome::Acquired => {
                let own = owns(0);
                yarm_user_rt::user_log!(
                    "FSH_RET round={} role=W result=acquired own={} resume={} waits={} deadline={} now={}",
                    r,
                    own,
                    resume,
                    waits,
                    d,
                    mono()
                );
                let woke = M.unlock();
                yarm_user_rt::user_log!("FSH_WUNLOCK round={} woke={}", r, woke);
            }
            LockOutcome::TimedOut => {
                yarm_user_rt::user_log!(
                    "FSH_RET round={} role=W result=timed_out own=0 resume={} waits={} deadline={} now={}",
                    r,
                    resume,
                    waits,
                    d,
                    mono()
                );
            }
            LockOutcome::Failed(_) => {
                fail("w_failed");
                yarm_user_rt::user_log!("FSH_RET round={} role=W result=failed", r);
            }
        }
        bump(&W_DONE);
    }
}

fn f_loop() {
    for r in 1..=ROUNDS {
        await_at_least(&GO_F, r);
        yarm_user_rt::user_log!("FSH_OP round={} role=F", r);
        M.lock();
        let own = owns(1);
        yarm_user_rt::user_log!("FSH_FRET round={} own={} now={}", r, own, mono());
        M.unlock();
        bump(&F_DONE);
    }
}

pub(super) extern "C" fn child_body() -> ! {
    let k = STARTED.fetch_add(1, SeqCst);
    let _ = futex_wake(STARTED.as_ptr(), u32::MAX, P);
    if k == 0 {
        w_loop();
    } else {
        f_loop();
    }
    park();
}

pub(super) fn run_once(s_tid: u64, entry: usize) {
    yarm_user_rt::user_log!(
        "FSH_BEGIN s_tid={} mutex=0x{:x} rounds={} lead={}",
        s_tid,
        M.word().as_ptr() as usize,
        ROUNDS,
        LEAD_NS
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
        yarm_user_rt::user_log!("FSH_THREAD role={} tid={}", NAMES[i], *tid);
    }
    for r in 1..=ROUNDS {
        let eps = EPS_NS[(r - 1) as usize];
        M.lock();
        // The main thread holds the owner slot for its whole hold: a W or F "owning" the mutex
        // while the main thread still holds it fails its ownership check.
        if OWNER.compare_exchange(0, 3, SeqCst, SeqCst).is_err() {
            fail("main_owner");
        }
        bump(&GO_W);
        await_at_least(&W_READY, r);
        let w_parked = confirm_parked(M.word(), 1);
        bump(&GO_F);
        let both = confirm_parked(M.word(), 2);
        yarm_user_rt::user_log!(
            "FSH_PARKED round={} w={} wf={}",
            r,
            u32::from(w_parked),
            u32::from(both)
        );
        if !w_parked || !both {
            fail("parked");
        }
        let target = DEADLINE.load(SeqCst).saturating_sub(eps);
        let mut t = mono();
        while t < target {
            t = mono();
        }
        OWNER.store(0, SeqCst);
        let woke = M.unlock();
        yarm_user_rt::user_log!(
            "FSH_UNLOCK round={} eps={} at={} deadline={} woke={}",
            r,
            eps,
            t,
            DEADLINE.load(SeqCst),
            woke
        );
        let w = await_at_least(&W_DONE, r);
        let f = await_at_least(&F_DONE, r);
        let free = M.word().load(SeqCst) == 0;
        yarm_user_rt::user_log!(
            "FSH_ROUND round={} w_done={} f_done={} free={}",
            r,
            u32::from(w),
            u32::from(f),
            u32::from(free)
        );
        if !w || !f || !free {
            fail("round");
        }
    }
    let fails = FAILS.load(SeqCst);
    let ok = tids.iter().all(|&t| t != 0) && fails == 0;
    yarm_user_rt::user_log!(
        "FSH_END fails={} result={}",
        fails,
        if ok { "ok" } else { "fail" }
    );
}
