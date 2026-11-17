// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RT-SCHED1A — the RT selection witness (`doc/SCHEDULING_CONTRACT.md` §23). Built only with
//! `<port>-rt-sched-witness`, and armed only by the deployment configuration
//! `yarm.sched_profile=rt`: everything it shows runs through the production owners of the RT
//! profile — the enqueue planner and commit, `hal::notify_enqueued` (the local self-raise and the
//! remote reschedule IPI), the RT scheduling point (`syscall_split::settle_rt_reschedule`), the
//! preempting tick, the yield transaction's head/tail requeue, the one selection owner and a
//! kernel thread's `sched_point`.
//!
//! Its population, all with CONFIGURED priorities and fixed homes:
//!
//! * two user tasks, each its own process running `arch/<port>/rt_witness.S`: **UA** on CPU 0 at
//!   level 20, a yield storm, and **UB** on CPU 1 at level 20, which yields only when asked;
//! * kernel threads **H0** (CPU 0, level 4), **H1** (CPU 1, level 4), **E** (CPU 1, level 20 —
//!   UB's equal), **L** (CPU 0, level 16), the dumper **DUMP** (CPU 0, level 3) and the setup
//!   thread **S** (CPU 0, level 1).
//!
//! The phases, each started by CPU 0's timer interrupt (the driver, [`on_timer`]) AFTER that
//! interrupt's tick decision, and each wake issued there through the production wake owner:
//!
//! * **A** (×3) — a LOCAL wake: H0 is woken on CPU 0 while UA's yield storm runs there. The wake's
//!   self-raised reschedule is taken on the return to UA and must preempt it (cause 1); H0 runs.
//! * **B** — a REMOTE wake through the real IPI: H1 is woken FROM CPU 0 while UB, observed running
//!   (its heartbeat advancing), holds CPU 1. CPU 1's next scheduling point must preempt UB — the
//!   reschedule arrival, or a CPU 1 tick that reached H1 first (the grader requires the arrival in
//!   at least one of the boot's remote wakes); H1 runs.
//! * **C** — an EQUAL-priority wake: E (level 20) is woken while UB (level 20) runs. The arrival
//!   must be declined, and CPU 1's ticks must not rotate UB (several are counted with UB still
//!   current and E not run). Then H1 is woken again and preempts UB, which must return to the
//!   HEAD of level 20 — ahead of E — so UB, not E, resumes when H1 parks (more ticks are counted on
//!   UB). E runs only after UB's ONE explicit yield (requested by the driver): UB went to its
//!   level's tail, behind E.
//! * **D** — a higher-priority wake during a running KERNEL thread: L (level 16) takes CPU 0 from
//!   UA, then H0 is woken while L runs. L's next scheduling point must yield (`rt=true`); H0 runs;
//!   L resumes and finishes.
//!
//! RT-SCHED1B (§24) — with `yarm.sched_budget=on` as well, two budget phases follow D, on the same
//! production owners plus the scheduling contexts (`kernel::sched_budget`):
//!
//! * **E** — a third user task **UH** (CPU 1, level 8, budget 2 of every 8 ticks, never yields) is
//!   placed FROM CPU 0 while UB runs: the remote wake preempts UB. UH exhausts its budget;
//!   the next scheduling point on CPU 1 (its tick) takes the CPU from it and parks it with no queue
//!   position, so the LOWER-priority UB runs on the same CPU. UH returns only at its fixed release
//!   instant (bound + k·8), rejoining its level's tail once and preempting UB at the tick. Three
//!   cycles are recorded with both heartbeats.
//! * **F** — a budgeted cooperative kernel thread **K** (CPU 0, level 6, budget 2 of every 10)
//!   works in chunks with a scheduling point after each: it is charged, its exhaustion leaves a
//!   reschedule pending that its next scheduling point honours (`rt=true`), it parks there, and it
//!   resumes from that same point only after its release. Three cycles.
//!
//! The dump also records each CPU's ledger (idle, unaccounted, essential work) and UH's and K's
//! contexts (charged, overrun, exhaustions, releases).
//!
//! Every fact is one record under a single causal stamp; the dumper prints them twice with a
//! checksum per line and a seal, and `scripts/grade-rt-sched-witness.py` decides the verdict from
//! phase ordering and exact task identities. Nothing here decides a verdict.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::kthread::{KThreadCtx, KThreadId, KWaitQueue};
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};
use crate::runtime::SharedKernel;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(include_str!("../arch/riscv64/rt_witness.S"));
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("../arch/aarch64/rt_witness.S"));
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("../arch/x86_64/rt_witness.S"));

unsafe extern "C" {
    static yarm_rtw_user_start: u8;
    static yarm_rtw_user_end: u8;
}

const CODE_VA: u64 = 0x3000_0000;
const STACK_VA: u64 = 0x3001_0000;
const SP_TOP: u64 = 0x3001_0ff0;
const MBX_VA: u64 = 0x3002_0000;

/// The mailbox words (`rt_witness.S`).
const M_HB: u64 = 0x00;
const M_YIELDS: u64 = 0x08;
const M_YREQ: u64 = 0x10;
const M_STORM: u64 = 0x20;
const M_CHUNK: u64 = 0x28;

