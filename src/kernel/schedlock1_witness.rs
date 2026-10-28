// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SCHED-LOCK1 — the QEMU independence and wake witness. Default-off: built only with
//! `sched-lock1-witness` (through `x86-sched-lock1-witness`, `aarch64-sched-lock1-witness` or
//! `riscv64-sched-lock1-witness`), which co-enables the port's production AP-scheduling witness
//! and runs on its workload (timer-preempted spinners on every admitted CPU, sleepers woken by the
//! reschedule IPI, cross-CPU IPC, fork/exit churn). Nothing here changes a scheduling decision.
//!
//! Two kinds of evidence, labelled apart:
//!
//! * **SYNCHRONIZED — the independence holds.** For each ordered pair `(A, B)` of the schedule,
//!   CPU A, inside its own production timer tick and so INSIDE its actual per-CPU scheduler lock
//!   (1L[A]) with its interrupts masked, records `HOLD_BEGIN` and waits — bounded — while CPU B
//!   keeps scheduling. The hold ends when B has made at least two OWNER COMMITS (critical sections
//!   of B's own 1L that changed B's run queue or current slot, recorded at the release) and one
//!   unit of TASK PROGRESS (a later timer tick on B finding current a task one of those commits
//!   installed: kernel entries run masked, so that tick interrupted the task itself, which had
//!   therefore been resumed and run). `HOLD_BEGIN` records the holder's scheduler-lock depth (its
//!   own 1L only, no admission lock), the held instance, and the lock's held flag observed by the
//!   holder; `HOLD_END` records the flag again — so nothing but A's 1L was held, and it was held
//!   throughout. A commit by B on A's lock (B needing A) is recorded as `RELATED` and ends the
//!   attempt unawarded.
//! * **NATURAL — the membership trace.** From a coherent `SNAP` (every CPU's queue and current,
//!   under every 1L) to a coherent `CENSUS`, every 1L critical section that changed a CPU's
//!   membership is recorded as its net change (`SEC` + `ADD`/`REM`), with the executing CPU, the
//!   lock instance and that CPU's acquisition generation; every remote-wake owner records its
//!   `CLAIM` (target, tid, ASID); every wake notification its `NOTIFY`. The grader replays the
//!   trace from SNAP and must arrive exactly at CENSUS: one placement per task at every step, no
//!   wake lost, none consumed twice, every claim matched by the ADD it names, every remote ADD
//!   followed by its notification.
//!
//! **Identity evidence (SCHED-LOCK1-ACCEPTANCE).**
//!
//! * `CELL` — at SNAP and again at CENSUS, under the coherent view, each CPU's cell address and
//!   its lock's address, taken from the domain's own cell array. Every `SEC` and `HOLD_BEGIN`
//!   names the cell of the lock its producer actually held; the grader checks each against this
//!   one map.
//! * `CLAIM` carries the task incarnation the waking owner staged from the authoritative TCB in
//!   its own rank-2 phase ([`stage_incarnation`]), and the generation and record number of the
//!   exact 1L section, on this executing CPU, that added the task. A claim with no staged
//!   incarnation, or no such section, is recorded as unbound and fails the grade.
//! * `RUN` — the first kernel entry (a syscall, a fault or the timer tick) by a task after its
//!   installation on a CPU records the task, the CPU's ACTIVE address space at that entry (the
//!   activation table, independent of every wake owner) and the installing section. Kernel code
//!   runs with interrupts masked except in the idle halt loop, where no task is current, so an
//!   entry with a task current interrupted that task or is its own call. `PROGRESS` carries the
//!   same evidence for the observer during a hold.
//! * `HOLD_BEGIN` and `HOLD_END` record the holder's interrupt-mask state.
//!
//! The record ring is bounded; an overflow is reported and refuses coverage. All output is the
//! synchronous dump after the AP-scheduling witness's own, at P's last step.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::{CpuId, CpuSched, MAX_CPUS};
use crate::runtime::SharedKernel;

#[cfg(not(feature = "apsched-witness"))]
compile_error!(
    "sched-lock1-witness runs on the AP-scheduling witness workload: enable it through x86-/aarch64-/riscv64-sched-lock1-witness"
);

pub const RECS_MAX: usize = 65_536;
/// CPUs the schedule covers (the AP-scheduling witness's mailbox maximum).
pub const MAX_W_CPUS: usize = 8;
/// Attempts per round before it is recorded as failed and the schedule moves on.
const ATTEMPTS: u64 = 3;
/// Bound on one hold, in spin iterations.
const HOLD_SPINS: u64 = 400_000_000;
/// Bound on the open window, in global ticks: past it the window closes with the rounds it has
/// (the grader then refuses coverage for the missing ones). Normally the window closes at the
/// workload's last step ([`close_at_end`]), long before this.
const WINDOW_MAX_TICKS: u64 = 4_000;

