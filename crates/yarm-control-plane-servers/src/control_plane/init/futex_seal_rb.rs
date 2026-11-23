// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — the RT-budget cell (`futex-seal-rb`, with `futex-seal-witness`; meaningful on a
//! boot with `yarm.sched_profile=rt yarm.sched_budget=on`).
//!
//! W, holding the mutex, makes ONE timed condition wait whose sequence word holds
//! `kernel::seal_witness::BUDGET_MAGIC` in its high half: the kernel arms exactly that wait
//! instance at registration and, in NR 9's park tail, drains W's budget once (an INDUCED
//! exhaustion through the production charge). The main thread confirms W is parked, takes the
//! mutex and holds it across W's deadline: the collector settles the timeout, the production
//! commit parks W Ready but out of budget, W's release rejoins it, and its runtime reacquisition
//! then parks it on the mutex word. Only once W is observed there (a time-bounded observation:
//! the release can be a whole period away) does the main thread unlock; W returns `TimedOut`
//! holding the mutex. Finally a wait on a word holding `DUMP_MAGIC` prints the kernel record.
//!
//! With budgets off (GP) the drain finds no context: the same wait times out as an ordinary one.
//!
//! Lines: FSB_BEGIN, FSB_THREAD, FSB_OP, FSB_PARKED, FSB_HOLD, FSB_MUTEX_PARKED, FSB_UNLOCK,
//! FSB_RET, FSB_DUMP, FSB_FAIL, FSB_END; the kernel's FSEAL_K_BUDGET.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use yarm_user_rt::futex_sync::{FutexCondvar, FutexMutex, FutexOps, Syscalls, WaitOutcome};
use yarm_user_rt::syscall::{
    CLOCK_MONOTONIC, FUTEX_BITSET_MATCH_ANY, FutexRequeueOutcome, FutexScope, FutexTimedWait,
    SyscallError, clock_gettime, futex_cmp_requeue, futex_wait, futex_wait_until, futex_wake,
    yield_now,
};

const P: FutexScope = FutexScope::Private;
pub(super) const STACK_BYTES: usize = 16384;
pub(super) const WORKERS: usize = 1;
/// `kernel::seal_witness::BUDGET_MAGIC` / `DUMP_MAGIC` (high halves).
const BUDGET_MAGIC: u32 = 0x5EB0_0000;
const DUMP_MAGIC: u32 = 0x5ED0_0000;
/// W's deadline lead: room for the main thread to confirm the wait and take the mutex.
const LEAD_NS: u64 = 1_500_000_000;
/// How long the main thread waits for W to reach the mutex word (a budget period can be long).
const REJOIN_BOUND_NS: u64 = 400_000_000_000;
const BOUND: u32 = 4_000_000;