pub const UA_TID: u64 = 9_700;
pub const UB_TID: u64 = 9_701;
const USER_LEVEL: usize = 20;
/// UA's spin between yields. Small, so the storm runs even in the slivers a level-0 server leaves
/// CPU 0 (the supervisor's one-tick receive-timeout poll takes most of each RISC-V tick).
const UA_CHUNK: u64 = 20_000;
const UB_CHUNK: u64 = 100_000;
/// RT-SCHED1B: UH, the budgeted high-priority user task of phase E.
pub const UH_TID: u64 = 9_702;
const UH_LEVEL: usize = 8;
const UH_BUDGET: (u64, u64) = (2, 8);
/// RT-SCHED1B: K, the budgeted cooperative kernel thread of phase F.
const K_LEVEL: usize = 6;
const K_BUDGET: (u64, u64) = (2, 10);
const K_CHUNK: u64 = 20_000;
const K_MAX_CHUNKS: u64 = 20_000_000;
/// Rejoins (after exhaustion) phase E and phase F each record.
const E_CYCLES: u64 = 3;
const F_CYCLES: u64 = 3;

// Roles (kernel threads index 0..=5 and 8; the user tasks are 6, 7 and 9).
const R_H0: usize = 0;
const R_H1: usize = 1;
const R_E: usize = 2;
const R_L: usize = 3;
const R_DUMP: usize = 4;
const R_S: usize = 5;
const R_UA: usize = 6;
const R_UB: usize = 7;
const R_K: usize = 8;
const R_UH: usize = 9;
const ROLES: usize = 10;
const ROLE_NAMES: [&str; ROLES] = ["H0", "H1", "E", "L", "DUMP", "S", "UA", "UB", "K", "UH"];
/// (home CPU, level) of each kernel-thread role.
const KT_PLAN: [(u8, usize); 6] = [(0, 4), (1, 4), (1, USER_LEVEL), (0, 16), (0, 3), (0, 1)];

/// (home CPU, level) of every role.
fn plan(role: usize) -> (u8, usize) {
    match role {
        r if r < KT_PLAN.len() => KT_PLAN[r],
        R_UA => (0, USER_LEVEL),
        R_UB => (1, USER_LEVEL),
        R_K => (0, K_LEVEL),
        _ => (1, UH_LEVEL),
    }
}

// Phases.
const P_OFF: u64 = 0;
const P_WAIT_IDLE: u64 = 1;
const P_SETUP: u64 = 2;
const P_WARMUP: u64 = 3;
const P_A_ARM: u64 = 4;
const P_A_WAIT: u64 = 5;
const P_B_ARM: u64 = 6;
const P_B_WAIT: u64 = 7;
const P_C_ARM: u64 = 8;
const P_C_WAIT: u64 = 9;
const P_C_H1: u64 = 10;
const P_C_AFTER: u64 = 11;
const P_C_YIELD: u64 = 12;
const P_D_ARM: u64 = 13;
const P_D_L: u64 = 14;
const P_D_WAIT: u64 = 15;
const P_E_ARM: u64 = 16;
const P_E_PLACE: u64 = 17;
const P_E_RUN: u64 = 18;
const P_F_ARM: u64 = 19;
const P_F_RUN: u64 = 20;
const P_DONE: u64 = 21;
const P_DUMPING: u64 = 22;
const PHASE_NAMES: [&str; 23] = [
    "OFF",
    "WAIT_IDLE",
    "SETUP",
    "WARMUP",
    "A_ARM",
    "A_WAIT",
    "B_ARM",
    "B_WAIT",
    "C_ARM",
    "C_WAIT",
    "C_H1",
    "C_AFTER",
    "C_YIELD",
    "D_ARM",
    "D_L",
    "D_WAIT",
    "E_ARM",
    "E_PLACE",
    "E_RUN",
    "F_ARM",
    "F_RUN",
    "DONE",
    "DUMPING",
];

/// Local-wake rounds of phase A.
const A_ROUNDS: u64 = 3;
/// CPU 1 ticks that must land on UB, with E queued and unrun, before H1 preempts it.
const C_TICKS_BEFORE: u64 = 2;
/// CPU 1 ticks that must land on UB after H1 parked, with E still queued and unrun, before UB is
/// asked to yield.
const C_TICKS_MIN: u64 = 4;
/// CPU 0 ticks any one phase may take before the witness gives up on it (recorded) and dumps.
const PHASE_TICK_LIMIT: u64 = 1_500;
/// L's work: chunks of spin, a scheduling point after each.
const L_CHUNK: u64 = 20_000;
const L_MAX_CHUNKS: u64 = 2_000_000;
const L_AFTER: u64 = 8;

// Record kinds.
const K_PROVISION: u64 = 1;
const K_START: u64 = 2;
const K_WAKE: u64 = 3;
const K_PREEMPT: u64 = 4;
const K_DECLINED: u64 = 5;
const K_RUN: u64 = 6;
const K_SCHEDPT: u64 = 7;
const K_TICK: u64 = 8;
const K_YREQ: u64 = 9;
const K_LSTART: u64 = 10;
const K_LRESUMED: u64 = 11;
const K_LEND: u64 = 12;
const K_PHASE: u64 = 13;
const K_TIMEOUT: u64 = 14;
const K_USER: u64 = 15;
const K_BIND: u64 = 16;
const K_EXHAUST: u64 = 17;
const K_PARK: u64 = 18;
const K_REPLENISH: u64 = 19;
const K_KSTART: u64 = 20;
const K_KRESUMED: u64 = 21;
const K_KEND: u64 = 22;
const K_LEDGER: u64 = 23;
const K_CTX: u64 = 24;
const KIND_NAMES: [&str; 25] = [
    "NONE",
    "PROVISION",
    "START",
    "WAKE",
    "PREEMPT",
    "DECLINED",
    "RUN",
    "SCHEDPT",
    "TICK",
    "YREQ",
    "LSTART",
    "LRESUMED",
    "LEND",
    "PHASE",
    "TIMEOUT",
    "USER",
    "BUDGET_BIND",
    "BUDGET_EXHAUST",
    "BUDGET_PARK",
    "BUDGET_REPLENISH",
    "KSTART",
    "KRESUMED",
    "KEND",
    "LEDGER",
    "CTX",
];