// ── record kinds ─────────────────────────────────────────────────────────────────────────────
pub const K_SNAP_BEGIN: u8 = 1;
pub const K_SNAP_CUR: u8 = 2;
pub const K_SNAP_Q: u8 = 3;
pub const K_SEC: u8 = 4;
pub const K_ADD: u8 = 5;
pub const K_REM: u8 = 6;
pub const K_CLAIM: u8 = 7;
pub const K_NOTIFY: u8 = 8;
pub const K_HOLD_BEGIN: u8 = 9;
pub const K_PROGRESS: u8 = 10;
pub const K_HOLD_END: u8 = 11;
pub const K_RELATED: u8 = 12;
pub const K_CENSUS_BEGIN: u8 = 13;
pub const K_CENSUS_CUR: u8 = 14;
pub const K_CENSUS_Q: u8 = 15;
pub const K_CENSUS_END: u8 = 16;
pub const K_ALL_MUTATED: u8 = 17;
pub const K_CELL: u8 = 18;
pub const K_RUN: u8 = 19;
const KINDS: [&str; 20] = [
    "-",
    "SNAP_BEGIN",
    "SNAP_CUR",
    "SNAP_Q",
    "SEC",
    "ADD",
    "REM",
    "CLAIM",
    "NOTIFY",
    "HOLD_BEGIN",
    "PROGRESS",
    "HOLD_END",
    "RELATED",
    "CENSUS_BEGIN",
    "CENSUS_CUR",
    "CENSUS_Q",
    "CENSUS_END",
    "ALL_MUTATED",
    "CELL",
    "RUN",
];

/// Incarnation encodings carried by `CLAIM`, `RUN` and `PROGRESS`: 0 = unbound (no authority),
/// 1 = a task with no address space, `asid + 2` otherwise.
const INC_UNBOUND: u64 = 0;
const INC_NO_ASID: u64 = 1;
fn inc_code(asid: Option<crate::kernel::vm::Asid>) -> u64 {
    asid.map_or(INC_NO_ASID, |a| u64::from(a.0) + 2)
}

struct Rec {
    valid: AtomicBool,
    kind: AtomicU8,
    cpu: AtomicU8,
    exec: AtomicU8,
    f: [AtomicU64; 6],
}

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: Rec = Rec {
    valid: AtomicBool::new(false),
    kind: AtomicU8::new(0),
    cpu: AtomicU8::new(0),
    exec: AtomicU8::new(0),
    f: [const { AtomicU64::new(0) }; 6],
};
static RECS: [Rec; RECS_MAX] = [EMPTY; RECS_MAX];
static NEXT: AtomicUsize = AtomicUsize::new(0);
static OVERFLOW: AtomicBool = AtomicBool::new(false);

/// 0 = not open, 1 = recording, 3 = closing, 2 = closed (CENSUS taken).
static PHASE: AtomicU8 = AtomicU8::new(0);
static NCPU: AtomicU64 = AtomicU64::new(0);
static ROUND: AtomicU64 = AtomicU64::new(0);
static ATTEMPT: AtomicU64 = AtomicU64::new(0);
static ROUNDS_OK: AtomicU64 = AtomicU64::new(0);
/// `round + 1` while a hold is in progress, else 0.
static HOLD_ACTIVE: AtomicU64 = AtomicU64::new(0);
static HOLDER: AtomicU8 = AtomicU8::new(0);
static OBSERVER: AtomicU8 = AtomicU8::new(0);
static B_COMMITS: AtomicU64 = AtomicU64::new(0);
static B_PROGRESS: AtomicU64 = AtomicU64::new(0);
static B_RELATED: AtomicU64 = AtomicU64::new(0);
/// Tasks B's commits installed during the current hold (tid + 1), with the installing generation
/// and the installing section's record number.
static B_INSTALLED: [[AtomicU64; 3]; 8] = [const { [const { AtomicU64::new(0) }; 3] }; 8];
static B_INSTALLED_N: AtomicUsize = AtomicUsize::new(0);
/// Per CPU, from its own last tick: it had a task current AND another queued — local scheduling
/// work a hold's observer can make progress on. A hold starts only when its observer has some.
static BUSY: [AtomicBool; MAX_W_CPUS] = [const { AtomicBool::new(false) }; MAX_W_CPUS];
static OPENED_AT: AtomicU64 = AtomicU64::new(0);

