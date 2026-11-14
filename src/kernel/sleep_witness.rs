// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-TIME-FOUNDATION1 C — the LIVE sleep witness's kernel half (`sched-time-witness`,
//! default off). It decides nothing: it reports the sleep lifecycle the production owners run,
//! and it adds two fixtures the ring-3 cell cannot produce by itself.
//!
//! Markers (one line each):
//!
//! * `STW_SLEEP_REGISTER tid gen deadline now` — a `ClockNanosleep` cycle registered (rank 2, in
//!   the registration hold).
//! * `STW_SLEEP_DONE cpu home tid gen deadline now current` — the collector on `cpu` claimed the
//!   cycle (monotonic `now`, read just after the claim, >= `deadline`) holding a reserved slot on
//!   `home`, whose commit follows; `current` is `cpu`'s current task at that moment (0 = idle).
//! * `STW_SLEEP_REFUSED cpu home tid gen deadline now` — a due cycle whose home queue refused
//!   the reservation: nothing changed, the cycle stays owed.
//! * `STW_SLEEP_KILLED tid gen` — fixture 1 (below).
//! * `STW_KTHREAD_BEAT kthread cpu n mono` / `STW_KTHREAD_DONE` — fixture 2.
//!
//! Fixtures:
//!
//! 1. **Exit before the deadline.** Ring 3 has no way to end another thread (no signals, no
//!    group exit), so a sleep requested for longer than [`KILL_THRESHOLD_NS`] marks its task for
//!    termination, and the next collector pass applies the task-domain death transition to it:
//!    the TCB to `Dead` and every futex record naming it retired by
//!    `retire_futex_waiters_for_task_locked` — the production owner `mark_task_dead` uses — in
//!    one task-lock hold. The cycle's deadline must then never complete.
//! 2. **Kernel threads running.** On the first pass that sees a sleep, one kernel thread is
//!    created on that CPU; it yields in a loop (every return is a timeout service point) and
//!    reports a heartbeat until [`KTHREAD_RUN_NS`] of monotonic time has passed.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::kthread::KThreadCtx;
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::runtime::SharedKernel;

/// A sleep longer than this (one hour) is fixture 1's termination target.
pub const KILL_THRESHOLD_NS: u64 = 3_600_000_000_000;
/// How long fixture 2's kernel thread runs.
pub const KTHREAD_RUN_NS: u64 = 40_000_000_000;
const BEAT_NS: u64 = 250_000_000;

static KILL_TARGET: AtomicU64 = AtomicU64::new(0);
static KTHREAD_STARTED: AtomicBool = AtomicBool::new(false);

/// rank 2 (registration hold): report a registered sleep; arm fixture 1 for a long one.
pub fn registered(tid: u64, generation: u64, deadline_ns: u64, now_ns: u64) {
    crate::yarm_log!(
        "STW_SLEEP_REGISTER tid={} gen={} deadline={} now={}",
        tid,
        generation,
        deadline_ns,
        now_ns
    );
    if deadline_ns.saturating_sub(now_ns) > KILL_THRESHOLD_NS {
        KILL_TARGET.store(tid, Ordering::Release);
    }
}

/// No lock held: the start of one collector pass on `cpu`.
pub(crate) fn pass(shared: &SharedKernel, cpu: CpuId) {
    if !KTHREAD_STARTED.load(Ordering::Acquire) && !KTHREAD_STARTED.swap(true, Ordering::AcqRel) {
        match shared.kthreads.create(beat, 0, cpu, TaskPriority::Normal) {
            Ok(id) => crate::yarm_log!("STW_KTHREAD_START kthread={:#x} cpu={}", id.raw(), cpu.0),
            Err(e) => crate::yarm_log!("STW_KTHREAD_START result=fail reason={:?}", e),
        }
    }
    let tid = KILL_TARGET.swap(0, Ordering::AcqRel);
    if tid == 0 {
        return;
    }
    // Only a sleeper that has LEFT its CPU: between its registration and the park's
    // compare-and-clear it is still some CPU's current, and a death there is not the case under
    // test. Re-armed for the next pass until the park has completed.
    let still_current = (0..crate::kernel::scheduler::MAX_CPUS)
        .any(|c| shared.current_tid_split_read(CpuId(c as u8)) == Some(tid));
    if still_current {
        KILL_TARGET.store(tid, Ordering::Release);
        return;
    }
    let killed = shared.with_task_futex_split_mut(|tcbs, table| {
        crate::kernel::boot::futex::sleep_witness_kill_locked(tcbs, table, tid)
    });
    match killed {
        Some(generation) => crate::yarm_log!("STW_SLEEP_KILLED tid={} gen={}", tid, generation),
        None => crate::yarm_log!("STW_SLEEP_KILLED tid={} result=not_sleeping", tid),
    }
}

pub fn done(cpu: u8, home: u8, tid: u64, generation: u64, deadline: u64, now: u64, current: u64) {
    crate::yarm_log!(
        "STW_SLEEP_DONE cpu={} home={} tid={} gen={} deadline={} now={} current={}",
        cpu,
        home,
        tid,
        generation,
        deadline,
        now,
        current
    );
}

pub fn refused(cpu: u8, home: u8, tid: u64, generation: u64, deadline: u64, now: u64) {
    crate::yarm_log!(
        "STW_SLEEP_REFUSED cpu={} home={} tid={} gen={} deadline={} now={}",
        cpu,
        home,
        tid,
        generation,
        deadline,
        now
    );
}

fn beat(ctx: &KThreadCtx, _arg: usize) {
    let kid = ctx.id().raw();
    let cpu = ctx.home().0;
    let start = crate::kernel::timekeeping::monotonic_ns().unwrap_or(0);
    let mut next = start;
    let mut n = 0u64;
    let mut yields = 0u64;
    loop {
        let now = crate::kernel::timekeeping::monotonic_ns().unwrap_or(u64::MAX);
        if now >= next {
            n += 1;
            crate::yarm_log!(
                "STW_KTHREAD_BEAT kthread={:#x} cpu={} n={} mono={} yields={}",
                kid,
                cpu,
                n,
                now,
                yields
            );
            next = now.saturating_add(BEAT_NS);
        }
        if now.saturating_sub(start) >= KTHREAD_RUN_NS || yields >= 1 << 40 {
            break;
        }
        let _ = ctx.yield_now();
        yields += 1;
    }
    crate::yarm_log!(
        "STW_KTHREAD_DONE kthread={:#x} cpu={} beats={}",
        kid,
        cpu,
        n
    );
}