static PROVISIONED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static PHASE: AtomicU64 = AtomicU64::new(P_OFF);
static PHASE_SEEN: AtomicU64 = AtomicU64::new(P_OFF);
static PHASE_TICKS: AtomicU64 = AtomicU64::new(0);
static A_ROUND: AtomicU64 = AtomicU64::new(0);
static C_TICKS: AtomicU64 = AtomicU64::new(0);
static LAST_UB_HB: AtomicU64 = AtomicU64::new(0);
static H0_D_DONE: AtomicBool = AtomicBool::new(false);
/// L recorded its start: only then may the driver issue H0's phase-D wake over it (a tick can land
/// on L after its dispatch but before its first record).
static L_STARTED: AtomicBool = AtomicBool::new(false);
static POOL: AtomicU64 = AtomicU64::new(0);
/// RT-SCHED1B: the domain enforces budgets and UH was provisioned — phases E and F run.
static BUDGETED: AtomicBool = AtomicBool::new(false);
static UH_PROVISIONED: AtomicBool = AtomicBool::new(false);
static E_REJOINS: AtomicU64 = AtomicU64::new(0);
/// UH was charged again after its last recorded rejoin: it ran.
static E_RAN_AFTER: AtomicBool = AtomicBool::new(false);
static F_REJOINS: AtomicU64 = AtomicU64::new(0);
/// K's completed chunks (its own heartbeat).
static K_PROGRESS: AtomicU64 = AtomicU64::new(0);
static MBX_PHYS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// The exact identity of each role: a kernel thread's id, or a user task's TID.
static ROLE_TID: [AtomicU64; ROLES] = [const { AtomicU64::new(0) }; ROLES];
static GO: [AtomicU64; R_K + 1] = [const { AtomicU64::new(0) }; R_K + 1];
static QS: [KWaitQueue; R_K + 1] = [const { KWaitQueue::new() }; R_K + 1];

fn phase() -> u64 {
    PHASE.load(Ordering::Acquire)
}

fn set_phase(p: u64) {
    PHASE.store(p, Ordering::Release);
    record(K_PHASE, 0, [p, 0, 0, 0, 0]);
}

/// Recording runs from the setup to the dump.
fn active() -> bool {
    let p = phase();
    p >= P_SETUP && p < P_DUMPING
}

// ─────────────────────────────── the record ───────────────────────────────

const MAX_RECS: usize = 2048;

struct Rec {
    valid: AtomicBool,
    kind: AtomicU64,
    cpu: AtomicU64,
    v: [AtomicU64; 5],
}

static RECS: [Rec; MAX_RECS] = [const {
    Rec {
        valid: AtomicBool::new(false),
        kind: AtomicU64::new(0),
        cpu: AtomicU64::new(0),
        v: [const { AtomicU64::new(0) }; 5],
    }
}; MAX_RECS];
static SEQ: AtomicU64 = AtomicU64::new(0);
static OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Append one record under the next causal stamp. Lock-free (each stamp owns one slot), never
/// logs: safe from interrupt context.
fn record(kind: u64, cpu: u64, v: [u64; 5]) {
    let seq = SEQ.fetch_add(1, Ordering::AcqRel);
    let Some(r) = RECS.get(seq as usize) else {
        OVERFLOW.fetch_add(1, Ordering::AcqRel);
        return;
    };
    r.kind.store(kind, Ordering::Relaxed);
    r.cpu.store(cpu, Ordering::Relaxed);
    for (slot, value) in r.v.iter().zip(v) {
        slot.store(value, Ordering::Relaxed);
    }
    r.valid.store(true, Ordering::Release);
}

/// A user task's mailbox word, read or written through the direct map with no lock (the task
/// itself only reads the words the kernel writes, and the reverse).
fn mbx_ptr(user: usize, off: u64) -> Option<*mut u64> {
    let phys = MBX_PHYS.get(user)?.load(Ordering::Acquire);
    if phys == 0 {
        return None;
    }
    KernelState::phys_to_direct_map_ptr(phys + off).map(|p| p.cast::<u64>())
}

fn mbx_read(user: usize, off: u64) -> u64 {
    // SAFETY: an aligned u64 inside the mailbox frame this witness allocated; the direct map
    // covers it for the life of the kernel.
    mbx_ptr(user, off).map_or(0, |p| unsafe { core::ptr::read_volatile(p) })
}

fn mbx_write(user: usize, off: u64, value: u64) {
    if let Some(p) = mbx_ptr(user, off) {
        // SAFETY: as in `mbx_read`.
        unsafe { core::ptr::write_volatile(p, value) };
    }
}

// ─────────────────────────────── provisioning ───────────────────────────────

fn map_fresh(
    kernel: &mut KernelState,
    asid: Asid,
    va: u64,
    count: u64,
) -> Result<u64, KernelError> {
    let mut first = 0;
    for i in 0..count {
        let phys = kernel.alloc_user_data_frame()?;
        if i == 0 {
            first = phys;
        }
        kernel.map_user_page_in_asid_raw(
            asid,
            VirtAddr(va + i * 0x1000),
            Mapping {
                phys: PhysAddr(phys),
                flags: PageFlags {
                    read: true,
                    write: true,
                    execute: true,
                    user: true,
                    cache_policy: CachePolicy::WriteBack,
                },
            },
        )?;
    }
    Ok(first)
}