/// Per EXECUTING CPU: the incarnation `(tid + 1, inc_code)` its current wake owner staged in that
/// owner's rank-2 phase, consumed by the owner's claim.
static STAGED: [[AtomicU64; 2]; MAX_CPUS] = [const { [const { AtomicU64::new(0) }; 2] }; MAX_CPUS];
/// Per EXECUTING CPU: the last recorded section in which it ADDED to some CPU's queue —
/// `(target + 1, generation, record number)`. Read by that CPU's own claim right after the commit.
static LAST_ADD: [[AtomicU64; 3]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; 3] }; MAX_CPUS];
/// Per CPU (written and read only under that CPU's own 1L): the last installation recorded on it —
/// `(tid + 1, generation, record number, observed running)`.
static LAST_INSTALL: [[AtomicU64; 4]; MAX_W_CPUS] =
    [const { [const { AtomicU64::new(0) }; 4] }; MAX_W_CPUS];

/// Per-CPU 1L acquisition generation (by the CELL acquired), and per EXECUTING CPU the number of
/// 1L and 1G guards it holds.
static ACQ_GEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static HELD_1L: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static HELD_1G: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// A CPU's membership at its 1L acquisition (used only by the holder of that 1L).
struct Snap {
    current: AtomicU64,
    n: AtomicUsize,
    tids: [AtomicU64; 3 * crate::kernel::scheduler::MAX_RUN_QUEUE],
    prio: [AtomicU8; 3 * crate::kernel::scheduler::MAX_RUN_QUEUE],
    valid: AtomicBool,
}
#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_SNAP: Snap = Snap {
    current: AtomicU64::new(0),
    n: AtomicUsize::new(0),
    tids: [const { AtomicU64::new(0) }; 3 * crate::kernel::scheduler::MAX_RUN_QUEUE],
    prio: [const { AtomicU8::new(0) }; 3 * crate::kernel::scheduler::MAX_RUN_QUEUE],
    valid: AtomicBool::new(false),
};
static SNAPS: [Snap; MAX_W_CPUS] = [EMPTY_SNAP; MAX_W_CPUS];

fn recording() -> bool {
    PHASE.load(Ordering::Acquire) == 1
}

/// The CPU executing this code, from its own hardware identity on each port.
fn exec_cpu() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        crate::arch::x86_64::descriptor_tables::current_cpu_id().0 as usize
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::aarch64::this_cpu_index()
    }
    #[cfg(target_arch = "riscv64")]
    {
        crate::arch::riscv64::boot::riscv_executing_cpu().0 as usize
    }
}

fn reserve(n: usize) -> Option<usize> {
    let base = NEXT.fetch_add(n, Ordering::AcqRel);
    if base + n > RECS_MAX {
        OVERFLOW.store(true, Ordering::Release);
        return None;
    }
    Some(base)
}

fn write(slot: usize, kind: u8, cpu: usize, exec: usize, f: [u64; 6]) {
    let r = &RECS[slot];
    r.kind.store(kind, Ordering::Relaxed);
    r.cpu.store(cpu as u8, Ordering::Relaxed);
    r.exec.store(exec as u8, Ordering::Relaxed);
    for (s, v) in r.f.iter().zip(f) {
        s.store(v, Ordering::Relaxed);
    }
    r.valid.store(true, Ordering::Release);
}

fn record(kind: u8, cpu: usize, f: [u64; 6]) {
    if let Some(s) = reserve(1) {
        write(s, kind, cpu, exec_cpu(), f);
    }
}

/// Whether the executing CPU has interrupts enabled (sampled without changing them).
fn irqs_enabled() -> bool {
    let st = crate::arch::irq_guard::irq_save();
    let on = st.interrupts_were_enabled;
    crate::arch::irq_guard::irq_restore(st);
    on
}

fn enc(t: Option<ThreadId>) -> u64 {
    t.map_or(0, |t| t.0 + 1)
}

fn take_snap(idx: usize, sched: &CpuSched) {
    let Some(s) = SNAPS.get(idx) else { return };
    s.current
        .store(enc(sched.witness_current()), Ordering::Relaxed);
    let mut n = 0;
    sched.witness_for_each_queued(|prio, tid| {
        if n < s.tids.len() {
            s.tids[n].store(tid.0, Ordering::Relaxed);
            s.prio[n].store(prio as u8, Ordering::Relaxed);
            n += 1;
        }
    });
    s.n.store(n, Ordering::Relaxed);
    s.valid.store(true, Ordering::Relaxed);
}

