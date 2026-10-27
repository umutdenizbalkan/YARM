// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! AP-SCHED1/2/3/4 — the production SMP-scheduling witness, for RISC-V, AArch64 and x86_64. Built
//! only with `riscv64-apsched-witness` / `aarch64-apsched-witness` / `x86-apsched-witness`, and
//! armed only WITHOUT
//! `yarm.ap_user_dispatch=1`: it exercises the production admission of EVERY admitted secondary,
//! never a diagnostic pinned one, and runs ordinary work on each of them (AP-SCHED2 generalized it
//! from CPU 1 alone; AP-SCHED3 moved this port-neutral half here from `arch::riscv64` and gave
//! AArch64 its own program; AP-SCHED4 gave x86_64 its own, and the APIC ID each CPU was admitted
//! under). The hooks that differ by port are the program
//! (`arch/<port>/apsched_witness.S`) and what a root departure leaves behind
//! ([`note_root_left`]).
//!
//! It provisions ONE App-class process P (`apsched_witness.S`) and nothing else. P's workers are
//! threads it spawns itself (NR 11) and children it forks (NR 12); the production balanced
//! placement decides where each runs and fixes that CPU as its home. The witness pins nothing and
//! fixes no identity but P's.
//!
//! Its kernel actions are, all labelled:
//!
//! * setup (P's space, pages, three buffered endpoints, P's capabilities) and one ordinary enqueue
//!   of P, at boot, when CPU 0 is the only CPU placement can choose;
//! * PUBLICATION of observation words into P's mailbox frame through the direct map, with no lock
//!   held: the released set, each CPU's admission, this process's timer preemptions per CPU, each
//!   secondary's idle state and the last reaped child — so P can wait for a fact instead of
//!   guessing with a delay;
//! * the REAP of each churn child, through the production NR 31 transaction on the CPU P's
//!   request trapped on. Production's only reaper is the process manager, for supervisor restarts;
//!   the witness stands in for it so an App process's exit and reap can be exercised at all.
//!
//! Every record carries one causal stamp from a single counter, so the grader orders events by
//! stamp and never by console position (the two harts' output interleaves).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::scheduler::{CpuId, MAX_CPUS};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr};
use crate::runtime::SharedKernel;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(include_str!("../arch/riscv64/apsched_witness.S"));
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("../arch/aarch64/apsched_witness.S"));
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("../arch/x86_64/apsched_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const P_STACK_VA: u64 = 0x2001_0000;
pub const P_SP_TOP: u64 = 0x2001_0ff0;
pub const WSTK_VA: u64 = 0x2002_0000;
pub const WSTK_SLOTS: u64 = 48;
pub const MBX_VA: u64 = 0x2007_0000;
pub const BUF_VA: u64 = 0x2008_0000;
pub const P_TID: u64 = 9_600;

const M_AP_READY: u64 = 0x000;
const M_EXPECTED: u64 = 0x020;
const M_REAPED: u64 = 0x038;
const M_PRE: u64 = 0x100;
const M_IDLE: u64 = 0x140;
/// CPU 0's timer interrupts — the global tick's source — that P's bounded waits measure their
/// deadlines against. CPU 0's timer is never the one a control breaks.
const M_CLOCK: u64 = 0x060;
/// The CPUs a spinner has started on, as the kernel observed each `W_START` (AArch64 P tops its
/// spinners up from it; the RISC-V program does not read it).
const M_W_CPUS: u64 = 0x068;
static W_CPUS: AtomicU64 = AtomicU64::new(0);
/// The CPUs the mailbox has per-CPU words for (the 8-slot handoff table's maximum).
const MBX_CPUS: usize = 8;

const MAX_CHILDREN: usize = 64;
/// Concurrent deadline windows (one per secondary).
const MAX_DEADLINES: usize = 8;

unsafe extern "C" {
    static yarm_apsw_start: u8;
    static yarm_apsw_end: u8;
    static yarm_apsw_cap_ep_p: u8;
    static yarm_apsw_cap_ep_t: u8;
    static yarm_apsw_cap_ep_d: u8;
    /// AArch64 and x86_64: P's private nap endpoint (its bounded waits block on it a tick at a
    /// time).
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    static yarm_apsw_cap_ep_n: u8;
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(0);
static MBX_PHYS: AtomicU64 = AtomicU64::new(0);
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Accepted timer interrupts per CPU, counted at the timer settlement's entry — before, and
/// independently of, whatever the scheduler does with them.
static IRQS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static PRE: [AtomicU64; MBX_CPUS] = [const { AtomicU64::new(0) }; MBX_CPUS];
static IDLES: [AtomicU64; MBX_CPUS] = [const { AtomicU64::new(0) }; MBX_CPUS];
static CHILDREN: [AtomicU64; MAX_CHILDREN] = [const { AtomicU64::new(0) }; MAX_CHILDREN];
static REAPED: [AtomicBool; MAX_CHILDREN] = [const { AtomicBool::new(false) }; MAX_CHILDREN];
/// The address spaces churn children reported from (`C_RUN`): only their root departures are
/// recorded.
static CHILD_ASIDS: [AtomicU64; MAX_CHILDREN] = [const { AtomicU64::new(0) }; MAX_CHILDREN];
static REFUSALS: AtomicU64 = AtomicU64::new(0);
/// Each deadline thread's window: (tid, tick, CPU 0 interrupts, its own CPU's interrupts) at
/// D_BEFORE.
static D_WIN: [[AtomicU64; 4]; MAX_DEADLINES] =
    [const { [const { AtomicU64::new(0) }; 4] }; MAX_DEADLINES];
static ENDED: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn stamp() -> u64 {
    SEQ.fetch_add(1, Ordering::AcqRel) + 1
}

fn witness_asid() -> u64 {
    ASID.load(Ordering::Acquire)
}

/// Is `tid` P or one of P's threads (they share P's space)?
fn is_process_task(shared: &SharedKernel, tid: u64) -> bool {
    tid != 0 && shared.task_asid_for_tid_split_read(tid) == witness_asid()
}

fn child_slot(tid: u64) -> Option<usize> {
    (0..MAX_CHILDREN).find(|&i| CHILDREN[i].load(Ordering::Acquire) == tid)
}

fn is_witness_task(shared: &SharedKernel, tid: u64) -> bool {
    is_process_task(shared, tid) || (tid != 0 && child_slot(tid).is_some())
}

/// WITNESS PUBLICATION — store one observation word into P's mailbox frame through the direct
/// map. No lock is taken or held; P only ever reads these words.
fn publish(off: u64, value: u64) {
    let phys = MBX_PHYS.load(Ordering::Acquire);
    if phys == 0 {
        return;
    }
    if let Some(ptr) = KernelState::phys_to_direct_map_ptr(phys + off) {
        // SAFETY: an aligned u64 inside the mailbox frame this witness allocated; the direct map
        // covers it for the life of the kernel.
        unsafe { core::ptr::write_volatile(ptr.cast::<u64>(), value) };
    }
}

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

fn cap32(c: CapId) -> Result<u32, KernelError> {
    u32::try_from(c.0).map_err(|_| KernelError::CapabilityFull)
}

/// Provision P from the bootstrap window (`&mut KernelState`): its space, its pages, its three
/// endpoints, and ONE ordinary enqueue — at boot, CPU 0 is the only CPU placement can choose.
pub fn provision(kernel: &mut KernelState) -> Result<(), KernelError> {
    let (asid, _as_root) = kernel.create_user_address_space()?;
    // SAFETY (all `&raw const` below): taking the address of an extern static reads nothing.
    let start = &raw const yarm_apsw_start;
    let len = &raw const yarm_apsw_end as usize - start as usize;
    map_fresh(kernel, asid, CODE_VA, (len as u64).div_ceil(0x1000))?;
    map_fresh(kernel, asid, P_STACK_VA, 1)?;
    map_fresh(kernel, asid, WSTK_VA, WSTK_SLOTS)?;
    let mbx = map_fresh(kernel, asid, MBX_VA, 1)?;
    map_fresh(kernel, asid, BUF_VA, 1)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;
    kernel.copy_to_user(asid, VirtAddr(BUF_VA), &[0u8; 0x1000])?;

    kernel.register_task_with_class(P_TID, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == P_TID)
            .ok_or(KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(P_SP_TOP);
        Ok::<_, KernelError>(())
    })?;

    // Three buffered endpoints (AArch64: four), each minted into P's own CNode with both rights:
    // P's threads share it.
    let rights = CapRights::SEND | CapRights::RECEIVE;
    let grant = |k: &mut KernelState| -> Result<u32, KernelError> {
        let (_, _, cap) = k.create_endpoint(1)?;
        let object = k
            .current_task_capability(cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let cnode = k.task_cnode(P_TID).ok_or(KernelError::TaskMissing)?;
        k.mint_capability_in_cnode(
            cnode,
            crate::kernel::capabilities::Capability::new(object, rights),
        )
        .and_then(cap32)
    };
    let ep_p = grant(kernel)?;
    let ep_t = grant(kernel)?;
    let ep_d = grant(kernel)?;

    // SAFETY: `start..start + len` is the program's bytes inside this kernel image's text.
    let mut image = unsafe { core::slice::from_raw_parts(start, len) }.to_vec();
    #[cfg_attr(target_arch = "riscv64", allow(unused_mut))]
    let mut caps = alloc::vec![
        (&raw const yarm_apsw_cap_ep_p, ep_p),
        (&raw const yarm_apsw_cap_ep_t, ep_t),
        (&raw const yarm_apsw_cap_ep_d, ep_d),
    ];
    // AArch64 and x86_64: a fourth endpoint nothing sends to, for P's one-tick naps.
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    caps.push((&raw const yarm_apsw_cap_ep_n, grant(kernel)?));
    for (at, value) in caps {
        let off = at as usize - start as usize;
        image[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }
    kernel.copy_to_user(asid, VirtAddr(CODE_VA), &image)?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    MBX_PHYS.store(mbx, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    kernel.enqueue_task(P_TID)?;
    crate::kernel::printk::printk_emit_sync(format_args!(
        "APSW_PROVISIONED p_tid={} asid={} image={} ep_p={} ep_t={} ep_d={} placement=balanced pinned=0",
        P_TID,
        asid.0,
        image.len(),
        ep_p,
        ep_t,
        ep_d
    ));
    Ok(())
}

// ─────────────────────────────── the record ───────────────────────────────

/// The record is kept, not printed as it happens: the two harts' console output interleaves
/// character by character, and a record torn by the other hart's marker would be lost evidence.
/// P's last step dumps it once, every line twice with a CRC, while nothing else of the witness
/// runs; the grader keeps exactly the lines whose CRC verifies.
const MAX_RECS: usize = 4096;
/// Preemption records kept per CPU beyond each task's first (the full count is in `PRE` and the
/// dump trailer): at 8 CPUs the spinners take ~300 preemptions each, which would flood the record.
/// Each witness task's first this-many timer preemptions are recorded, on whichever CPU (the
/// per-CPU totals still reach the mailbox, unbounded). The budget is per TASK: a per-CPU budget (48)
/// let one task that was preempted many times before the spinners started — a deadline thread
/// spinning on its gate while P was still spawning, slow under ten emulated harts — use up a CPU's
/// records, so the spinners that followed it there kept only their first, and the grade read the
/// missing records as missing preemptions.
const PRE_RECS_PER_TASK: u64 = 6;
/// Witness tasks the budget can track (P's workers and its churn children over a full run).
const PRE_TASKS: usize = 128;

const K_AP_ADMITTED: u64 = 1;
const K_PREEMPT: u64 = 2;
const K_IDLE: u64 = 3;
const K_IDLE_IPI: u64 = 4;
const K_IDLE_DISPATCH: u64 = 5;
const K_USER: u64 = 6;
const K_DEADLINE: u64 = 7;
const K_REAP: u64 = 8;
const K_REAP_GONE: u64 = 9;
const K_ROOT_LEFT: u64 = 10;
const K_RELEASE: u64 = 11;
/// A reschedule IPI taken on a secondary OUTSIDE its idle boundary (AArch64: in EL0 or kernel code).
const K_SGI: u64 = 12;

const KINDS: [&str; 13] = [
    "NONE",
    "AP_ADMITTED",
    "PREEMPT",
    "IDLE",
    "IDLE_IPI",
    "IDLE_DISPATCH",
    "USER",
    "DEADLINE",
    "REAP",
    "REAP_GONE",
    "ROOT_LEFT",
    "RELEASE",
    "SGI",
];

/// The user steps, by code (index + 1); 0 = a step this table does not know.
const STEPS: [&str; 32] = [
    "P_START",
    "P_NO_AP",
    "P_AP_READY",
    "P_PRE_TIMEOUT",
    "P_A_TIMEOUT",
    "P_A_DONE",
    "P_B_TIMEOUT",
    "P_NO_IDLE",
    "P_WAKE",
    "P_NO_ACK",
    "P_ACKED",
    "P_PING",
    "P_PING_RAW",
    "P_C_TIMEOUT",
    "P_FORKED",
    "P_REAP_TIMEOUT",
    "P_END",
    "P_SPAWN_FAIL",
    "P_FORK_FAIL",
    "C_RUN",
    "D_BEFORE",
    "W_START",
    "W_DONE",
    "S_START",
    "S_WOKE",
    "T_START",
    "T_PONG",
    "EXIT_REFUSED",
    "D_AFTER",
    "P_REAP",
    "P_W_TIMEOUT",
    "P_W_COVER",
];

fn step_code(step: &str) -> u64 {
    STEPS
        .iter()
        .position(|s| *s == step)
        .map_or(0, |i| i as u64 + 1)
}

struct Rec {
    valid: AtomicBool,
    kind: AtomicU64,
    cpu: AtomicU64,
    tid: AtomicU64,
    step: AtomicU64,
    v: [AtomicU64; 5],
}

static RECS: [Rec; MAX_RECS] = [const {
    Rec {
        valid: AtomicBool::new(false),
        kind: AtomicU64::new(0),
        cpu: AtomicU64::new(0),
        tid: AtomicU64::new(0),
        step: AtomicU64::new(0),
        v: [const { AtomicU64::new(0) }; 5],
    }
}; MAX_RECS];
static OVERFLOW: AtomicBool = AtomicBool::new(false);

/// Append one record under the next causal stamp. Lock-free: each stamp owns one slot.
fn record(kind: u64, cpu: u64, tid: u64, step: u64, v: [u64; 5]) {
    let seq = stamp();
    let Some(r) = RECS.get(seq as usize - 1) else {
        OVERFLOW.store(true, Ordering::Release);
        return;
    };
    r.kind.store(kind, Ordering::Relaxed);
    r.cpu.store(cpu, Ordering::Relaxed);
    r.tid.store(tid, Ordering::Relaxed);
    r.step.store(step, Ordering::Relaxed);
    for (slot, value) in r.v.iter().zip(v) {
        slot.store(value, Ordering::Relaxed);
    }
    r.valid.store(true, Ordering::Release);
}

/// The boot hart released `released` (of the trap-ready `trap_ready`) at the end of its boot
/// borrow: the set P's run must cover.
pub fn note_release_plan(released: u64, trap_ready: u64) {
    if !enabled() {
        return;
    }
    record(K_RELEASE, 0, 0, 0, [released, trap_ready, 0, 0, 0]);
    publish(M_EXPECTED, released);
}

static ADMITTED: AtomicU64 = AtomicU64::new(0);

/// CPU `cpu` completed its production admission (called last, by that CPU).
pub fn ap_admitted(cpu: CpuId) {
    admitted_record(cpu, [0; 5]);
}

/// x86_64: as [`ap_admitted`], with the hardware identity (the APIC ID the CPU read from its own
/// LAPIC) it was admitted under, so the grader can check it against the firmware plan separately
/// from the logical id: `v0` = APIC ID, `v1` = 1 (identity recorded).
#[cfg(target_arch = "x86_64")]
pub fn ap_admitted_as(cpu: CpuId, hw_id: u64) {
    admitted_record(cpu, [hw_id, 1, 0, 0, 0]);
}

fn admitted_record(cpu: CpuId, v: [u64; 5]) {
    if !enabled() || cpu.0 as usize >= MBX_CPUS {
        return;
    }
    record(K_AP_ADMITTED, u64::from(cpu.0), 0, 0, v);
    let all = ADMITTED.fetch_or(1u64 << cpu.0, Ordering::AcqRel) | (1u64 << cpu.0);
    publish(M_AP_READY, all);
}

/// Every accepted timer interrupt, at the timer settlement's entry.
pub fn note_timer_irq(cpu: CpuId) {
    if !enabled() {
        return;
    }
    if cpu.0 == 0 {
        CLOCK_TICKING.store(true, Ordering::SeqCst);
    }
    if let Some(n) = IRQS.get(cpu.0 as usize) {
        let now = n.fetch_add(1, Ordering::SeqCst) + 1;
        if cpu.0 == 0 {
            publish(M_CLOCK, now);
        }
    }
}

/// CPU 0's tick advance for the interrupt [`note_timer_irq`] counted is complete.
pub fn note_timer_tick_done(cpu: CpuId) {
    if enabled() && cpu.0 == 0 {
        CLOCK_TICKING.store(false, Ordering::SeqCst);
    }
}

/// CPU 0 is between counting a timer interrupt and advancing the tick for it.
static CLOCK_TICKING: AtomicBool = AtomicBool::new(false);

/// One consistent `(tick, CPU 0 timer interrupts)` pair, read from any CPU. The two are separate
/// words, so a plain pair of loads can split one CPU 0 interrupt across a window edge (seen once
/// at 8 harts: 41 ticks over 42 interrupts). Retried until no CPU 0 interrupt was in flight and
/// the count did not move; bounded, after which the last pair is returned.
fn clock_snapshot(shared: &SharedKernel) -> (u64, u64) {
    let mut pair = (0, 0);
    for _ in 0..100_000 {
        let i1 = IRQS[0].load(Ordering::SeqCst);
        let busy1 = CLOCK_TICKING.load(Ordering::SeqCst);
        let tick = shared.scheduler_tick_now_split_read();
        let busy2 = CLOCK_TICKING.load(Ordering::SeqCst);
        let i2 = IRQS[0].load(Ordering::SeqCst);
        pair = (tick, i2);
        if i1 == i2 && !busy1 && !busy2 {
            break;
        }
        core::hint::spin_loop();
    }
    pair
}

/// A timer preemption the settlement COMMITTED, with its outgoing task.
pub fn note_preempt(shared: &SharedKernel, cpu: CpuId, outgoing: u64) {
    // P itself waits in yield loops, so it is running at most ticks; only its workers count.
    let c = cpu.0 as usize;
    if !enabled() || c >= MBX_CPUS || outgoing == P_TID || !is_witness_task(shared, outgoing) {
        return;
    }
    let n = PRE[c].fetch_add(1, Ordering::AcqRel) + 1;
    // Each task's first PRE_RECS_PER_TASK preemptions are recorded (the grader needs one per
    // spinner, and PRE_MIN per CPU across its spinners).
    if task_preemptions(outgoing) <= PRE_RECS_PER_TASK {
        record(K_PREEMPT, c as u64, outgoing, 0, [n, 0, 0, 0, 0]);
    }
    // SCHED-LOCK1 witness (default-off): labelled fixture synchronization — phase A continues
    // until the independence rounds are done (`schedlock1_witness::holds_phase_a`).
    #[cfg(feature = "sched-lock1-witness")]
    if crate::kernel::schedlock1_witness::holds_phase_a() {
        return;
    }
    publish(M_PRE + 8 * c as u64, n);
}

/// The CPUs on which a phase-A spinner has reported running.
#[cfg(feature = "sched-lock1-witness")]
pub(crate) fn spinner_cpus() -> u64 {
    W_CPUS.load(Ordering::Acquire)
}

/// The tasks preempted so far, and how many times each (index-paired).
static PRE_SEEN: [AtomicU64; PRE_TASKS] = [const { AtomicU64::new(0) }; PRE_TASKS];
static PRE_COUNT: [AtomicU64; PRE_TASKS] = [const { AtomicU64::new(0) }; PRE_TASKS];

/// This preemption's ordinal for `tid` (1 for its first), or `u64::MAX` once the table is full.
/// Only the tick path of the CPU running `tid` calls it, so a task never races itself for a slot.
fn task_preemptions(tid: u64) -> u64 {
    let slot = PRE_SEEN
        .iter()
        .position(|t| t.load(Ordering::Acquire) == tid)
        .or_else(|| {
            PRE_SEEN.iter().position(|t| {
                t.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        });
    slot.map_or(u64::MAX, |i| {
        PRE_COUNT[i].fetch_add(1, Ordering::AcqRel) + 1
    })
}

/// `cpu` arrived at its idle wait from a task (not a return to an interrupted idle).
/// Secondaries only: CPU 0 also runs the core services, whose idle arrivals are not this witness's.
pub fn note_idle_reached(cpu: usize) {
    if !enabled() || cpu == 0 || cpu >= MBX_CPUS {
        return;
    }
    #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
    x86_late_ipi::check(cpu);
    let n = IDLES[cpu].fetch_add(1, Ordering::AcqRel) + 1;
    record(K_IDLE, cpu as u64, 0, 0, [n, 0, 0, 0, 0]);
    publish(M_IDLE + 8 * cpu as u64, 1);
}

/// `cpu` left a user root for the kernel's own space (`hal::leave_user_root`), after the hardware
/// change and the record release. Secondaries only, and only for a churn child's space (learned
/// from the child's own `C_RUN`, which precedes its exit): a child can run and exit before P has
/// even reported forking it, so the grader matches the root against the child's own step.
///
/// Recorded from what the transition LEFT BEHIND, not from the fact that it was called: the live
/// translation root (RISC-V: `satp` must be Bare, 0; AArch64: `TTBR0_EL1` must be the kernel's
/// own root, ASID 0) and the activation record (must name no space). A transition that did not
/// happen records nothing.
pub fn note_root_left(cpu: CpuId, leaving: Option<crate::kernel::vm::Asid>) {
    if !enabled() || cpu.0 == 0 {
        return;
    }
    let Some(asid) = leaving else {
        return;
    };
    let asid = u64::from(asid.0);
    if !CHILD_ASIDS
        .iter()
        .any(|a| a.load(Ordering::Acquire) == asid && asid != 0)
    {
        return;
    }
    #[cfg(target_arch = "riscv64")]
    let (root, left) = {
        let satp: usize;
        // SAFETY: a CSR read.
        unsafe {
            core::arch::asm!("csrr {0}, satp", out(reg) satp, options(nomem, nostack, preserves_flags))
        };
        (satp as u64, satp == 0)
    };
    #[cfg(target_arch = "aarch64")]
    let (root, left) = {
        let ttbr0: u64;
        // SAFETY: a system-register read.
        unsafe {
            core::arch::asm!("mrs {0}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags))
        };
        let (_, _, kernel_root) = crate::arch::aarch64::boot::bootstrap_mmu_registers();
        (ttbr0, ttbr0 == kernel_root)
    };
    #[cfg(target_arch = "x86_64")]
    let (root, left) = {
        let cr3: u64;
        // SAFETY: a control-register read.
        unsafe {
            core::arch::asm!("mov {0}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags))
        };
        (
            cr3,
            crate::arch::x86_64::descriptor_tables::is_kernel_root_cr3(cr3),
        )
    };
    if !left || crate::arch::hal::active_address_space(cpu).is_some() {
        return;
    }
    record(K_ROOT_LEFT, u64::from(cpu.0), 0, 0, [asid, root, 0, 0, 0]);
}

/// A reschedule IPI taken at `cpu`'s idle boundary, with the sources its consumption swapped out.
pub fn note_idle_ipi(cpu: CpuId, sources: u64) {
    if !enabled() || cpu.0 == 0 {
        return;
    }
    record(K_IDLE_IPI, u64::from(cpu.0), 0, 0, [sources, 0, 0, 0, 0]);
}

/// A reschedule IPI `cpu` took outside its idle boundary (`lane` 1 = from user mode, 3 = in kernel
/// code; on x86_64, 4 = observed by count at the next idle arrival). Lets the grader tell a wake
/// whose IPI was delivered but lost the race to the idle CPU's own timer from a wake whose IPI
/// never came. AArch64 records it at the SGI's arrival, RISC-V at a user-origin software
/// interrupt carrying a sender, x86_64 from its per-CPU arrival count (below).
pub fn note_sgi_elsewhere(cpu: CpuId, lane: u64) {
    if !enabled() || cpu.0 == 0 {
        return;
    }
    record(K_SGI, u64::from(cpu.0), 0, 0, [lane, 0, 0, 0, 0]);
}

/// `cpu` leaves idle to run `tid`, by the trigger that woke it.
pub fn note_idle_dispatch(shared: &SharedKernel, cpu: CpuId, tid: u64, by_ipi: bool) {
    if !enabled() {
        return;
    }
    if cpu.0 != 0 && (cpu.0 as usize) < MBX_CPUS {
        publish(M_IDLE + 8 * u64::from(cpu.0), 0);
    }
    // Secondaries only: CPU 0's idle exits are P's own naps (and the core services'), which no
    // obligation grades and which would only fill the record.
    if cpu.0 == 0 || !is_witness_task(shared, tid) {
        return;
    }
    record(
        K_IDLE_DISPATCH,
        u64::from(cpu.0),
        tid,
        0,
        [u64::from(by_ipi), 0, 0, 0, 0],
    );
    #[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
    if !by_ipi {
        x86_late_ipi::arm(cpu);
    }
}

/// x86_64: the reschedule IPI's stub is settled in assembly outside the idle boundary, so its late
/// arrival is observed by count: armed when a CPU leaves idle by its timer, checked at that CPU's
/// next idle arrival. Recorded only when the vector-0xF1 arrival count grew and the TLB
/// acknowledgement generation did not (the same vector also carries shootdowns), so a shootdown
/// cannot stand in for the reschedule IPI.
#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
mod x86_late_ipi {
    use super::{CpuId, MBX_CPUS};
    use core::sync::atomic::{AtomicU64, Ordering};
    static ARMED: [AtomicU64; MBX_CPUS] = [const { AtomicU64::new(0) }; MBX_CPUS];

    pub(super) fn arm(cpu: CpuId) {
        let Some(slot) = ARMED.get(cpu.0 as usize) else {
            return;
        };
        let r = crate::arch::x86_64::percpu::read_record(cpu);
        let packed =
            (1u64 << 63) | (u64::from(r.tlb_ack_gen) << 32) | u64::from(r.remote_wake_count);
        slot.store(packed, Ordering::Release);
    }

    pub(super) fn check(cpu: usize) {
        let Some(slot) = ARMED.get(cpu) else { return };
        let armed = slot.swap(0, Ordering::AcqRel);
        if armed == 0 {
            return;
        }
        let r = crate::arch::x86_64::percpu::read_record(CpuId(cpu as u8));
        let (ack0, wake0) = ((armed >> 32) as u32 & 0x7FFF_FFFF, armed as u32);
        if r.remote_wake_count != wake0 && (r.tlb_ack_gen & 0x7FFF_FFFF) == ack0 {
            super::note_sgi_elsewhere(CpuId(cpu as u8), 4);
        }
    }
}

/// The DebugLog observation point. Returns `true` when the message was this witness's.
pub fn observe_user_marker(
    shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    asid: u64,
    msg: &str,
    a2: u64,
    a3: u64,
) -> bool {
    if !enabled() || !msg.starts_with("APS ") {
        return false;
    }
    let step = msg.trim_end().trim_start_matches("APS ");
    let c = u64::from(cpu.0);
    match step {
        "P_FORKED" => {
            // A churn child: the reaper accepts only TIDs P reported here.
            let _ = (0..MAX_CHILDREN).find(|&i| {
                CHILDREN[i]
                    .compare_exchange(0, a2, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
        }
        // Polled: only the outcome is recorded, not every attempt.
        "P_REAP" => {
            reap(shared, cpu, a2, a3);
            return true;
        }
        "C_RUN" => {
            let _ = (0..MAX_CHILDREN).find(|&i| {
                CHILD_ASIDS[i]
                    .compare_exchange(0, asid, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            });
        }
        "W_START" => {
            let bit = 1u64 << (c & 63);
            publish(M_W_CPUS, W_CPUS.fetch_or(bit, Ordering::AcqRel) | bit);
        }
        "D_BEFORE" => {
            // One window per deadline thread, keyed by its TID.
            if let Some(w) = D_WIN.iter().find(|w| {
                w[0].compare_exchange(0, tid, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }) {
                let (tick, irq0) = clock_snapshot(shared);
                w[1].store(tick, Ordering::Release);
                w[2].store(irq0, Ordering::Release);
                w[3].store(
                    IRQS.get(c as usize)
                        .map_or(0, |n| n.load(Ordering::Acquire)),
                    Ordering::Release,
                );
            }
        }
        "D_AFTER" => {
            let (tick, irq0) = clock_snapshot(shared);
            if let Some(w) = D_WIN.iter().find(|w| w[0].load(Ordering::Acquire) == tid) {
                let own = IRQS
                    .get(c as usize)
                    .map_or(0, |n| n.load(Ordering::Acquire));
                record(
                    K_DEADLINE,
                    c,
                    tid,
                    0,
                    [
                        a2,
                        a3,
                        tick.wrapping_sub(w[1].load(Ordering::Acquire)),
                        irq0.wrapping_sub(w[2].load(Ordering::Acquire)),
                        own.wrapping_sub(w[3].load(Ordering::Acquire)),
                    ],
                );
            }
            return true;
        }
        _ => {}
    }
    record(K_USER, c, tid, step_code(step), [a2, a3, asid, 0, 0]);
    if step == "P_END" && !ENDED.swap(true, Ordering::AcqRel) {
        #[cfg(feature = "sched-lock1-witness")]
        crate::kernel::schedlock1_witness::close_at_end(shared);
        dump();
    }
    true
}

/// WITNESS REAPER — the churn child's reap, through the production NR 31 transaction on the CPU
/// P's request trapped on. A child that has not exited yet is refused by the transaction itself
/// (`NonTerminal` / `StillScheduled`) having mutated nothing; P asks again.
fn reap(shared: &SharedKernel, cpu: CpuId, target: u64, round: u64) {
    let Some(slot) = child_slot(target) else {
        return;
    };
    if REAPED[slot].load(Ordering::Acquire) {
        return;
    }
    let mut owners = crate::kernel::syscall::reap_txn::SharedReapOwners { shared, cpu };
    match crate::kernel::syscall::reap_txn::run_reap_transaction(&mut owners, target) {
        Ok(outcome) => {
            REAPED[slot].store(true, Ordering::Release);
            record(
                K_REAP,
                u64::from(cpu.0),
                target,
                0,
                [
                    round,
                    u64::from(outcome.process_reaped),
                    outcome.address_spaces_destroyed as u64,
                    outcome.address_spaces_quarantined as u64,
                    0,
                ],
            );
            publish(M_REAPED, target);
        }
        Err(refusal) if refusal.is_already_reaped() => {
            REAPED[slot].store(true, Ordering::Release);
            record(
                K_REAP_GONE,
                u64::from(cpu.0),
                target,
                0,
                [round, 0, 0, 0, 0],
            );
            publish(M_REAPED, target);
        }
        Err(_) => {
            REFUSALS.fetch_add(1, Ordering::AcqRel);
        }
    }
}

// ─────────────────────────────── the dump ───────────────────────────────

struct Line {
    buf: [u8; 256],
    len: usize,
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let end = self.len + b.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(b);
        self.len = end;
        Ok(())
    }
}

/// IEEE CRC-32 (the one `zlib.crc32` computes), bitwise.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Print one line twice, each copy carrying the CRC of its content.
pub(crate) fn emit_twice(args: core::fmt::Arguments<'_>) {
    let mut line = Line {
        buf: [0; 256],
        len: 0,
    };
    if core::fmt::write(&mut line, args).is_err() {
        return;
    }
    let body = core::str::from_utf8(&line.buf[..line.len]).unwrap_or("APSW_R torn");
    let crc = crc32(body.as_bytes());
    for _ in 0..2 {
        crate::kernel::printk::printk_emit_sync(format_args!("{} crc={:08x}", body, crc));
    }
}

/// The sealed dump, at P's last step: every record by stamp, then the totals.
fn dump() {
    let n = (SEQ.load(Ordering::Acquire) as usize).min(MAX_RECS);
    for (i, r) in RECS.iter().take(n).enumerate() {
        // A record stamped on the other CPU may still be mid-write: bounded wait for its flag.
        let mut spins = 0u32;
        while !r.valid.load(Ordering::Acquire) && spins < 1_000_000 {
            core::hint::spin_loop();
            spins += 1;
        }
        let kind = r.kind.load(Ordering::Relaxed) as usize;
        let step = r.step.load(Ordering::Relaxed) as usize;
        let v = |j: usize| r.v[j].load(Ordering::Relaxed);
        emit_twice(format_args!(
            "APSW_R seq={} kind={} cpu={} tid={} step={} v0={} v1={} v2={} v3={} v4={}",
            i + 1,
            KINDS.get(kind).copied().unwrap_or("UNKNOWN"),
            r.cpu.load(Ordering::Relaxed),
            r.tid.load(Ordering::Relaxed),
            if step == 0 {
                "-"
            } else {
                STEPS.get(step - 1).copied().unwrap_or("?")
            },
            v(0),
            v(1),
            v(2),
            v(3),
            v(4)
        ));
    }
    #[cfg(target_arch = "x86_64")]
    {
        let used = |c: usize| ap_stack::deepest_use(c).unwrap_or(0);
        let (bsp_reserved, bsp_painted_from, bsp_used) = ap_stack::boot_cpu();
        emit_twice(format_args!(
            "APSW_STACK reserved=0x{:x} used=0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x} bsp_reserved=0x{:x} bsp_unpainted_top=0x{:x}",
            ap_stack::reserved(),
            bsp_used,
            used(1),
            used(2),
            used(3),
            used(4),
            used(5),
            used(6),
            used(7),
            bsp_reserved,
            bsp_painted_from
        ));
    }
    #[cfg(target_arch = "aarch64")]
    {
        let used = |c: usize| ap_stack::deepest_use(c).unwrap_or(0);
        emit_twice(format_args!(
            "APSW_STACK reserved=0x{:x} used=0,0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x}",
            ap_stack::reserved(),
            used(1),
            used(2),
            used(3),
            used(4),
            used(5),
            used(6),
            used(7)
        ));
    }
    // SCHED-LOCK1 witness (default-off): its sealed dump precedes this one's end marker, so a
    // runner that stops at APSW_DUMP_END has it whole.
    #[cfg(feature = "sched-lock1-witness")]
    crate::kernel::schedlock1_witness::dump();
    let irq = |c: usize| IRQS[c].load(Ordering::Acquire);
    let pre = |c: usize| PRE[c].load(Ordering::Acquire);
    emit_twice(format_args!(
        "APSW_DUMP_END count={} overflow={} irq={},{},{},{},{},{},{},{} pre={},{},{},{},{},{},{},{} admitted=0x{:x} reap_retries={}",
        n,
        u8::from(OVERFLOW.load(Ordering::Acquire)),
        irq(0),
        irq(1),
        irq(2),
        irq(3),
        irq(4),
        irq(5),
        irq(6),
        irq(7),
        pre(0),
        pre(1),
        pre(2),
        pre(3),
        pre(4),
        pre(5),
        pre(6),
        pre(7),
        ADMITTED.load(Ordering::Acquire),
        REFUSALS.load(Ordering::Acquire)
    ));
}

/// AP-SCHED3 §C — stack headroom, where it is measurable: each secondary's dispatch stack is
/// painted with a pattern just before the boot CPU starts it (nothing has used it yet), and the
/// dump reports, per CPU, how far down from the top the pattern was ever overwritten — the deepest
/// any exception, syscall, fault or settlement on that CPU reached during the run. The boot CPU's
/// stack is in use from `_start` and cannot be painted, so it is not measured.
#[cfg(target_arch = "aarch64")]
pub mod ap_stack {
    const PAINT: u64 = 0x5359_4e43_5354_4b50;

    /// The pool slot `cpu` (1..=slots) runs on, as `[bottom, top)`.
    fn range(cpu: usize) -> Option<(usize, usize)> {
        let (end, stride, slots) = crate::arch::aarch64::smp::ap_stack_pool();
        if cpu == 0 || cpu > slots {
            return None;
        }
        let top = end - (cpu - 1) * stride;
        Some((top - stride, top))
    }

    pub fn reserved() -> usize {
        crate::arch::aarch64::smp::ap_stack_pool().1
    }

    /// Paint `cpu`'s stack. Called by the boot CPU before that CPU is started.
    pub fn paint(cpu: usize) {
        let Some((bottom, top)) = range(cpu) else {
            return;
        };
        let mut a = bottom;
        while a < top {
            // SAFETY: an aligned word of the reserved stack slot of a CPU not yet started.
            unsafe { core::ptr::write_volatile(a as *mut u64, PAINT) };
            a += 8;
        }
        // The secondary's first instructions run with its MMU off, so its stack writes are not
        // cacheable: clean AND invalidate the painted lines to the point of coherency, so no dirty
        // line of this CPU's can later be written back over them, and no clean one read instead.
        let ctr: u64;
        // SAFETY: a read of the cache type register.
        unsafe { core::arch::asm!("mrs {0}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
        let line = 4usize << ((ctr >> 16) & 0xf);
        let mut a = bottom;
        while a < top {
            // SAFETY: data-cache maintenance by VA over the slot just painted.
            unsafe { core::arch::asm!("dc civac, {0}", in(reg) a, options(nostack)) };
            a += line;
        }
        // SAFETY: completes the maintenance before the CPU_ON that follows.
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }

    /// Bytes below the top that were ever written, or `None` for a CPU with no slot.
    pub fn deepest_use(cpu: usize) -> Option<usize> {
        let (bottom, top) = range(cpu)?;
        let mut a = bottom;
        // SAFETY: aligned reads inside the slot; only the owning CPU writes it, and a stale read
        // only under-reports.
        while a < top && unsafe { core::ptr::read_volatile(a as *const u64) } == PAINT {
            a += 8;
        }
        Some(top - a)
    }
}

/// AP-SCHED4 — stack use on x86_64, measured (not a bound). Each secondary's dispatch stack (its
/// TSS `rsp0` and SYSCALL `rsp0`: every trap it takes from user mode and at its idle boundary) is
/// painted at provisioning, before boot publication — no secondary has switched to it yet — and
/// the dump reports how far below the top it was ever written. The boot CPU's trap stack is the
/// boot stack, in use since `_start`: everything below the provisioning frame (less a margin) is
/// painted then, so its figure is the deepest use AFTER provisioning, floored at the unpainted top.
#[cfg(target_arch = "x86_64")]
pub mod ap_stack {
    const PAINT: u64 = 0x5359_4e43_5354_4b50;
    /// Left unpainted below the painting frame (interrupts may nest beneath it meanwhile).
    const MARGIN: u64 = 64 * 1024;
    const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

    unsafe extern "C" {
        static boot_stack: u8;
        static boot_stack_end: u8;
    }

    static BSP_PAINT_TOP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

    fn range(cpu: usize) -> Option<(u64, u64)> {
        let bottom = crate::arch::x86_64::descriptor_tables::ap_dispatch_stack_base(cpu)?;
        let top = crate::arch::x86_64::descriptor_tables::ap_dispatch_stack_top(cpu)?;
        Some((bottom, top))
    }

    fn boot_range() -> (u64, u64) {
        // SAFETY: addresses of linker-placed symbols; nothing is read. They are low-VA symbols,
        // run through their high alias (`long_mode_entry`).
        let lo = (&raw const boot_stack) as u64 | KERNEL_VIRT_BASE;
        let hi = (&raw const boot_stack_end) as u64 | KERNEL_VIRT_BASE;
        (lo, hi)
    }

    pub fn reserved() -> usize {
        crate::arch::x86_64::descriptor_tables::AP_DISPATCH_STACK_BYTES
    }

    fn paint_words(bottom: u64, top: u64) {
        let mut a = bottom;
        while a < top {
            // SAFETY: an aligned word of a stack range nothing is using (see the callers).
            unsafe { core::ptr::write_volatile(a as *mut u64, PAINT) };
            a += 8;
        }
    }

    /// Paint every secondary's dispatch stack, and the boot stack below this frame. Called once,
    /// by the boot CPU, at provisioning (before publication: no secondary is on its dispatch
    /// stack yet).
    pub fn paint_all() {
        for cpu in 1..crate::kernel::scheduler::MAX_CPUS {
            if let Some((bottom, top)) = range(cpu) {
                paint_words(bottom, top);
            }
        }
        let rsp: u64;
        // SAFETY: reads the stack pointer.
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags))
        };
        let (lo, hi) = boot_range();
        if lo < rsp && rsp <= hi {
            let top = (rsp - MARGIN.min(rsp - lo)) & !7;
            paint_words(lo, top);
            BSP_PAINT_TOP.store(top, core::sync::atomic::Ordering::Release);
        }
    }

    fn deepest(bottom: u64, top: u64) -> u64 {
        let mut a = bottom;
        // SAFETY: aligned reads inside the range; a stale read only under-reports.
        while a < top && unsafe { core::ptr::read_volatile(a as *const u64) } == PAINT {
            a += 8;
        }
        a
    }

    /// Bytes below the top that were ever written, or `None` for a CPU with no slot.
    pub fn deepest_use(cpu: usize) -> Option<usize> {
        let (bottom, top) = range(cpu)?;
        Some((top - deepest(bottom, top)) as usize)
    }

    /// The boot CPU: `(reserved, unpainted top distance, deepest use)`, in bytes below the boot
    /// stack's top (0s when it was not painted).
    pub fn boot_cpu() -> (u64, u64, u64) {
        let (lo, hi) = boot_range();
        let painted = BSP_PAINT_TOP.load(core::sync::atomic::Ordering::Acquire);
        if painted == 0 {
            return (hi - lo, 0, 0);
        }
        (hi - lo, hi - painted, hi - deepest(lo, painted))
    }
}