/// Provision UA and UB from the bootstrap window: each its own process (space, program, stack,
/// mailbox), registered as an App task with its CONFIGURED priority (level 20). Neither is placed
/// here: S places each on its home through the production planner once CPU 1 dispatches. Armed
/// only by `yarm.sched_profile=rt`.
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    if crate::kernel::boot::sched_profile() != crate::kernel::scheduler::SchedProfile::RealTime {
        crate::yarm_log!("RTW_PROVISION result=skip reason=gp_profile");
        return Ok(());
    }
    // SAFETY (both `&raw const`): taking the address of an extern static reads nothing.
    let start = &raw const yarm_rtw_user_start;
    let len = &raw const yarm_rtw_user_end as usize - start as usize;
    // SAFETY: `start..start + len` is the program's bytes inside this kernel image's text.
    let image = unsafe { core::slice::from_raw_parts(start, len) }.to_vec();
    // RT-SCHED1B: UH only when budgets are requested (it is placed in phase E alone).
    let budgeted = crate::kernel::boot::sched_budget_requested();
    let users: &[(u64, u64, u64)] = if budgeted {
        &[
            (UA_TID, 1, UA_CHUNK),
            (UB_TID, 0, UB_CHUNK),
            (UH_TID, 0, UB_CHUNK),
        ]
    } else {
        &[(UA_TID, 1, UA_CHUNK), (UB_TID, 0, UB_CHUNK)]
    };
    for (i, &(tid, storm, chunk)) in users.iter().enumerate() {
        let role = [R_UA, R_UB, R_UH][i];
        let lvl = plan(role).1;
        let level = TaskPriority::from_level(lvl).ok_or(KernelError::TaskMissing)?;
        let (asid, _root) = kernel.create_user_address_space()?;
        map_fresh(kernel, asid, CODE_VA, (len as u64).div_ceil(0x1000))?;
        map_fresh(kernel, asid, STACK_VA, 1)?;
        let mbx = map_fresh(kernel, asid, MBX_VA, 1)?;
        kernel.copy_to_user(asid, VirtAddr(CODE_VA), &image)?;
        let mut words = [0u8; 0x1000];
        words[M_STORM as usize..M_STORM as usize + 8].copy_from_slice(&storm.to_le_bytes());
        words[M_CHUNK as usize..M_CHUNK as usize + 8].copy_from_slice(&chunk.to_le_bytes());
        kernel.copy_to_user(asid, VirtAddr(MBX_VA), &words)?;
        kernel.register_task_with_class(tid, crate::kernel::task::TaskClass::App)?;
        kernel.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|t| t.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.asid = Some(asid);
            tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
            tcb.user_context.stack_ptr = VirtAddr(SP_TOP);
            crate::kernel::task_enqueue::configure_priority_locked(&mut tcbs[..], tid, level)
                .map_err(|_| KernelError::TaskMissing)?;
            if role == R_UH {
                // The deployment configuration's budget, bound at UH's first placement.
                let params =
                    crate::kernel::sched_budget::BudgetParams::validate(UH_BUDGET.0, UH_BUDGET.1)
                        .map_err(|_| KernelError::TaskMissing)?;
                crate::kernel::task_enqueue::configure_budget_locked(&mut tcbs[..], tid, params)
                    .map_err(|_| KernelError::TaskMissing)?;
            }
            Ok(())
        })?;
        MBX_PHYS[i].store(mbx, Ordering::Release);
        ROLE_TID[role].store(tid, Ordering::Release);
        crate::kernel::printk::printk_emit_sync(format_args!(
            "RTW_PROVISIONED role={} tid={} asid={} level={} storm={} chunk={} image={} placed=0 budget={}",
            ROLE_NAMES[role],
            tid,
            asid.0,
            lvl,
            storm,
            chunk,
            image.len(),
            if role == R_UH { "2/8" } else { "default" }
        ));
    }
    UH_PROVISIONED.store(budgeted, Ordering::Release);
    PROVISIONED.store(true, Ordering::Release);
    Ok(())
}

// ─────────────────────────────── start (the drain owner, CPU 0) ───────────────────────────────

/// The drain owner ran on `cpu`: once, on CPU 0, create S (which parks until the driver releases
/// it) and arm the driver.
pub(crate) fn maybe_start(shared: &SharedKernel, cpu: CpuId) {
    if cpu.0 != 0
        || !PROVISIONED.load(Ordering::Acquire)
        || STARTED.load(Ordering::Acquire)
        || STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    POOL.store(
        &shared.kthreads as *const crate::kernel::kthread::KThreadPool as u64,
        Ordering::Release,
    );
    let budgeted = shared.sched_domain().budget_enabled() && UH_PROVISIONED.load(Ordering::Acquire);
    BUDGETED.store(budgeted, Ordering::Release);
    let (home, level) = KT_PLAN[R_S];
    match create(shared, R_S, home, level) {
        Some(id) => {
            PHASE.store(P_WAIT_IDLE, Ordering::Release);
            crate::yarm_log!(
                "RTW_START s={:#x} cpu=0 budget_phases={}",
                id.raw(),
                budgeted as u8
            );
        }
        None => crate::yarm_log!("RTW_START result=fail reason=create_s"),
    }
}

fn pool() -> Option<&'static crate::kernel::kthread::KThreadPool> {
    let p = POOL.load(Ordering::Acquire);
    // SAFETY: the address of the kernel's own `SharedKernel::kthreads`, which lives for the
    // kernel's lifetime; stored once in `maybe_start`.
    (p != 0).then(|| unsafe { &*(p as *const crate::kernel::kthread::KThreadPool) })
}

fn create(shared: &SharedKernel, role: usize, home: u8, level: usize) -> Option<KThreadId> {
    let prio = TaskPriority::from_level(level)?;
    // RT-SCHED1B: K runs under its own budgeted context (an independent worker, SC-EXEC-3).
    let budget = (role == R_K)
        .then(|| crate::kernel::sched_budget::BudgetParams::validate(K_BUDGET.0, K_BUDGET.1).ok())
        .flatten();
    let id = shared
        .kthreads
        .create_budgeted(body, role, CpuId(home), prio, budget)
        .ok()?;
    ROLE_TID[role].store(id.tid().0, Ordering::Release);
    Some(id)
}