/// Record `RUN` for CPU `c`'s last installation if `cur` is that task and it has not been observed.
/// `src` is 1 for a trap entry, 0 for the tick hook.
fn observe_run(c: usize, cur: u64, src: u64) -> Option<u64> {
    let slot = LAST_INSTALL.get(c)?;
    if !recording()
        || slot[0].load(Ordering::Acquire) != cur + 1
        || slot[3].swap(1, Ordering::AcqRel) != 0
    {
        return None;
    }
    let inc = inc_code(crate::arch::hal::active_address_space(CpuId(c as u8)));
    record(
        K_RUN,
        c,
        [
            cur,
            inc,
            slot[1].load(Ordering::Relaxed),
            ACQ_GEN[c].load(Ordering::Acquire),
            slot[2].load(Ordering::Relaxed),
            src,
        ],
    );
    Some(inc)
}

/// A kernel entry on `cpu` (the shared trap entry, before anything else): if a just-installed
/// task is current there (its published current), this is its first observed execution.
pub(crate) fn trap_entered(shared: &SharedKernel, cpu: CpuId) {
    let c = cpu.0 as usize;
    if c >= MAX_W_CPUS || !recording() {
        return;
    }
    if let Some(cur) = shared.sched_domain().current_mirror(c)
        && cur.0 != 0
    {
        let _ = observe_run(c, cur.0, 1);
    }
}

// ── hooks called by the scheduler domain ─────────────────────────────────────────────────────

/// Before CPU `idx`'s 1L is acquired by the executing CPU.
pub(crate) fn before_acquire(idx: usize) {
    if HOLD_ACTIVE.load(Ordering::Acquire) != 0
        && idx == HOLDER.load(Ordering::Acquire) as usize
        && exec_cpu() == OBSERVER.load(Ordering::Acquire) as usize
    {
        B_RELATED.fetch_add(1, Ordering::AcqRel);
        record(
            K_RELATED,
            idx,
            [HOLD_ACTIVE.load(Ordering::Acquire) - 1, 0, 0, 0, 0, 0],
        );
    }
}

/// CPU `idx`'s 1L was acquired (`sched` is its state, held).
pub(crate) fn after_acquire(idx: usize, sched: &CpuSched) {
    ACQ_GEN[idx].fetch_add(1, Ordering::AcqRel);
    HELD_1L[exec_cpu()].fetch_add(1, Ordering::AcqRel);
    if recording() {
        take_snap(idx, sched);
    } else if let Some(s) = SNAPS.get(idx) {
        s.valid.store(false, Ordering::Relaxed);
    }
}

