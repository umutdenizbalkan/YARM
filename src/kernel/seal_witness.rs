// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! FUTEX-SEAL1 — the kernel half of the live futex seal witness (`futex-seal-witness`, default
//! off; it implies `futex-time-witness`, whose markers record every timed wait). Armed only on a
//! boot with `yarm.futex_requeue_witness=1`; init's selector-18 cell built with
//! `futex-seal-witness` drives it.
//!
//! **Budget sequence RB.** A user thread's default scheduling context (`B = T`) can never be
//! exhausted by execution alone, and nothing in the ABI configures a user thread's budget, so a
//! live futex waiter that is out of budget when its timeout settles cannot arise on its own. This
//! module INDUCES that precondition once: init marks one timed wait instance (its expected word
//! value has [`BUDGET_MAGIC`] in the high half) and the registration arms exactly that
//! `(tid, generation)`. When the collector has claimed that instance's timeout, its reserved
//! commit — on the waiter's home CPU, inside the commit's own 1L section, after that section's
//! budget sync has applied every due release and immediately BEFORE the production placement
//! decision — charges the waiter's context its whole remaining budget through the production
//! `sched_budget::charge`, once. Everything after that is production: the commit parks the
//! exhausted Ready waiter (no queue position, its rejoin slot held), the home CPU's tick applies
//! the release at its fixed instant and the waiter rejoins its level's tail, then it takes its
//! completion and returns to ring 3. The induced charge is NOT elapsed time: the context's
//! `charged` statistic includes it.
//!
//! The first form drained at registration (NR 9's park tail). That cannot keep the waiter
//! exhausted until settlement: its releases continue on the period grid while it is blocked, and
//! on RISC-V a 64-tick default period is not reliably longer than the deadline lead (dev run
//! `full1-riscv64-rtb`: a release preceded the settlement, the sample was inconclusive). In the
//! commit section no release can fall between the drain and the placement decision.
//!
//! Why induced: charging is in whole global ticks at the start of each 1L section, and a tick that
//! exhausts a running thread is itself the scheduling point that takes its CPU, so an exhausted
//! thread does not run again to call NR 9. The only natural overlap is the waiter's own NR 9 block
//! section being the first section on its CPU after the clock owner advanced the tick past its
//! budget — a window as wide as the (uncontrolled) phase between two CPUs' timers. An explicit
//! `B < T` does not change that.
//!
//! Every event is recorded in atomics (nothing is printed under a scheduler lock), each with a
//! stamp from one counter, and printed by [`dump`] when init registers a timed wait whose expected
//! value has [`DUMP_MAGIC`] in the high half (inside the futex hold, where the FUTEX-TIME1 markers
//! print too). With budgets off (GP) the drain finds no context and records `enabled=0`: the
//! waiter's timeout then completes as an ordinary one.
//!
//! Clock domains: the futex deadline and its settlement are `CLOCK_MONOTONIC` nanoseconds (the
//! FUTEX-TIME1 markers); the drain, the park and the release are scheduler ticks (`budget.now`).
//! Their order across domains is the stamp order.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};

/// The high half of the one budget-sequence cycle's expected value.
pub const BUDGET_MAGIC: u32 = 0x5EB0_0000;
/// The high half of the expected value of the wait that prints the record.
pub const DUMP_MAGIC: u32 = 0x5ED0_0000;

static STAMP: AtomicU64 = AtomicU64::new(1);
static ARMED_TID: AtomicU64 = AtomicU64::new(0);
static ARMED_GEN: AtomicU64 = AtomicU64::new(0);
static ARMS: AtomicU32 = AtomicU32::new(0);
/// The marked instance's tid until its park tail takes the one drain.
static PENDING: AtomicU64 = AtomicU64::new(0);
static DRAINS: AtomicU32 = AtomicU32::new(0);
static DRAIN_ENABLED: AtomicU32 = AtomicU32::new(0);
static DRAIN_STAMP: AtomicU64 = AtomicU64::new(0);
static DRAIN_TICK: AtomicU64 = AtomicU64::new(0);
static DRAIN_BEFORE: AtomicU64 = AtomicU64::new(0);
static DRAIN_NEXT: AtomicU64 = AtomicU64::new(0);
static COMMITS: AtomicU32 = AtomicU32::new(0);
static COMMIT_PARKED: AtomicU32 = AtomicU32::new(0);
static COMMIT_STAMP: AtomicU64 = AtomicU64::new(0);
static PARKS: AtomicU32 = AtomicU32::new(0);
static PARK_STAMP: AtomicU64 = AtomicU64::new(0);
static PARK_TICK: AtomicU64 = AtomicU64::new(0);
static PARK_NEXT: AtomicU64 = AtomicU64::new(0);
static PARK_CPU: AtomicU32 = AtomicU32::new(u32::MAX);
static REJOIN_CPU: AtomicU32 = AtomicU32::new(u32::MAX);
static RELEASES: AtomicU32 = AtomicU32::new(0);
/// The stamp of the first release applied to the armed context after its drain: a release
/// stamped before the commit means it was replenished before settlement (inconclusive round).
static FIRST_RELEASE_STAMP: AtomicU64 = AtomicU64::new(0);
static REJOINS: AtomicU32 = AtomicU32::new(0);
static REJOIN_STAMP: AtomicU64 = AtomicU64::new(0);
static REJOIN_TICK: AtomicU64 = AtomicU64::new(0);
static REJOIN_RELEASE: AtomicU64 = AtomicU64::new(0);

fn stamp() -> u64 {
    STAMP.fetch_add(1, SeqCst)
}