/// Release role `role`'s parked thread once more (any context: the queue lock is interrupt-safe
/// and the wake takes only the home CPU's 1L, notifying after it is released).
fn go(role: usize) {
    GO[role].fetch_add(1, Ordering::AcqRel);
    if let Some(p) = pool() {
        let _ = QS[role].wake_all(p);
    }
}

// ─────────────────────────────── the kernel-thread bodies ───────────────────────────────

fn body(ctx: &KThreadCtx, role: usize) {
    let (home, level) = plan(role);
    record(
        K_START,
        ctx.home().0 as u64,
        [role as u64, ctx.id().tid().0, home as u64, level as u64, 0],
    );
    let mut seen = 0u64;
    loop {
        let _ = QS[role].wait_until(ctx, || GO[role].load(Ordering::Acquire) > seen);
        seen = GO[role].load(Ordering::Acquire);
        match role {
            R_S if phase() == P_E_PLACE => place_uh(ctx),
            R_S => setup(ctx),
            R_L => l_work(ctx),
            R_K => k_work(ctx),
            R_DUMP => dump(ctx),
            _ => run_once(ctx, role),
        }
    }
}

/// H0, H1, E: record the run (with both user tasks' yield counts at this instant) and advance.
fn run_once(ctx: &KThreadCtx, role: usize) {
    let p = phase();
    let round = A_ROUND.load(Ordering::Acquire);
    record(
        K_RUN,
        ctx.home().0 as u64,
        [
            role as u64,
            ctx.id().tid().0,
            p,
            round,
            mbx_read(0, M_YIELDS) << 32 | (mbx_read(1, M_YIELDS) & 0xFFFF_FFFF),
        ],
    );
    match (role, p) {
        (R_H0, P_A_WAIT) => set_phase(if round < A_ROUNDS { P_A_ARM } else { P_B_ARM }),
        (R_H0, P_D_WAIT) => H0_D_DONE.store(true, Ordering::Release),
        (R_H1, P_B_WAIT) => set_phase(P_C_ARM),
        (R_H1, P_C_H1) => {
            C_TICKS.store(0, Ordering::Release);
            set_phase(P_C_AFTER);
        }
        // E must run only in C_YIELD; anywhere else the grader fails it (rotation or position).
        (R_E, P_C_WAIT | P_C_H1 | P_C_AFTER | P_C_YIELD) => set_phase(P_D_ARM),
        _ => {}
    }
}

/// S: place UA and UB on their homes through the production planner, then hand over to the
/// driver.
fn setup(ctx: &KThreadCtx) {
    let shared = ctx.shared();
    set_phase(P_SETUP);
    let roles: &[usize] = if BUDGETED.load(Ordering::Acquire) {
        &[R_H0, R_H1, R_E, R_L, R_DUMP, R_K]
    } else {
        &[R_H0, R_H1, R_E, R_L, R_DUMP]
    };
    for &role in roles {
        let (home, level) = plan(role);
        if create(shared, role, home, level).is_none() {
            record(K_TIMEOUT, 0, [P_SETUP, role as u64, 1, 0, 0]);
        }
    }
    for (role, cpu) in [(R_UA, 0u8), (R_UB, 1u8)] {
        let tid = ROLE_TID[role].load(Ordering::Acquire);
        let placed = shared.enqueue_on_cpu_split(CpuId(cpu), tid);
        record(
            K_USER,
            ctx.home().0 as u64,
            [
                role as u64,
                tid,
                cpu as u64,
                placed.map_or(u64::MAX, |c| c.0 as u64),
                0,
            ],
        );
    }
    set_phase(P_WARMUP);
}