/// CPU `idx`'s 1L is about to be released; `sched` is its final state. Records the section's net
/// membership change, if any.
pub(crate) fn before_release(idx: usize, instance: usize, sched: &CpuSched) {
    let exec = exec_cpu();
    HELD_1L[exec].fetch_sub(1, Ordering::AcqRel);
    let Some(snap) = SNAPS.get(idx) else { return };
    if !recording() || !snap.valid.load(Ordering::Relaxed) {
        return;
    }
    snap.valid.store(false, Ordering::Relaxed);
    let before_cur = snap.current.load(Ordering::Relaxed);
    let after_cur = enc(sched.witness_current());
    let n = snap.n.load(Ordering::Relaxed);
    let before: [(u64, u8); 192] = core::array::from_fn(|i| {
        if i < n {
            (
                snap.tids[i].load(Ordering::Relaxed),
                snap.prio[i].load(Ordering::Relaxed),
            )
        } else {
            (u64::MAX, 0)
        }
    });
    let mut after: [(u64, u8); 192] = [(u64::MAX, 0); 192];
    let mut m = 0;
    sched.witness_for_each_queued(|prio, tid| {
        if m < 192 {
            after[m] = (tid.0, prio as u8);
            m += 1;
        }
    });
    // Fast path: most sections change nothing — same current, same queue in the same order.
    if before_cur == after_cur && n == m && before[..n] == after[..m] {
        return;
    }
    let in_set = |set: &[(u64, u8)], tid: u64| set.iter().any(|&(t, _)| t == tid);
    let added: [(u64, u8); 192] = core::array::from_fn(|i| {
        if i < m && !in_set(&before[..n], after[i].0) {
            after[i]
        } else {
            (u64::MAX, 0)
        }
    });
    let removed: [u64; 192] = core::array::from_fn(|i| {
        if i < n && !in_set(&after[..m], before[i].0) {
            before[i].0
        } else {
            u64::MAX
        }
    });
    let n_add = added.iter().filter(|a| a.0 != u64::MAX).count();
    let n_rem = removed.iter().filter(|&&r| r != u64::MAX).count();
    if before_cur == after_cur && n_add == 0 && n_rem == 0 {
        return;
    }
    let acq_gen = ACQ_GEN[idx].load(Ordering::Acquire);
    let mut sec_seq = 0u64;
    if let Some(base) = reserve(1 + n_add + n_rem) {
        sec_seq = base as u64 + 1;
        write(
            base,
            K_SEC,
            idx,
            exec,
            [
                acq_gen,
                before_cur,
                after_cur,
                n_add as u64,
                n_rem as u64,
                instance as u64,
            ],
        );
        let mut k = base + 1;
        for &(tid, prio) in added.iter().filter(|a| a.0 != u64::MAX) {
            write(
                k,
                K_ADD,
                idx,
                exec,
                [acq_gen, tid, u64::from(prio), 0, 0, 0],
            );
            k += 1;
        }
        for &tid in removed.iter().filter(|&&r| r != u64::MAX) {
            write(k, K_REM, idx, exec, [acq_gen, tid, 0, 0, 0, 0]);
            k += 1;
        }
    }
    if n_add > 0
        && let Some(slot) = LAST_ADD.get(exec)
    {
        slot[0].store(idx as u64 + 1, Ordering::Relaxed);
        slot[1].store(acq_gen, Ordering::Relaxed);
        slot[2].store(sec_seq, Ordering::Release);
    }
    if after_cur > 1
        && after_cur != before_cur
        && let Some(slot) = LAST_INSTALL.get(idx)
    {
        slot[0].store(0, Ordering::Release);
        slot[1].store(acq_gen, Ordering::Relaxed);
        slot[3].store(0, Ordering::Relaxed);
        slot[2].store(sec_seq, Ordering::Relaxed);
        slot[0].store(after_cur, Ordering::Release);
    }
    // An OWNER COMMIT by the observer on its own CPU, during a hold.
    let active = HOLD_ACTIVE.load(Ordering::Acquire);
    if active != 0 && exec == idx && idx == OBSERVER.load(Ordering::Acquire) as usize {
        B_COMMITS.fetch_add(1, Ordering::AcqRel);
        if after_cur != 0 && after_cur != before_cur {
            let i = B_INSTALLED_N.fetch_add(1, Ordering::AcqRel);
            if let Some(slot) = B_INSTALLED.get(i) {
                slot[1].store(acq_gen, Ordering::Release);
                slot[2].store(sec_seq, Ordering::Release);
                slot[0].store(after_cur, Ordering::Release);
            }
        }
    }
}

/// The coherent whole-domain view (every 1L, and 1G) was taken / released by the executing CPU.
pub(crate) fn all_acquired() {
    let e = exec_cpu();
    HELD_1G[e].fetch_add(1, Ordering::AcqRel);
    HELD_1L[e].fetch_add(MAX_CPUS as u64, Ordering::AcqRel);
}
pub(crate) fn all_released() {
    let e = exec_cpu();
    HELD_1G[e].fetch_sub(1, Ordering::AcqRel);
    HELD_1L[e].fetch_sub(MAX_CPUS as u64, Ordering::AcqRel);
}

/// A 1G-plus-1L admission guard was taken / released (its 1L is counted by the per-CPU hooks).
pub(crate) fn admission_acquired() {
    HELD_1G[exec_cpu()].fetch_add(1, Ordering::AcqRel);
}
pub(crate) fn admission_released() {
    HELD_1G[exec_cpu()].fetch_sub(1, Ordering::AcqRel);
}

/// A wake owner, in its own rank-2 phase, names the incarnation it is about to place: `tid` with
/// the address space the authoritative TCB holds now. Consumed by that owner's [`claim`].
pub(crate) fn stage_incarnation(tid: u64, asid: Option<crate::kernel::vm::Asid>) {
    let Some(slot) = STAGED.get(exec_cpu()) else {
        return;
    };
    slot[1].store(inc_code(asid), Ordering::Relaxed);
    slot[0].store(tid + 1, Ordering::Release);
}

/// [`stage_incarnation`] from the owner's own rank-2 view of the task table (`tcbs` is held).
pub(crate) fn stage_from_tcbs(tcbs: &[Option<crate::kernel::task::ThreadControlBlock>], tid: u64) {
    if let Some(t) = tcbs.iter().flatten().find(|t| t.tid.0 == tid) {
        stage_incarnation(tid, t.asid);
    }
}