#[repr(C, align(16))]
pub(super) struct Stacks(pub(super) [[u8; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut STACKS: Stacks = Stacks([[0; STACK_BYTES]; WORKERS]);
#[allow(unused)]
pub(super) static mut TLS: [[u8; 256]; WORKERS] = [[0; 256]; WORKERS];

static M: FutexMutex = FutexMutex::new(P);
static CV: FutexCondvar = FutexCondvar::new(P);
static DUMPW: AtomicU32 = AtomicU32::new(DUMP_MAGIC | 1);
static SCRATCH: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicU32 = AtomicU32::new(0);
static READY: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicU32 = AtomicU32::new(0);
static RESUME: AtomicU64 = AtomicU64::new(0);
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

/// The real system calls; records the first clock reading after W's sequence wait returns.
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
        if core::ptr::eq(word, CV.seq()) {
            let _ = RESUME.compare_exchange(0, mono(), SeqCst, SeqCst);
        }
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
    yarm_user_rt::user_log!("FSB_FAIL what={}", what);
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

/// The order-preserving parked-waiter observation, bounded by time instead of by attempts.
fn confirm_parked_within(word: &AtomicU32, n: u32, max_ns: u64) -> bool {
    let until = mono().saturating_add(max_ns);
    let mut have = 0;
    loop {
        let v = word.load(SeqCst);
        if let Ok(o) = futex_cmp_requeue(word.as_ptr(), P, SCRATCH.as_ptr(), P, v, 0, n - have) {
            have += o.requeued;
            if have == n {
                let back = futex_cmp_requeue(SCRATCH.as_ptr(), P, word.as_ptr(), P, 0, 0, n);
                return matches!(back, Ok(b) if b.requeued == n);
            }
        }
        if mono() >= until {
            let _ = futex_cmp_requeue(SCRATCH.as_ptr(), P, word.as_ptr(), P, 0, 0, n);
            return false;
        }
        let _ = yield_now();
    }
}

fn w_body() {
    M.lock();
    let now = mono();
    let d = now + LEAD_NS;
    yarm_user_rt::user_log!(
        "FSB_OP role=W deadline={} now={} seq=0x{:x}",
        d,
        now,
        CV.seq().load(SeqCst)
    );
    bump(&READY);
    let out = CV.wait_until_with(&Timing, &M, d);
    let held = M.word().load(SeqCst) != 0;
    let mine = OWNER.compare_exchange(0, 1, SeqCst, SeqCst).is_ok();
    if mine {
        OWNER.store(0, SeqCst);
    }
    let slug = match out {
        WaitOutcome::Notified => "notified",
        WaitOutcome::TimedOut => "timed_out",
        WaitOutcome::Failed(_) => "failed",
    };
    yarm_user_rt::user_log!(
        "FSB_RET role=W result={} held={} own={} resume={} deadline={} now={}",
        slug,
        u32::from(held),
        u32::from(mine),
        RESUME.load(SeqCst),
        d,
        mono()
    );
    if !held || !mine {
        fail("ownership");
    }
    M.unlock();
    bump(&DONE);
}

pub(super) extern "C" fn child_body() -> ! {
    let _ = STARTED.fetch_add(1, SeqCst);
    let _ = futex_wake(STARTED.as_ptr(), u32::MAX, P);
    w_body();
    park();
}

pub(super) fn run_once(s_tid: u64, entry: usize) {
    // The marked instance's expected value: BUDGET_MAGIC in the high half.
    CV.seq().store(BUDGET_MAGIC | 1, SeqCst);
    yarm_user_rt::user_log!(
        "FSB_BEGIN s_tid={} mutex=0x{:x} seq=0x{:x}",
        s_tid,
        M.word().as_ptr() as usize,
        CV.seq().as_ptr() as usize
    );
    let stacks = core::ptr::addr_of_mut!(STACKS) as usize;
    let tls = core::ptr::addr_of_mut!(TLS) as usize;
    let top = (stacks + STACK_BYTES) & !0xF;
    // SAFETY: a valid entry; the static stack and TLS outlive the thread.
    let tid = unsafe { yarm_user_rt::syscall::spawn_thread(tls, top, entry) }.unwrap_or(0);
    await_at_least(&STARTED, 1);
    yarm_user_rt::user_log!("FSB_THREAD role=W tid={}", tid);
    await_at_least(&READY, 1);
    let parked = confirm_parked_within(CV.seq(), 1, 30_000_000_000);
    yarm_user_rt::user_log!("FSB_PARKED word=seq ok={}", u32::from(parked));
    if !parked {
        fail("parked");
    }
    // Hold the mutex across W's deadline, settlement, budget park and rejoin.
    M.lock();
    yarm_user_rt::user_log!("FSB_HOLD now={}", mono());
    let on_mutex = confirm_parked_within(M.word(), 1, REJOIN_BOUND_NS);
    yarm_user_rt::user_log!("FSB_MUTEX_PARKED ok={} now={}", u32::from(on_mutex), mono());
    if !on_mutex {
        fail("mutex_parked");
    }
    let woke = M.unlock();
    yarm_user_rt::user_log!("FSB_UNLOCK woke={} now={}", woke, mono());
    let done = await_at_least(&DONE, 1);
    if !done {
        fail("done");
    }
    // Print the kernel record (registered, so the kernel's dump runs; it then times out).
    let d = mono() + 50_000_000;
    let r = futex_wait_until(
        DUMPW.as_ptr(),
        DUMP_MAGIC | 1,
        P,
        Some(d),
        FUTEX_BITSET_MATCH_ANY,
    );
    yarm_user_rt::user_log!("FSB_DUMP result={}", u32::from(r.is_ok()));
    let fails = FAILS.load(SeqCst);
    let ok = tid != 0 && fails == 0;
    yarm_user_rt::user_log!(
        "FSB_END fails={} result={}",
        fails,
        if ok { "ok" } else { "fail" }
    );
}