/// L: take the CPU and work in chunks, a scheduling point after each, until H0 has run in phase D
/// and a few more chunks are done.
fn l_work(ctx: &KThreadCtx) {
    let cpu = ctx.home().0 as u64;
    record(K_LSTART, cpu, [ctx.id().tid().0, phase(), 0, 0, 0]);
    L_STARTED.store(true, Ordering::Release);
    let mut chunks = 0u64;
    let mut after = 0u64;
    while chunks < L_MAX_CHUNKS {
        let mut x = chunks;
        for _ in 0..L_CHUNK {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        chunks += 1;
        if let Ok(true) = ctx.sched_point() {
            record(K_LRESUMED, cpu, [ctx.id().tid().0, chunks, phase(), 0, 0]);
        }
        if H0_D_DONE.load(Ordering::Acquire) {
            after += 1;
            if after >= L_AFTER {
                break;
            }
        }
    }
    record(K_LEND, cpu, [ctx.id().tid().0, chunks, after, 0, 0]);
    if phase() == P_D_WAIT {
        set_phase(if BUDGETED.load(Ordering::Acquire) {
            P_E_ARM
        } else {
            P_DONE
        });
    }
}

// ─────────────────────────────── RT-SCHED1B: phases E and F ───────────────────────────────

/// S, phase E: place UH on CPU 1 through the production planner, FROM CPU 0 (the remote wake's
/// reschedule IPI), while UB runs there. The placement binds UH's configured context.
fn place_uh(ctx: &KThreadCtx) {
    let shared = ctx.shared();
    let tid = ROLE_TID[R_UH].load(Ordering::Acquire);
    let placed = shared.enqueue_on_cpu_split(CpuId(1), tid);
    record(
        K_USER,
        ctx.home().0 as u64,
        [
            R_UH as u64,
            tid,
            1,
            placed.map_or(u64::MAX, |c| c.0 as u64),
            mbx_read(1, M_HB),
        ],
    );
    set_phase(P_E_RUN);
}

/// K, phase F: chunks of work, a scheduling point after each, until it has been parked by its own
/// exhaustion and resumed after its release `F_CYCLES` times.
fn k_work(ctx: &KThreadCtx) {
    let cpu = ctx.home().0 as u64;
    let tid = ctx.id().tid().0;
    let c = ctx
        .shared()
        .with_cpu_sched_split(ctx.home(), |v| {
            v.context_of(crate::kernel::ipc::ThreadId(tid))
        })
        .ok()
        .flatten();
    record(
        K_KSTART,
        cpu,
        [
            tid,
            c.map_or(0, |c| c.params().budget() << 32 | c.params().period()),
            c.map_or(u64::MAX, |c| c.bound_at()),
            c.map_or(0, |c| c.remaining()),
            phase(),
        ],
    );
    let mut chunks = 0u64;
    while chunks < K_MAX_CHUNKS {
        let mut x = chunks;
        for _ in 0..K_CHUNK {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        chunks += 1;
        K_PROGRESS.store(chunks, Ordering::Release);
        if let Ok(true) = ctx.sched_point() {
            let rejoins = F_REJOINS.load(Ordering::Acquire);
            if active() {
                record(
                    K_KRESUMED,
                    cpu,
                    [tid, chunks, phase(), rejoins, mbx_read(0, M_HB)],
                );
            }
            if rejoins >= F_CYCLES {
                break;
            }
        }
    }
    record(
        K_KEND,
        cpu,
        [tid, chunks, F_REJOINS.load(Ordering::Acquire), 0, 0],
    );
    if phase() == P_F_RUN {
        set_phase(P_DONE);
    }
}

/// Which phase-E/F subject `tid` is, and its own and its lower-priority neighbour's heartbeats.
fn budget_subject(tid: u64) -> Option<(usize, u64)> {
    let p = phase();
    let hb = |own: u64, lower: u64| (own & 0xFFFF_FFFF) << 32 | (lower & 0xFFFF_FFFF);
    if tid == ROLE_TID[R_UH].load(Ordering::Acquire) && (p == P_E_PLACE || p == P_E_RUN) {
        Some((R_UH, hb(mbx_read(2, M_HB), mbx_read(1, M_HB))))
    } else if tid != 0
        && tid == ROLE_TID[R_K].load(Ordering::Acquire)
        && (p == P_F_ARM || p == P_F_RUN)
    {
        Some((
            R_K,
            hb(K_PROGRESS.load(Ordering::Acquire), mbx_read(0, M_HB)),
        ))
    } else {
        None
    }
}

/// Scheduler hook (1L held; lock-free here): `c` was bound at `now` in `slot`.
pub(crate) fn note_budget_bound(
    now: u64,
    slot: usize,
    c: &crate::kernel::sched_budget::SchedContext,
) {
    if let Some((_, hb)) = budget_subject(c.tid) {
        let p = c.params();
        record(
            K_BIND,
            0,
            [c.tid, slot as u64, p.budget() << 32 | p.period(), now, hb],
        );
    }
}

/// Scheduler hook (1L held): a charge on `cpu` exhausted `c` or ran it into debt.
pub(crate) fn note_budget_exhausted(
    cpu: u8,
    c: &crate::kernel::sched_budget::SchedContext,
    now: u64,
    overrun: u64,
) {
    if let Some((role, hb)) = budget_subject(c.tid) {
        if role == R_UH && E_REJOINS.load(Ordering::Acquire) >= E_CYCLES {
            E_RAN_AFTER.store(true, Ordering::Release);
        }
        record(
            K_EXHAUST,
            cpu as u64,
            [
                c.tid,
                now,
                overrun << 32 | (c.remaining() & 0xFFFF_FFFF),
                c.debt(),
                hb,
            ],
        );
    }
}

/// Scheduler hook (1L held): exhausted `c` lost its queue position at `now`.
pub(crate) fn note_budget_parked(now: u64, c: &crate::kernel::sched_budget::SchedContext) {
    if let Some((_, hb)) = budget_subject(c.tid) {
        let (kind, level) = match c.parked {
            Some((crate::kernel::sched_budget::Park::Reserved, l)) => (1u64, l as u64),
            Some((crate::kernel::sched_budget::Park::KernelThread, l)) => (2, l as u64),
            None => (0, 0),
        };
        record(
            K_PARK,
            0,
            [
                c.tid,
                now,
                kind << 8 | level,
                c.debt() << 32 | (c.remaining() & 0xFFFF_FFFF),
                hb,
            ],
        );
    }
}

/// Scheduler hook (1L held): `count` releases of `c` were due on `cpu` at `now`, the last at
/// `release`; `rejoined` — it was parked and now has budget, so it joined its level's tail.
pub(crate) fn note_budget_replenished(
    cpu: u8,
    c: &crate::kernel::sched_budget::SchedContext,
    now: u64,
    release: u64,
    count: u64,
    rejoined: bool,
) {
    if let Some((role, hb)) = budget_subject(c.tid) {
        if rejoined {
            match role {
                R_UH => E_REJOINS.fetch_add(1, Ordering::AcqRel),
                _ => F_REJOINS.fetch_add(1, Ordering::AcqRel),
            };
        }
        record(
            K_REPLENISH,
            cpu as u64,
            [
                c.tid,
                now,
                release,
                count << 32 | (rejoined as u64) << 16 | (c.remaining() & 0xFFFF),
                hb,
            ],
        );
    }
}

// ─────────────────────────────── hooks from the production owners ───────────────────────────────

/// A committed RT preemption (`settle_rt_reschedule`, cause 1, or the preempting tick, cause 2).
pub(crate) fn note_preempt(_shared: &SharedKernel, cpu: CpuId, outgoing: u64, cause: u64) {
    if active() {
        record(K_PREEMPT, cpu.0 as u64, [outgoing, cause, phase(), 0, 0]);
    }
}

/// A reschedule arrival the RT scheduling point declined (nothing strictly higher queued).
pub(crate) fn note_declined(cpu: CpuId, current: u64, cur_level: u64, best_level: u64, cause: u64) {
    if active() {
        record(
            K_DECLINED,
            cpu.0 as u64,
            [current, cur_level, best_level, cause, phase()],
        );
    }
}

/// A kernel thread's scheduling point consumed a pending reschedule: `rt` = `None` (GP),
/// `Some(false)` (kept the CPU), `Some(true)` (yields to the head of its level).
pub(crate) fn note_sched_point(cpu: CpuId, kthread: u64, rt: Option<bool>) {
    if active() {
        let code = match rt {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        };
        record(K_SCHEDPT, cpu.0 as u64, [kthread, code, phase(), 0, 0]);
    }
}

/// THE DRIVER — every accepted timer interrupt, AFTER its tick decision and with no lock held:
/// `current` is the task it landed on (`kthread`: a kernel thread; `u64::MAX`/0: none). Only CPU
/// 0's non-preempting ticks act; CPU 1's are counted in phase C.
pub(crate) fn on_timer(
    shared: &SharedKernel,
    cpu: CpuId,
    current: u64,
    preempting: bool,
    kthread: bool,
) {
    let p = phase();
    if p == P_OFF || p >= P_DUMPING {
        return;
    }
    if cpu.0 == 1 {
        if p == P_C_WAIT || p == P_C_AFTER {
            record(
                K_TICK,
                1,
                [current, preempting as u64, kthread as u64, p, 0],
            );
            if current == ROLE_TID[R_UB].load(Ordering::Acquire) {
                C_TICKS.fetch_add(1, Ordering::AcqRel);
            }
        }
        return;
    }
    if cpu.0 != 0 {
        return;
    }
    // The per-phase deadline: a phase that never completes is recorded, and the witness dumps.
    if PHASE_SEEN.swap(p, Ordering::AcqRel) != p {
        PHASE_TICKS.store(0, Ordering::Release);
    } else if PHASE_TICKS.fetch_add(1, Ordering::AcqRel) + 1 > PHASE_TICK_LIMIT && p < P_DONE {
        record(K_TIMEOUT, 0, [p, current, 0, 0, 0]);
        set_phase(P_DONE);
        return;
    }
    if preempting {
        return;
    }
    let ua = ROLE_TID[R_UA].load(Ordering::Acquire);
    let ub = ROLE_TID[R_UB].load(Ordering::Acquire);
    let on_ua = !kthread && current == ua;
    match p {
        P_WAIT_IDLE => {
            let idle = !kthread && (current == 0 || current == u64::MAX);
            let cpu1 = shared.sched_domain().topo().dispatching() & 0b10 != 0;
            if idle && cpu1 {
                go(R_S);
            }
        }
        P_WARMUP => {
            if mbx_read(0, M_HB) > 0 && mbx_read(1, M_HB) > 0 {
                set_phase(P_A_ARM);
            }
        }
        P_A_ARM if on_ua => {
            let round = A_ROUND.fetch_add(1, Ordering::AcqRel) + 1;
            set_phase(P_A_WAIT);
            record(
                K_WAKE,
                0,
                [P_A_WAIT, R_H0 as u64, current, round, mbx_read(0, M_YIELDS)],
            );
            go(R_H0);
        }
        P_B_ARM | P_C_ARM => {
            // UB observed running on CPU 1: current there, and its heartbeat moved since the
            // last look.
            let hb = mbx_read(1, M_HB);
            let moved = hb > LAST_UB_HB.swap(hb, Ordering::AcqRel);
            let c1 = shared.current_tid_split_read(CpuId(1));
            if moved && c1 == Some(ub) {
                let (next, role) = if p == P_B_ARM {
                    (P_B_WAIT, R_H1)
                } else {
                    C_TICKS.store(0, Ordering::Release);
                    (P_C_WAIT, R_E)
                };
                set_phase(next);
                record(
                    K_WAKE,
                    0,
                    [next, role as u64, ub, hb, mbx_read(1, M_YIELDS)],
                );
                go(role);
            }
        }
        P_C_WAIT => {
            // With E queued behind it, UB is preempted by a strictly higher wake (H1): it must
            // return to the HEAD of level 20, ahead of E (SC-PRI-12 (a)).
            let c1 = shared.current_tid_split_read(CpuId(1));
            if C_TICKS.load(Ordering::Acquire) >= C_TICKS_BEFORE && c1 == Some(ub) {
                set_phase(P_C_H1);
                record(
                    K_WAKE,
                    0,
                    [
                        P_C_H1,
                        R_H1 as u64,
                        ub,
                        mbx_read(1, M_HB),
                        mbx_read(1, M_YIELDS),
                    ],
                );
                go(R_H1);
            }
        }
        P_C_AFTER => {
            if C_TICKS.load(Ordering::Acquire) >= C_TICKS_MIN {
                let yields = mbx_read(1, M_YIELDS);
                set_phase(P_C_YIELD);
                record(
                    K_YREQ,
                    0,
                    [ub, yields, C_TICKS.load(Ordering::Acquire), 0, 0],
                );
                mbx_write(1, M_YREQ, mbx_read(1, M_YREQ) + 1);
            }
        }
        P_D_ARM if on_ua => {
            set_phase(P_D_L);
            record(
                K_WAKE,
                0,
                [P_D_L, R_L as u64, current, 0, mbx_read(0, M_YIELDS)],
            );
            go(R_L);
        }
        P_D_L
            if kthread
                && L_STARTED.load(Ordering::Acquire)
                && current == ROLE_TID[R_L].load(Ordering::Acquire) =>
        {
            set_phase(P_D_WAIT);
            record(K_WAKE, 0, [P_D_WAIT, R_H0 as u64, current, 0, 0]);
            go(R_H0);
        }
        P_E_ARM => {
            // As B: UB observed running on CPU 1, then UH is placed there from CPU 0.
            let hb = mbx_read(1, M_HB);
            let moved = hb > LAST_UB_HB.swap(hb, Ordering::AcqRel);
            if moved && shared.current_tid_split_read(CpuId(1)) == Some(ub) {
                set_phase(P_E_PLACE);
                record(
                    K_WAKE,
                    0,
                    [P_E_PLACE, R_UH as u64, ub, hb, mbx_read(1, M_YIELDS)],
                );
                go(R_S);
            }
        }
        P_E_RUN if E_RAN_AFTER.load(Ordering::Acquire) => set_phase(P_F_ARM),
        P_F_ARM => {
            set_phase(P_F_RUN);
            record(K_WAKE, 0, [P_F_RUN, R_K as u64, current, 0, 0]);
            go(R_K);
        }
        P_DONE => {
            // Quiet the user tasks (no more yields), then dump.
            mbx_write(0, M_STORM, 0);
            set_phase(P_DUMPING);
            go(R_DUMP);
        }
        _ => {}
    }
}

// ─────────────────────────────── the dump ───────────────────────────────

fn fnv(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for b in bytes {
        h = (h ^ u32::from(*b)).wrapping_mul(0x0100_0193);
    }
    h
}

struct Line {
    buf: [u8; 224],
    len: usize,
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        Ok(())
    }
}

fn emit(line: &Line) -> u32 {
    let body = &line.buf[..line.len];
    let crc = fnv(body);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "{} crc=0x{:08x}",
        core::str::from_utf8(body).unwrap_or("RTW_INVALID"),
        crc
    ));
    crc
}