/// A remote-wake owner committed `tid` onto `target` (called right after the commit returned).
/// The claim carries the incarnation this owner staged and the commit — the last section on this
/// executing CPU that added to `target`'s queue — with no lookup by bare TID.
pub(crate) fn claim(_shared: &SharedKernel, target: CpuId, tid: u64) {
    let exec = exec_cpu();
    let staged = STAGED.get(exec).map_or(INC_UNBOUND, |s| {
        let t = s[0].swap(0, Ordering::AcqRel);
        if t == tid + 1 {
            s[1].load(Ordering::Relaxed)
        } else {
            INC_UNBOUND
        }
    });
    if !recording() {
        return;
    }
    let (commit_gen, seq) = LAST_ADD.get(exec).map_or((0, 0), |s| {
        if s[0].load(Ordering::Acquire) == u64::from(target.0) + 1 {
            (s[1].load(Ordering::Relaxed), s[2].load(Ordering::Relaxed))
        } else {
            (0, 0)
        }
    });
    record(
        K_CLAIM,
        target.0 as usize,
        [
            tid,
            staged,
            ROUND.load(Ordering::Acquire),
            commit_gen,
            seq,
            0,
        ],
    );
}

/// A wake notification to `target` (the production owner, at its call).
pub(crate) fn notify(target: CpuId) {
    if !recording() || target.0 as usize >= MAX_CPUS {
        return;
    }
    record(
        K_NOTIFY,
        target.0 as usize,
        [
            ACQ_GEN[target.0 as usize].load(Ordering::Acquire),
            0,
            0,
            0,
            0,
            0,
        ],
    );
}

fn pair(round: u64, n: u64) -> (usize, usize) {
    let n = n.max(2);
    let r = round % (2 * n);
    let a = r % n;
    let b = (a + 1) % n;
    if r < n {
        (a as usize, b as usize)
    } else {
        (b as usize, a as usize)
    }
}

fn total_rounds() -> u64 {
    let n = NCPU.load(Ordering::Acquire);
    if n <= 2 { 2 } else { 2 * n }
}

/// Inside CPU `cpu`'s production timer tick, holding its 1L (`instance`, generation `gen`):
/// observer progress, and — on the round's holder — THE HOLD.
pub(crate) fn on_tick(cpu: CpuId, instance: usize, held_flag: impl Fn() -> bool, sched: &CpuSched) {
    let c = cpu.0 as usize;
    if let Some(busy) = BUSY.get(c) {
        let mut queued = 0usize;
        sched.witness_for_each_queued(|_, _| queued += 1);
        busy.store(
            sched.witness_current().is_some_and(|t| t.0 != 0) && queued > 0,
            Ordering::Release,
        );
    }
    // RUN: the first tick on this CPU after an installation finds the installed task current —
    // kernel entries run masked, so this tick interrupted that task. The address space the CPU
    // has ACTIVE now is that task's incarnation, from the activation table (no wake owner's word).
    let running_inc = inc_code(crate::arch::hal::active_address_space(cpu));
    if let Some(cur) = sched.witness_current()
        && cur.0 != 0
    {
        let _ = observe_run(c, cur.0, 0);
    }
    let active = HOLD_ACTIVE.load(Ordering::Acquire);
    if active != 0 && c == OBSERVER.load(Ordering::Acquire) as usize {
        if let Some(cur) = sched.witness_current() {
            let n = B_INSTALLED_N.load(Ordering::Acquire).min(B_INSTALLED.len());
            if let Some(slot) = B_INSTALLED[..n]
                .iter()
                .find(|s| s[0].load(Ordering::Acquire) == cur.0 + 1)
            {
                // The record is reserved BEFORE the count, so a progress event the holder counts
                // always precedes its HOLD_END in sequence order.
                record(
                    K_PROGRESS,
                    c,
                    [
                        active - 1,
                        cur.0,
                        ACQ_GEN[c].load(Ordering::Acquire),
                        slot[1].load(Ordering::Acquire),
                        running_inc,
                        slot[2].load(Ordering::Acquire),
                    ],
                );
                B_PROGRESS.fetch_add(1, Ordering::AcqRel);
            }
        }
        return;
    }
    if !recording() || active != 0 {
        return;
    }
    let round = ROUND.load(Ordering::Acquire);
    if round >= total_rounds() {
        return;
    }
    let (a, b) = pair(round, NCPU.load(Ordering::Acquire));
    if c != a || !BUSY[b].load(Ordering::Acquire) {
        // Not this CPU's turn, or its observer has no local scheduling to do now: no attempt.
        return;
    }
    // ── THE HOLD: this CPU's 1L is held by this very tick, interrupts masked ──
    B_COMMITS.store(0, Ordering::Release);
    B_PROGRESS.store(0, Ordering::Release);
    B_RELATED.store(0, Ordering::Release);
    B_INSTALLED_N.store(0, Ordering::Release);
    HOLDER.store(a as u8, Ordering::Release);
    OBSERVER.store(b as u8, Ordering::Release);
    let acq_gen = ACQ_GEN[a].load(Ordering::Acquire);
    let held_at_begin = held_flag();
    record(
        K_HOLD_BEGIN,
        a,
        [
            round,
            b as u64,
            instance as u64,
            acq_gen,
            HELD_1L[a].load(Ordering::Acquire) | (HELD_1G[a].load(Ordering::Acquire) << 32),
            u64::from(held_at_begin)
                | (u64::from(irqs_enabled()) << 1)
                | (ATTEMPT.load(Ordering::Acquire) << 8),
        ],
    );
    HOLD_ACTIVE.store(round + 1, Ordering::SeqCst);
    let mut spins = 0u64;
    let ok = loop {
        if B_COMMITS.load(Ordering::Acquire) >= 2 && B_PROGRESS.load(Ordering::Acquire) >= 1 {
            break true;
        }
        if B_RELATED.load(Ordering::Acquire) != 0 || spins >= HOLD_SPINS {
            break false;
        }
        spins += 1;
        core::hint::spin_loop();
    };
    HOLD_ACTIVE.store(0, Ordering::SeqCst);
    record(
        K_HOLD_END,
        a,
        [
            round,
            B_COMMITS.load(Ordering::Acquire),
            B_PROGRESS.load(Ordering::Acquire),
            B_RELATED.load(Ordering::Acquire),
            spins,
            u64::from(held_flag())
                | (u64::from(ok) << 1)
                | (u64::from(irqs_enabled()) << 2)
                | (ACQ_GEN[a].load(Ordering::Acquire) << 8),
        ],
    );
    if ok {
        ROUNDS_OK.fetch_add(1, Ordering::AcqRel);
        ATTEMPT.store(0, Ordering::Release);
        ROUND.fetch_add(1, Ordering::AcqRel);
    } else if ATTEMPT.fetch_add(1, Ordering::AcqRel) + 1 >= ATTEMPTS {
        ATTEMPT.store(0, Ordering::Release);
        ROUND.fetch_add(1, Ordering::AcqRel);
    }
}