/// Whether `expected` marks the budget-sequence cycle (and the witness is armed on this boot).
pub fn is_budget(expected: u32) -> bool {
    crate::kernel::boot::futex_requeue_witness_enabled() && expected & 0xFFFF_0000 == BUDGET_MAGIC
}
/// Whether `expected` asks for the record to be printed.
pub fn is_dump(expected: u32) -> bool {
    crate::kernel::boot::futex_requeue_witness_enabled() && expected & 0xFFFF_0000 == DUMP_MAGIC
}

/// rank 2 (the registration hold): the marked cycle `(tid, generation)` was registered.
pub fn arm(tid: u64, generation: u64) {
    ARMED_TID.store(tid, SeqCst);
    ARMED_GEN.store(generation, SeqCst);
    ARMS.fetch_add(1, SeqCst);
    PENDING.store(tid, SeqCst);
}

/// The collector's commit section for `tid`'s claimed, marked instance: whether this is that
/// instance's one drain (consumed: never a second drain, for this or any later wait).
pub fn take_pending(tid: u64) -> bool {
    tid != 0 && PENDING.compare_exchange(tid, 0, SeqCst, SeqCst).is_ok()
}

/// The collector, after it claimed `(tid, generation)`'s timeout: is this the marked cycle?
pub fn armed_cycle(tid: u64, generation: u64) -> bool {
    tid != 0 && ARMED_TID.load(SeqCst) == tid && ARMED_GEN.load(SeqCst) == generation
}

/// The collector's commit section, before the production commit: the drain's result —
/// `(remaining before, tick, next release)`, or `None` with budgets off (no context).
pub fn note_drain(result: Option<(u64, u64, u64)>) {
    DRAINS.fetch_add(1, SeqCst);
    DRAIN_STAMP.store(stamp(), SeqCst);
    match result {
        Some((before, tick, next)) => {
            DRAIN_ENABLED.store(1, SeqCst);
            DRAIN_BEFORE.store(before, SeqCst);
            DRAIN_TICK.store(tick, SeqCst);
            DRAIN_NEXT.store(next, SeqCst);
        }
        None => DRAIN_ENABLED.store(0, SeqCst),
    }
}

/// The collector's reserved commit, after the production commit: whether the context is parked.
pub fn note_commit(parked: bool) {
    COMMITS.fetch_add(1, SeqCst);
    COMMIT_PARKED.store(u32::from(parked), SeqCst);
    COMMIT_STAMP.store(stamp(), SeqCst);
}

/// The scheduler parked `tid` (exhausted, Ready) at tick `now` on `cpu`; its next release is `next`.
pub fn note_parked(now: u64, tid: u64, next: u64, cpu: u8) {
    if tid == 0 || ARMED_TID.load(SeqCst) != tid {
        return;
    }
    PARKS.fetch_add(1, SeqCst);
    PARK_STAMP.store(stamp(), SeqCst);
    PARK_TICK.store(now, SeqCst);
    PARK_NEXT.store(next, SeqCst);
    PARK_CPU.store(u32::from(cpu), SeqCst);
}

/// The scheduler applied `tid`'s release `release` at tick `now`; `rejoin`: it rejoined its tail.
pub fn note_replenished(now: u64, tid: u64, release: u64, rejoin: bool, cpu: u8) {
    if tid == 0 || ARMED_TID.load(SeqCst) != tid || DRAINS.load(SeqCst) == 0 {
        return;
    }
    if RELEASES.fetch_add(1, SeqCst) == 0 {
        FIRST_RELEASE_STAMP.store(stamp(), SeqCst);
    }
    if rejoin {
        REJOINS.fetch_add(1, SeqCst);
        REJOIN_STAMP.store(stamp(), SeqCst);
        REJOIN_TICK.store(now, SeqCst);
        REJOIN_RELEASE.store(release, SeqCst);
        REJOIN_CPU.store(u32::from(cpu), SeqCst);
    }
}

/// Print the record (rank 2: the futex hold of the dump wait, like every FUTEX-TIME1 marker).
pub fn dump() {
    // Two lines: the kernel log truncates a longer one.
    crate::yarm_log!(
        "FSEAL_K_BUDGET1 tid={} gen={} arms={} drains={} enabled={} drain_stamp={} drain_tick={} before={} next_release={} commits={} commit_parked={} commit_stamp={}",
        ARMED_TID.load(SeqCst),
        ARMED_GEN.load(SeqCst),
        ARMS.load(SeqCst),
        DRAINS.load(SeqCst),
        DRAIN_ENABLED.load(SeqCst),
        DRAIN_STAMP.load(SeqCst),
        DRAIN_TICK.load(SeqCst),
        DRAIN_BEFORE.load(SeqCst),
        DRAIN_NEXT.load(SeqCst),
        COMMITS.load(SeqCst),
        COMMIT_PARKED.load(SeqCst),
        COMMIT_STAMP.load(SeqCst)
    );
    crate::yarm_log!(
        "FSEAL_K_BUDGET2 tid={} parks={} park_stamp={} park_tick={} park_next={} park_cpu={} releases={} first_release_stamp={} rejoins={} rejoin_stamp={} rejoin_tick={} rejoin_release={} rejoin_cpu={}",
        ARMED_TID.load(SeqCst),
        PARKS.load(SeqCst),
        PARK_STAMP.load(SeqCst),
        PARK_TICK.load(SeqCst),
        PARK_NEXT.load(SeqCst),
        PARK_CPU.load(SeqCst),
        RELEASES.load(SeqCst),
        FIRST_RELEASE_STAMP.load(SeqCst),
        REJOINS.load(SeqCst),
        REJOIN_STAMP.load(SeqCst),
        REJOIN_TICK.load(SeqCst),
        REJOIN_RELEASE.load(SeqCst),
        REJOIN_CPU.load(SeqCst)
    );
}