/// Every record in stamp order, then the identities and a seal — the whole dump twice, so a line
/// damaged on the shared console in one copy survives in the other.
fn dump(ctx: &KThreadCtx) {
    use core::fmt::Write;
    if BUDGETED.load(Ordering::Acquire) {
        let shared = ctx.shared();
        for cpu in 0..2u8 {
            if let Ok((idle, unaccounted, ticks, arrivals, bound)) =
                shared.with_cpu_sched_split(CpuId(cpu), |v| v.budget_ledger())
            {
                record(
                    K_LEDGER,
                    cpu as u64,
                    [idle, unaccounted, ticks, arrivals, bound as u64],
                );
            }
        }
        for role in [R_UH, R_K] {
            let tid = ROLE_TID[role].load(Ordering::Acquire);
            let c = shared
                .with_cpu_sched_split(CpuId(plan(role).0), |v| {
                    v.context_of(crate::kernel::ipc::ThreadId(tid))
                })
                .ok()
                .flatten();
            if let Some(c) = c {
                record(
                    K_CTX,
                    plan(role).0 as u64,
                    [
                        tid,
                        c.charged(),
                        c.overrun(),
                        (c.exhaustions() as u64) << 32 | (c.releases() & 0xFFFF_FFFF),
                        c.debt() << 32 | (c.remaining() & 0xFFFF_FFFF),
                    ],
                );
            }
        }
    }
    let total = SEQ.load(Ordering::Acquire).min(MAX_RECS as u64);
    for pass in 0..2u64 {
        let mut seal: u32 = 0x811C_9DC5;
        let mut emitted = 0u64;
        for role in 0..ROLES {
            let mut line = Line {
                buf: [0; 224],
                len: 0,
            };
            let (home, level) = plan(role);
            let _ = write!(
                line,
                "RTW_ROLE pass={} role={} tid=0x{:x} home={} level={}",
                pass,
                ROLE_NAMES[role],
                ROLE_TID[role].load(Ordering::Acquire),
                home,
                level
            );
            seal = (seal ^ emit(&line)).wrapping_mul(0x0100_0193);
        }
        for seq in 0..total {
            let r = &RECS[seq as usize];
            if !r.valid.load(Ordering::Acquire) {
                continue;
            }
            let kind = r.kind.load(Ordering::Relaxed) as usize;
            let mut line = Line {
                buf: [0; 224],
                len: 0,
            };
            let v: [u64; 5] = core::array::from_fn(|i| r.v[i].load(Ordering::Relaxed));
            let _ = write!(
                line,
                "RTW_REC pass={} seq={} kind={} cpu={} v0=0x{:x} v1=0x{:x} v2=0x{:x} v3=0x{:x} v4=0x{:x}",
                pass,
                seq,
                KIND_NAMES.get(kind).copied().unwrap_or("UNKNOWN"),
                r.cpu.load(Ordering::Relaxed),
                v[0],
                v[1],
                v[2],
                v[3],
                v[4]
            );
            seal = (seal ^ emit(&line)).wrapping_mul(0x0100_0193);
            emitted += 1;
        }
        let mut line = Line {
            buf: [0; 224],
            len: 0,
        };
        let _ = write!(
            line,
            "RTW_SEAL pass={} records={} taken={} overflow={} final_phase={} seal=0x{:08x}",
            pass,
            emitted,
            SEQ.load(Ordering::Acquire),
            OVERFLOW.load(Ordering::Acquire),
            PHASE_NAMES[phase().min(P_DUMPING) as usize],
            seal
        );
        emit(&line);
    }
    crate::kernel::printk::printk_emit_sync(format_args!("RTW_DONE"));
}