/// Record every CPU's membership under `view` — the coherent whole-domain view (every 1L held).
fn snapshot(
    view: &crate::kernel::scheduler::SchedAll<'_>,
    shared: &SharedKernel,
    kinds: (u8, u8, u8),
    extra: u64,
) {
    let domain = shared.sched_domain();
    let n = NCPU.load(Ordering::Acquire) as usize;
    record(
        kinds.0,
        0,
        [
            n as u64,
            domain.topo().online(),
            domain.topo().dispatching(),
            domain.clock_now().0,
            extra,
            ROUND.load(Ordering::Acquire),
        ],
    );
    // The authoritative CPU → lock-instance map: from the domain's own cell array, under every 1L.
    for cpu in 0..n {
        let (cell, lock) = domain.witness_cell_addrs(cpu);
        record(
            K_CELL,
            cpu,
            [
                cell as u64,
                lock as u64,
                u64::from(kinds.0 == K_CENSUS_BEGIN),
                0,
                0,
                0,
            ],
        );
    }
    for cpu in 0..n {
        let sched = view.witness_cell(CpuId(cpu as u8));
        record(
            kinds.1,
            cpu,
            [
                enc(sched.witness_current()),
                ACQ_GEN[cpu].load(Ordering::Acquire),
                0,
                0,
                0,
                0,
            ],
        );
        sched.witness_for_each_queued(|prio, tid| {
            record(kinds.2, cpu, [tid.0, prio as u64, 0, 0, 0, 0]);
        });
    }
}

/// After a production timer tick on `cpu`, with no scheduler lock held: open the window (CPU 0,
/// once phase-A spinners run on every present CPU) or, past the tick bound, close it (normally
/// [`close_at_end`] closes it). Opening and closing both happen under
/// the coherent view, so no 1L section straddles the opening or the closing: the SNAP is exactly
/// the state before the first recorded section and the CENSUS exactly the state after the last.
pub(crate) fn after_tick(shared: &SharedKernel, cpu: CpuId) {
    if !crate::kernel::apsched_witness::enabled() {
        return;
    }
    match PHASE.load(Ordering::Acquire) {
        0 if cpu.0 == 0 => {
            let topo = shared.sched_domain().topo();
            let present = topo.present();
            if present.count_ones() < 2 || topo.dispatching() != present {
                return;
            }
            // The window opens once the workload's phase-A spinners run on every present CPU
            // (two per CPU, rotated by timer preemption: local scheduling on each CPU).
            let started = crate::kernel::apsched_witness::spinner_cpus();
            if started & present != present {
                return;
            }
            OPENED_AT.store(shared.sched_domain().clock_now().0, Ordering::Release);
            NCPU.store(
                u64::from(present.count_ones()).min(MAX_W_CPUS as u64),
                Ordering::Release,
            );
            let domain = shared.sched_domain();
            let mut guard = domain.lock_all();
            let view = guard.view();
            snapshot(&view, shared, (K_SNAP_BEGIN, K_SNAP_CUR, K_SNAP_Q), 0);
            PHASE.store(1, Ordering::Release);
            drop(view);
            drop(guard);
        }
        1 if shared.sched_domain().clock_now().0
            > OPENED_AT.load(Ordering::Acquire) + WINDOW_MAX_TICKS
            && HOLD_ACTIVE.load(Ordering::Acquire) == 0 =>
        {
            close(shared);
        }
        _ => {}
    }
}

/// The workload's last step (P_END, from its log syscall: no scheduler or domain lock is held
/// there), before either dump: the window closes under the coherent view, so the natural trace
/// spans every workload phase after A — idle-IPI wakes, cross-CPU IPC, fork and reap.
pub(crate) fn close_at_end(shared: &SharedKernel) {
    if PHASE.load(Ordering::Acquire) != 1 {
        return;
    }
    // A hold in progress ends by its own bound; the coherent view would wait for it anyway.
    let mut spins = 0u64;
    while HOLD_ACTIVE.load(Ordering::Acquire) != 0 && spins < 2 * HOLD_SPINS {
        core::hint::spin_loop();
        spins += 1;
    }
    close(shared);
}

fn close(shared: &SharedKernel) {
    let domain = shared.sched_domain();
    let mut guard = domain.lock_all();
    if PHASE
        .compare_exchange(1, 3, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let view = guard.view();
    snapshot(
        &view,
        shared,
        (K_CENSUS_BEGIN, K_CENSUS_CUR, K_CENSUS_Q),
        ROUNDS_OK.load(Ordering::Acquire),
    );
    record(
        K_CENSUS_END,
        0,
        [
            ROUNDS_OK.load(Ordering::Acquire),
            total_rounds(),
            0,
            0,
            0,
            0,
        ],
    );
    PHASE.store(2, Ordering::Release);
    drop(view);
    drop(guard);
}

/// FIXTURE SYNCHRONIZATION (labelled): until every hold round has run, the AP-scheduling witness
/// withholds its preemption counts from the workload, so phase A's spinners — the observers'
/// local scheduling — keep running through the rounds. The window's tick bound closes it, so this
/// never holds the workload indefinitely.
pub(crate) fn holds_phase_a() -> bool {
    match PHASE.load(Ordering::Acquire) {
        0 => true,
        1 => ROUND.load(Ordering::Acquire) < total_rounds(),
        _ => false,
    }
}

/// The sealed dump, after the AP-scheduling witness's own.
pub(crate) fn dump() {
    let n = NEXT.load(Ordering::Acquire).min(RECS_MAX);
    for (i, r) in RECS.iter().take(n).enumerate() {
        let mut spins = 0u32;
        while !r.valid.load(Ordering::Acquire) && spins < 1_000_000 {
            core::hint::spin_loop();
            spins += 1;
        }
        let f = |j: usize| r.f[j].load(Ordering::Relaxed);
        crate::kernel::apsched_witness::emit_twice(format_args!(
            "SL1W_R seq={} kind={} cpu={} exec={} f0={} f1={} f2={} f3={} f4={} f5={}",
            i + 1,
            KINDS
                .get(r.kind.load(Ordering::Relaxed) as usize)
                .copied()
                .unwrap_or("UNKNOWN"),
            r.cpu.load(Ordering::Relaxed),
            r.exec.load(Ordering::Relaxed),
            f(0),
            f(1),
            f(2),
            f(3),
            f(4),
            f(5)
        ));
    }
    crate::kernel::apsched_witness::emit_twice(format_args!(
        "SL1W_END count={} overflow={} phase={} ncpu={} rounds={} rounds_ok={}",
        n,
        u8::from(OVERFLOW.load(Ordering::Acquire)),
        PHASE.load(Ordering::Acquire),
        NCPU.load(Ordering::Acquire),
        total_rounds(),
        ROUNDS_OK.load(Ordering::Acquire)
    ));
}
