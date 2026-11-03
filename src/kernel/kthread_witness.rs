// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 witness — default off (`kthread1-witness`), co-enabled with each port's AP-scheduling
//! witness so the production user workload runs on every admitted CPU at the same time.
//!
//! A director kernel thread on CPU 0 declares the population (the dispatching CPUs), then on every
//! CPU of it creates two alternators and a sleeper, and drives:
//!
//! * kernel-origin interrupts during kernel-thread execution — each alternator round spins with
//!   patterned general registers until one of ITS OWN interrupts has been taken and returned, then
//!   checks every patterned register;
//! * cooperative switching — each round ends in a voluntary yield bracketed by a callee-saved
//!   register check (the holder's round whose scheduling point switched away has no separate
//!   yield), and private stack contents are re-checked every round; the two alternators of a CPU
//!   must strictly alternate;
//! * remote wake — the director (and, for CPU 0, a waker on CPU 1) wakes each CPU's sleeper while
//!   that CPU's holder alternator is spinning, so the reschedule IPI lands on a kernel thread; the
//!   holder's next scheduling point must honour it and the sleeper must resume. With one CPU the
//!   director wakes the sleeper locally once it has blocked, and there is no hold;
//! * a lost-wakeup probe (a wake landing between the condition check and the park), switch-safety
//!   probes (a masked section and an interrupt-masking guard), create/exit/reuse cycles with a
//!   stale-id probe, and exhaustion and recovery of the pool.
//!
//! Every fact is a record in a per-CPU ring (written only by that CPU, with interrupts masked,
//! never logging from interrupt context), stamped from one global sequence. The director dumps
//! them in sequence order with a checksum per record and a seal. `scripts/grade-kthread1-witness.py`
//! grades them; nothing here decides a verdict.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::kthread as arch;
use crate::kernel::kthread::{
    KSwitchReason, KSwitchRefusal, KTHREAD_CAPACITY, KTHREAD_STACK_BYTES, KThreadCtx, KThreadId,
    KWaitQueue, KWake,
};
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::runtime::SharedKernel;

const W_CPUS: usize = 8;
const RING: usize = 2048;
const ROUNDS: u64 = 5;
const MAX_ROUNDS: u64 = 400;
const CYCLES: usize = 6;
const SPIN_CAP: u64 = 1 << 31;
const TOKEN: u64 = 0x4B54_1000;

#[cfg(target_arch = "riscv64")]
const TICK_MS: u64 = 10;
#[cfg(target_arch = "aarch64")]
const TICK_MS: u64 = 50;
#[cfg(target_arch = "x86_64")]
const TICK_MS: u64 = 800;

// Record kinds.
pub(crate) const K_POP: u64 = 1;
pub(crate) const K_CREATE: u64 = 2;
pub(crate) const K_RUN: u64 = 3;
pub(crate) const K_IRQ: u64 = 4;
pub(crate) const K_SWITCH: u64 = 5;
pub(crate) const K_DISPATCH: u64 = 6;
pub(crate) const K_WAKE: u64 = 7;
pub(crate) const K_WOKEN: u64 = 8;
pub(crate) const K_RESCHED: u64 = 9;
pub(crate) const K_PROBE: u64 = 10;
pub(crate) const K_RECLAIM: u64 = 11;
pub(crate) const K_EXHAUST: u64 = 12;
pub(crate) const K_USER: u64 = 13;
pub(crate) const K_FINAL: u64 = 14;
pub(crate) const K_YIELD: u64 = 15;
pub(crate) const K_BLIP: u64 = 16;
pub(crate) const K_WAKEREQ: u64 = 17;
pub(crate) const K_DECIDE: u64 = 18;
/// KTHREAD1-ACCEPTANCE §4: the director declares itself (the one thread with no create record).
pub(crate) const K_DIRECTOR: u64 = 19;
/// KTHREAD1-ACCEPTANCE §5: P's single-CPU rounds (user-origin entries, authenticated as P's).
pub(crate) const K_UWORK: u64 = 20;
pub(crate) const K_UARM: u64 = 21;
pub(crate) const K_UTIMEOUT: u64 = 22;

fn kind_name(k: u64) -> &'static str {
    match k {
        K_POP => "pop",
        K_CREATE => "create",
        K_RUN => "run",
        K_IRQ => "irq",
        K_SWITCH => "switch",
        K_DISPATCH => "dispatch",
        K_WAKE => "wake",
        K_WOKEN => "woken",
        K_RESCHED => "resched",
        K_PROBE => "probe",
        K_RECLAIM => "reclaim",
        K_EXHAUST => "exhaust",
        K_USER => "user",
        K_FINAL => "final",
        K_YIELD => "yield",
        K_BLIP => "blip",
        K_WAKEREQ => "wakereq",
        K_DECIDE => "decide",
        K_DIRECTOR => "director",
        K_UWORK => "uwork",
        K_UARM => "uarm",
        K_UTIMEOUT => "utimeout",
        _ => "unknown",
    }
}

// Roles.
const R_DIRECTOR: u64 = 1;
const R_ALT: u64 = 2;
const R_SLEEPER: u64 = 3;
const R_WAKER: u64 = 4;
const R_BLIP: u64 = 5;
const R_LOSTWAKE: u64 = 6;
const R_HOLDER: u64 = 7;

// Probe kinds (K_PROBE f1).
const P_MASKED: u64 = 1;
const P_IRQ_GUARD: u64 = 2;
const P_STALE_WAKE: u64 = 3;
const P_LOST_WAKE: u64 = 4;

struct Ring {
    next: AtomicUsize,
    overflow: AtomicU64,
    slots: [[AtomicU64; 8]; RING],
}

impl Ring {
    const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            overflow: AtomicU64::new(0),
            slots: [const { [const { AtomicU64::new(0) }; 8] }; RING],
        }
    }
}

static RINGS: [Ring; W_CPUS] = [const { Ring::new() }; W_CPUS];
static SEQ: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Records are taken only between the population record and the seal.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Per slot: the role of the incarnation `ROLE_ID[slot]` names.
static ROLE_ID: [AtomicU64; KTHREAD_CAPACITY] = [const { AtomicU64::new(0) }; KTHREAD_CAPACITY];
static ROLE: [AtomicU64; KTHREAD_CAPACITY] = [const { AtomicU64::new(0) }; KTHREAD_CAPACITY];
static USER_DISPATCHES: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];
static KT_DISPATCHES: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];

// Scenario state.
static HOLDING: [AtomicBool; W_CPUS] = [const { AtomicBool::new(false) }; W_CPUS];
static HOLD_CLAIMED: [AtomicBool; W_CPUS] = [const { AtomicBool::new(false) }; W_CPUS];
static HOLD_PERMIT: [AtomicBool; W_CPUS] = [const { AtomicBool::new(false) }; W_CPUS];
static HOLD_ROUND: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];
/// One dispatching CPU: no other CPU can wake its sleeper or grant a hold, so the director wakes
/// the sleeper locally and the alternators stop after `ROUNDS` rounds.
static SINGLE: AtomicBool = AtomicBool::new(false);
static ZERO_GO: AtomicBool = AtomicBool::new(false);
static WAKE_DONE: [AtomicBool; W_CPUS] = [const { AtomicBool::new(false) }; W_CPUS];
static TOKENS: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];
static SLEEPERS: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];
static SLEEP_Q: [KWaitQueue; W_CPUS] = [const { KWaitQueue::new() }; W_CPUS];
/// Alternators that have run their rounds park here until their CPU's hold is permitted.
static PERMIT_Q: [KWaitQueue; W_CPUS] = [const { KWaitQueue::new() }; W_CPUS];
static ALT_DONE: AtomicU64 = AtomicU64::new(0);
static GATE_OPEN: AtomicBool = AtomicBool::new(false);
/// Exhaustion holders, spread so every holder has a registration slot.
const GATE_QS_N: usize = KTHREAD_CAPACITY.div_ceil(crate::kernel::kthread::KWAIT_CAPACITY);
static GATE_QS: [KWaitQueue; GATE_QS_N] = [const { KWaitQueue::new() }; GATE_QS_N];
static STALE_OPEN: AtomicBool = AtomicBool::new(false);
static STALE_Q: KWaitQueue = KWaitQueue::new();
static LOST_FLAG: AtomicBool = AtomicBool::new(false);
static LOST_Q: KWaitQueue = KWaitQueue::new();
/// The waker parks here until the director lets it go (it used to poll by yielding).
static ZERO_Q: KWaitQueue = KWaitQueue::new();
/// A wake's issuer parks here until the target CPU's holder is spinning (it used to poll).
static HOLD_Q: [KWaitQueue; W_CPUS] = [const { KWaitQueue::new() }; W_CPUS];
/// KTHREAD1-ACCEPTANCE §4: the role of the creation in progress, and the creator's CPU, read by
/// [`note_create`] inside `KThreadPool::create` before the new incarnation is enqueued. Only the
/// director creates (and `maybe_start` creates the director), one creation at a time.
static PENDING_ROLE: AtomicU64 = AtomicU64::new(0);
static CREATOR_CPU: AtomicU64 = AtomicU64::new(0);

fn role_of(id: KThreadId) -> u64 {
    let s = id.slot();
    if ROLE_ID[s].load(Ordering::Acquire) == id.raw() {
        ROLE[s].load(Ordering::Acquire)
    } else {
        0
    }
}

fn set_role(id: KThreadId, role: u64) {
    ROLE[id.slot()].store(role, Ordering::Release);
    ROLE_ID[id.slot()].store(id.raw(), Ordering::Release);
}

/// Take one record on `cpu`'s ring. The caller is CPU `cpu`; interrupts are masked for the write
/// so an interrupt record cannot interleave with a thread record in the same slot.
fn record(cpu: CpuId, kind: u64, f: [u64; 5]) {
    let ci = cpu.0 as usize;
    if ci >= W_CPUS || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let was = arch::irq_disable_save();
    let ring = &RINGS[ci];
    let i = ring.next.load(Ordering::Relaxed);
    if i >= RING {
        ring.overflow.fetch_add(1, Ordering::AcqRel);
    } else {
        let seq = SEQ.fetch_add(1, Ordering::AcqRel);
        let slot = &ring.slots[i];
        slot[0].store(seq, Ordering::Relaxed);
        slot[1].store(kind | ((cpu.0 as u64) << 8), Ordering::Relaxed);
        for (j, v) in f.iter().enumerate() {
            slot[2 + j].store(*v, Ordering::Relaxed);
        }
        slot[7].store(1, Ordering::Release);
        ring.next.store(i + 1, Ordering::Release);
    }
    arch::irq_restore(was);
}

// ── hooks from production owners (all feature-gated at the call site) ─────────────────────────

/// The drain owner ran on `cpu`: start the director once, on CPU 0.
pub(crate) fn maybe_start(shared: &SharedKernel, cpu: CpuId) {
    // Not during the user workload's idle-wake phase, which needs its secondaries idle.
    if cpu.0 != 0
        || !crate::kernel::apsched_witness::idle_phase_over()
        || STARTED.load(Ordering::Acquire)
        || STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    PENDING_ROLE.store(R_DIRECTOR, Ordering::Release);
    CREATOR_CPU.store(cpu.0 as u64, Ordering::Release);
    match shared
        .kthreads
        .create(director, 0, cpu, TaskPriority::Normal)
    {
        Ok(id) => {
            crate::yarm_log!("KT1_WITNESS_START director={:#x} cpu=0", id.raw());
        }
        Err(e) => crate::yarm_log!("KT1_WITNESS_START result=fail reason={:?}", e),
    }
}

/// KTHREAD1-ACCEPTANCE §4 — called by `KThreadPool::create` for a new incarnation BEFORE it is
/// enqueued: its role is set and its creation recorded while no CPU can yet dispatch it, so its
/// create record precedes every record of its execution and no early dispatch goes unrecorded
/// for want of a role.
pub(crate) fn note_create(id: KThreadId, home: CpuId, low: usize, high: usize) {
    let role = PENDING_ROLE.swap(0, Ordering::AcqRel);
    if role == 0 {
        return;
    }
    set_role(id, role);
    record(
        CpuId(CREATOR_CPU.load(Ordering::Acquire) as u8),
        K_CREATE,
        [id.raw(), role, home.0 as u64, low as u64, high as u64],
    );
}

static IDLE_ARRIVALS: [AtomicU64; W_CPUS] = [const { AtomicU64::new(0) }; W_CPUS];

/// `cpu` arrived at its idle wait (every port's idle-arrival point; observation only).
pub(crate) fn note_idle_arrival(cpu: usize) {
    if let Some(c) = IDLE_ARRIVALS.get(cpu) {
        c.fetch_add(1, Ordering::AcqRel);
    }
}

/// KTHREAD1-ACCEPTANCE §5 — one of P's single-CPU steps, from its DebugLog syscall: a user-origin
/// entry whose task the trap path authenticated (`tid`, `asid`). Recorded on the CPU it trapped on:
/// `uwork` [tid, round, the value P's load returned, asid, 0]; `uarm` [tid, round, deadline
/// ticks, the clock now, this CPU's idle arrivals]; `utimeout` [tid, round, the receive's
/// result, the clock now, this CPU's idle arrivals].
pub(crate) fn note_user_step(
    shared: &SharedKernel,
    cpu: CpuId,
    tid: u64,
    asid: u64,
    step: &str,
    a2: u64,
    a3: u64,
) {
    if tid != crate::kernel::apsched_witness::P_TID
        || asid != crate::kernel::apsched_witness::p_asid()
    {
        return;
    }
    let idle = IDLE_ARRIVALS
        .get(cpu.0 as usize)
        .map_or(0, |c| c.load(Ordering::Acquire));
    let now = shared.sched_domain().clock_now().0;
    match step {
        "S1_WORK" => record(cpu, K_UWORK, [tid, a2, a3, asid, 0]),
        "S1_ARM" => record(cpu, K_UARM, [tid, a2, a3, now, idle]),
        "S1_TIMEOUT" => record(cpu, K_UTIMEOUT, [tid, a2, a3, now, idle]),
        _ => {}
    }
}

/// The one mark seam (`d6_genuine_mark_running_via_task_seam`) marked a USER task Running on `cpu`.
pub(crate) fn note_user_dispatch(cpu: CpuId) {
    if ACTIVE.load(Ordering::Acquire)
        && let Some(c) = USER_DISPATCHES.get(cpu.0 as usize)
    {
        c.fetch_add(1, Ordering::AcqRel);
    }
}

pub(crate) fn note_dispatch(cpu: CpuId, id: KThreadId) {
    if let Some(c) = KT_DISPATCHES.get(cpu.0 as usize)
        && ACTIVE.load(Ordering::Acquire)
    {
        c.fetch_add(1, Ordering::AcqRel);
    }
    // The director polls by yielding; its dispatches are not recorded, or its waits would fill
    // the CPU's ring. It is declared by its own record and exempt by identity, not by role.
    let role = role_of(id);
    if role != R_DIRECTOR && role != 0 {
        record(cpu, K_DISPATCH, [id.raw(), role, 0, 0, 0]);
    }
}

pub(crate) fn note_switched_out(cpu: CpuId, id: KThreadId, reason: KSwitchReason, resident: u64) {
    // As for dispatches.
    let role = role_of(id);
    if role != R_DIRECTOR && role != 0 {
        record(cpu, K_SWITCH, [id.raw(), reason as u64, resident, role, 0]);
    }
}

pub(crate) fn note_reclaim(cpu: CpuId, id: KThreadId, free: usize) {
    record(
        cpu,
        K_RECLAIM,
        [
            id.raw(),
            id.slot() as u64,
            id.generation() as u64,
            free as u64,
            role_of(id),
        ],
    );
}

/// From interrupt context: an authenticated kernel-thread interrupt. `kind` 1 = timer, 2 =
/// reschedule. Never logs.
pub(crate) fn note_interrupt(
    cpu: CpuId,
    id: KThreadId,
    kind: u64,
    pc: u64,
    frame: usize,
    sources: u64,
) {
    let role = role_of(id);
    if role == R_ALT || role == R_SLEEPER || role == R_WAKER || role == R_LOSTWAKE {
        // f4: the role, and (bits 8..) the CPUs the hardware names as the reschedule's requesters
        // (RISC-V mailbox sources, AArch64 SGI source; 0 where the hardware names none).
        record(
            cpu,
            K_IRQ,
            [id.raw(), kind, pc, frame as u64, role | (sources << 8)],
        );
    }
}

// ── register checks ───────────────────────────────────────────────────────────────────────────

const ASYNC_REGS: usize = 24;

/// Spin with patterned general registers until `*counter != initial` (one of this thread's own
/// interrupts was taken and returned) or the cap runs out; store the registers afterwards. Every
/// register is caller-saved from the interrupt's point of view: an interrupt must preserve all of
/// them. Returns the iterations left (0 = cap ran out).
#[cfg(target_arch = "riscv64")]
unsafe fn spin_patterned(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; ASYNC_REGS],
) -> u64 {
    let mut left: u64 = SPIN_CAP;
    // SAFETY: registers named below are declared clobbered; `out` holds 24 slots.
    unsafe {
        core::arch::asm!(
            "addi t0, a4, 1", "addi t1, a4, 2", "addi t2, a4, 3", "addi t3, a4, 4",
            "addi t4, a4, 5", "addi t5, a4, 6", "addi t6, a4, 7",
            "addi s2, a4, 8", "addi s3, a4, 9", "addi s4, a4, 10", "addi s5, a4, 11",
            "addi s6, a4, 12", "addi s7, a4, 13", "addi s8, a4, 14", "addi s9, a4, 15",
            "addi s10, a4, 16", "addi s11, a4, 17",
            "addi a5, a4, 18", "addi a6, a4, 19", "addi a7, a4, 20", "addi ra, a4, 21",
            "2:",
            "ld a4, 0(a0)",
            "bne a4, a1, 3f",
            "addi a2, a2, -1",
            "bnez a2, 2b",
            "3:",
            "sd t0, 0(a3)", "sd t1, 8(a3)", "sd t2, 16(a3)", "sd t3, 24(a3)",
            "sd t4, 32(a3)", "sd t5, 40(a3)", "sd t6, 48(a3)",
            "sd s2, 56(a3)", "sd s3, 64(a3)", "sd s4, 72(a3)", "sd s5, 80(a3)",
            "sd s6, 88(a3)", "sd s7, 96(a3)", "sd s8, 104(a3)", "sd s9, 112(a3)",
            "sd s10, 120(a3)", "sd s11, 128(a3)",
            "sd a5, 136(a3)", "sd a6, 144(a3)", "sd a7, 152(a3)", "sd ra, 160(a3)",
            in("a0") counter, in("a1") initial, inout("a2") left, in("a3") out.as_mut_ptr(),
            inout("a4") seed => _,
            out("t0") _, out("t1") _, out("t2") _, out("t3") _, out("t4") _, out("t5") _,
            out("t6") _, out("s2") _, out("s3") _, out("s4") _, out("s5") _, out("s6") _,
            out("s7") _, out("s8") _, out("s9") _, out("s10") _, out("s11") _, out("a5") _,
            out("a6") _, out("a7") _, out("ra") _,
        );
    }
    left
}
#[cfg(target_arch = "riscv64")]
const ASYNC_USED: usize = 21;

#[cfg(target_arch = "x86_64")]
unsafe fn spin_patterned(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; ASYNC_REGS],
) -> u64 {
    let mut left: u64 = SPIN_CAP;
    // SAFETY: registers named below are declared clobbered; `out` holds 24 slots.
    unsafe {
        core::arch::asm!(
            "lea rax, [r8 + 1]", "lea r9, [r8 + 2]", "lea r10, [r8 + 3]", "lea r11, [r8 + 4]",
            "lea r12, [r8 + 5]", "lea r13, [r8 + 6]", "lea r14, [r8 + 7]", "lea r15, [r8 + 8]",
            "2:",
            "mov r8, [rdi]",
            "cmp r8, rsi",
            "jne 3f",
            "dec rdx",
            "jnz 2b",
            "3:",
            "mov [rcx + 0], rax", "mov [rcx + 8], r9", "mov [rcx + 16], r10", "mov [rcx + 24], r11",
            "mov [rcx + 32], r12", "mov [rcx + 40], r13", "mov [rcx + 48], r14", "mov [rcx + 56], r15",
            in("rdi") counter, in("rsi") initial, inout("rdx") left, in("rcx") out.as_mut_ptr(),
            inout("r8") seed => _,
            out("rax") _, out("r9") _, out("r10") _, out("r11") _, out("r12") _, out("r13") _,
            out("r14") _, out("r15") _,
        );
    }
    left
}
#[cfg(target_arch = "x86_64")]
const ASYNC_USED: usize = 8;

#[cfg(target_arch = "aarch64")]
unsafe fn spin_patterned(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; ASYNC_REGS],
) -> u64 {
    let mut left: u64 = SPIN_CAP;
    // SAFETY: registers named below are declared clobbered; `out` holds 24 slots.
    unsafe {
        core::arch::asm!(
            "add x5, x4, #1", "add x6, x4, #2", "add x7, x4, #3", "add x8, x4, #4",
            "add x9, x4, #5", "add x10, x4, #6", "add x11, x4, #7", "add x12, x4, #8",
            "add x13, x4, #9", "add x14, x4, #10", "add x15, x4, #11", "add x16, x4, #12",
            "add x17, x4, #13", "add x20, x4, #14", "add x21, x4, #15", "add x22, x4, #16",
            "add x23, x4, #17", "add x24, x4, #18", "add x25, x4, #19", "add x26, x4, #20",
            "add x27, x4, #21", "add x28, x4, #22", "add x30, x4, #23",
            "2:",
            "ldr x4, [x0]",
            "cmp x4, x1",
            "b.ne 3f",
            "subs x2, x2, #1",
            "b.ne 2b",
            "3:",
            "stp x5, x6, [x3, #0]", "stp x7, x8, [x3, #16]", "stp x9, x10, [x3, #32]",
            "stp x11, x12, [x3, #48]", "stp x13, x14, [x3, #64]", "stp x15, x16, [x3, #80]",
            "stp x17, x20, [x3, #96]", "stp x21, x22, [x3, #112]", "stp x23, x24, [x3, #128]",
            "stp x25, x26, [x3, #144]", "stp x27, x28, [x3, #160]", "str x30, [x3, #176]",
            in("x0") counter, in("x1") initial, inout("x2") left, in("x3") out.as_mut_ptr(),
            inout("x4") seed => _,
            out("x5") _, out("x6") _, out("x7") _, out("x8") _, out("x9") _, out("x10") _,
            out("x11") _, out("x12") _, out("x13") _, out("x14") _, out("x15") _, out("x16") _,
            out("x17") _, out("x20") _, out("x21") _, out("x22") _, out("x23") _, out("x24") _,
            out("x25") _, out("x26") _, out("x27") _, out("x28") _, out("x30") _,
        );
    }
    left
}
#[cfg(target_arch = "aarch64")]
const ASYNC_USED: usize = 23;

/// The yield call made from inside the callee-saved check below.
extern "C" fn witness_yield(ctx: *const KThreadCtx) -> u64 {
    // SAFETY: the caller passes its own context.
    match unsafe { (*ctx).yield_now() } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

const CS_REGS: usize = 10;

/// Load the callee-saved registers the compiler lets us name with patterns, yield through an
/// ordinary C call, and store them back: a voluntary switch must preserve exactly these.
#[cfg(target_arch = "riscv64")]
fn yield_callee_checked(ctx: &KThreadCtx, seed: u64, out: &mut [u64; CS_REGS]) -> u64 {
    let rc: u64;
    // SAFETY: the C call follows the ABI (`clobber_abi("C")`); the callee-saved registers loaded
    // with patterns are declared clobbered so the compiler preserves its own values around this.
    unsafe {
        core::arch::asm!(
            "addi s2, a2, 1", "addi s3, a2, 2", "addi s4, a2, 3", "addi s5, a2, 4",
            "addi s6, a2, 5", "addi s7, a2, 6", "addi s8, a2, 7", "addi s9, a2, 8",
            "addi s10, a2, 9", "addi s11, a2, 10",
            "addi sp, sp, -16",
            "sd a1, 0(sp)",
            "jalr a3",
            "ld a1, 0(sp)",
            "addi sp, sp, 16",
            "sd s2, 0(a1)", "sd s3, 8(a1)", "sd s4, 16(a1)", "sd s5, 24(a1)",
            "sd s6, 32(a1)", "sd s7, 40(a1)", "sd s8, 48(a1)", "sd s9, 56(a1)",
            "sd s10, 64(a1)", "sd s11, 72(a1)",
            inout("a0") ctx as *const KThreadCtx => rc,
            inout("a1") out.as_mut_ptr() => _,
            in("a2") seed,
            in("a3") witness_yield as extern "C" fn(*const KThreadCtx) -> u64,
            out("s2") _, out("s3") _, out("s4") _, out("s5") _, out("s6") _, out("s7") _,
            out("s8") _, out("s9") _, out("s10") _, out("s11") _,
            clobber_abi("C"),
        );
    }
    rc
}
#[cfg(target_arch = "riscv64")]
const CS_USED: usize = 10;

#[cfg(target_arch = "x86_64")]
fn yield_callee_checked(ctx: &KThreadCtx, seed: u64, out: &mut [u64; CS_REGS]) -> u64 {
    let rc: u64;
    // SAFETY: as the RISC-V form. The stack stays 16-byte aligned for the call: two pushes.
    unsafe {
        core::arch::asm!(
            "lea r12, [rdx + 1]", "lea r13, [rdx + 2]", "lea r14, [rdx + 3]", "lea r15, [rdx + 4]",
            "push rsi",
            "push rsi",
            "call rax",
            "pop rsi",
            "pop rsi",
            "mov [rsi + 0], r12", "mov [rsi + 8], r13", "mov [rsi + 16], r14", "mov [rsi + 24], r15",
            inout("rdi") ctx as *const KThreadCtx => _,
            inout("rsi") out.as_mut_ptr() => _,
            in("rdx") seed,
            inout("rax") witness_yield as extern "C" fn(*const KThreadCtx) -> u64 as usize => rc,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            clobber_abi("C"),
        );
    }
    rc
}
#[cfg(target_arch = "x86_64")]
const CS_USED: usize = 4;

#[cfg(target_arch = "aarch64")]
fn yield_callee_checked(ctx: &KThreadCtx, seed: u64, out: &mut [u64; CS_REGS]) -> u64 {
    let rc: u64;
    // SAFETY: as the RISC-V form.
    unsafe {
        core::arch::asm!(
            "add x20, x2, #1", "add x21, x2, #2", "add x22, x2, #3", "add x23, x2, #4",
            "add x24, x2, #5", "add x25, x2, #6", "add x26, x2, #7", "add x27, x2, #8",
            "add x28, x2, #9",
            "stp x1, x1, [sp, #-16]!",
            "blr x3",
            "ldp x1, x9, [sp], #16",
            "stp x20, x21, [x1, #0]", "stp x22, x23, [x1, #16]", "stp x24, x25, [x1, #32]",
            "stp x26, x27, [x1, #48]", "str x28, [x1, #64]",
            inout("x0") ctx as *const KThreadCtx => rc,
            inout("x1") out.as_mut_ptr() => _,
            in("x2") seed,
            in("x3") witness_yield as extern "C" fn(*const KThreadCtx) -> u64,
            out("x20") _, out("x21") _, out("x22") _, out("x23") _, out("x24") _, out("x25") _,
            out("x26") _, out("x27") _, out("x28") _,
            clobber_abi("C"),
        );
    }
    rc
}
#[cfg(target_arch = "aarch64")]
const CS_USED: usize = 9;

// ── the scenario ──────────────────────────────────────────────────────────────────────────────

fn ticks(ctx: &KThreadCtx) -> u64 {
    ctx.shared().sched_domain().clock_now().0
}

/// Yield until `cond` holds or `ms` (in scheduler ticks of this port) passed.
fn wait_for(ctx: &KThreadCtx, ms: u64, cond: impl Fn() -> bool) -> bool {
    let deadline = ticks(ctx) + ms / TICK_MS + 1;
    loop {
        if cond() {
            return true;
        }
        if ticks(ctx) > deadline {
            return cond();
        }
        let _ = ctx.yield_now();
    }
}

/// Create a witness thread. Its role and its create record are written by [`note_create`],
/// inside the pool's creation, before it can run.
fn create_raw(
    ctx: &KThreadCtx,
    entry: crate::kernel::kthread::KEntry,
    arg: usize,
    cpu: CpuId,
    role: u64,
) -> Result<KThreadId, crate::kernel::kthread::KCreateError> {
    let pool = ctx.pool();
    let _m = ctx.masked();
    PENDING_ROLE.store(role, Ordering::Release);
    CREATOR_CPU.store(ctx.home().0 as u64, Ordering::Release);
    let r = pool.create(entry, arg, cpu, TaskPriority::Normal);
    // A creation refused before its slot was taken never reached the hook.
    PENDING_ROLE.store(0, Ordering::Release);
    r
}

fn create(
    ctx: &KThreadCtx,
    entry: crate::kernel::kthread::KEntry,
    arg: usize,
    cpu: CpuId,
    role: u64,
) -> Option<KThreadId> {
    create_raw(ctx, entry, arg, cpu, role).ok()
}

fn wake_code(w: KWake) -> u64 {
    match w {
        KWake::Enqueued => 1,
        KWake::Pending => 2,
        KWake::Stale => 3,
    }
}

fn refusal_code(r: KSwitchRefusal) -> u64 {
    match r {
        KSwitchRefusal::IrqMasked => 1,
        KSwitchRefusal::GuardHeld => 2,
        KSwitchRefusal::NotResident => 3,
        KSwitchRefusal::WrongStack => 4,
        KSwitchRefusal::Stale => 5,
        KSwitchRefusal::Scheduler => 6,
    }
}

/// The interrupt count of `id`'s slot, as an address the patterned spin can poll.
fn irq_counter(ctx: &KThreadCtx) -> *const u64 {
    ctx.pool().interrupt_counter_addr(ctx.id())
}

/// One spin until this thread's next interrupt; returns (registers intact, interrupt arrived).
fn spin_once(ctx: &KThreadCtx, seed: u64) -> (bool, bool) {
    let counter = irq_counter(ctx);
    // SAFETY: the counter is an `AtomicU64` in the pool; a plain load of it is what the spin does.
    let initial = unsafe { core::ptr::read_volatile(counter) };
    let mut out = [0u64; ASYNC_REGS];
    // SAFETY: see `spin_patterned`.
    let left = unsafe { spin_patterned(counter, initial, seed, &mut out) };
    let ok = (0..ASYNC_USED).all(|i| out[i] == seed.wrapping_add(i as u64 + 1));
    (ok, left != 0)
}

fn alternator(ctx: &KThreadCtx, _arg: usize) {
    let me = ctx.id();
    let cpu = ctx.home();
    let ci = cpu.0 as usize;
    let mut private = [0u64; 128];
    for (i, w) in private.iter_mut().enumerate() {
        *w = me.raw().rotate_left(i as u32) ^ (i as u64);
    }
    let pool = ctx.pool();
    let mut round = 0u64;
    loop {
        round += 1;
        // Done after ROUNDS rounds, once this CPU's hold has happened and at least four more rounds
        // have run after it (the alternation the grader checks). Bounded either way.
        let hold_round = HOLD_ROUND[ci].load(Ordering::Acquire);
        let settled = SINGLE.load(Ordering::Acquire) || (hold_round != 0 && round > hold_round + 4);
        if round > MAX_ROUNDS || (round > ROUNDS && settled) {
            break;
        }
        // Holds are granted one CPU at a time, so a CPU can wait long for its own. Past ROUNDS,
        // park until the permit instead of running rounds that would only fill the record ring.
        if round > ROUNDS && hold_round == 0 && !SINGLE.load(Ordering::Acquire) {
            let _ = PERMIT_Q[ci].wait_until(ctx, || HOLD_PERMIT[ci].load(Ordering::Acquire));
        }
        let seed = me.raw().wrapping_mul(31).wrapping_add(round << 20) & !0xFFF;
        let mut regs_ok = true;
        let mut arrived = true;
        // The hold is granted by whoever will wake this CPU's sleeper, and one CPU at a time: a
        // held CPU runs nothing else, so the waker must be running elsewhere.
        let holder = HOLD_PERMIT[ci].load(Ordering::Acquire)
            && !HOLD_CLAIMED[ci].swap(true, Ordering::AcqRel);
        if holder {
            HOLDING[ci].store(true, Ordering::Release);
            {
                let _m = ctx.masked();
                let _ = HOLD_Q[ci].wake_all(pool);
            }
            // Hold this CPU — spinning, interrupts enabled — until this CPU's sleeper has been
            // woken from another CPU and one more interrupt (the reschedule IPI) has landed here.
            let mut spins = 0u32;
            loop {
                let done = WAKE_DONE[ci].load(Ordering::Acquire);
                let (ok, got) = spin_once(ctx, seed);
                regs_ok &= ok;
                arrived &= got;
                spins += 1;
                if (done && spins > 1) || !got || spins > 100_000 {
                    break;
                }
            }
            HOLDING[ci].store(false, Ordering::Release);
        } else {
            let (ok, got) = spin_once(ctx, seed);
            regs_ok = ok;
            arrived = got;
        }
        let stack_ok = private
            .iter()
            .enumerate()
            .all(|(i, w)| *w == me.raw().rotate_left(i as u32) ^ (i as u64));
        let checks = u64::from(stack_ok) | (u64::from(regs_ok) << 1) | (u64::from(arrived) << 3);
        record(
            cpu,
            K_RUN,
            [
                me.raw(),
                round,
                arch::current_sp() as u64,
                pool.interrupt_count(me),
                checks,
            ],
        );
        if holder {
            let c = pool.cpu_counters(cpu);
            let pending = u64::from(pool.resched_pending(cpu));
            // The decision's input, recorded BEFORE the scheduling point (which may switch).
            record(
                cpu,
                K_DECIDE,
                [
                    me.raw(),
                    pending,
                    c.resched_interrupts,
                    c.timer_interrupts,
                    round,
                ],
            );
            let yielded = ctx.sched_point().unwrap_or(false);
            record(
                cpu,
                K_RESCHED,
                [
                    me.raw(),
                    u64::from(yielded),
                    pending,
                    c.resched_interrupts,
                    c.resched_honoured,
                ],
            );
            HOLD_ROUND[ci].store(round, Ordering::Release);
            // A scheduling point that switched away was this round's turn: yielding again would
            // hand the other alternator two turns in a row.
            if yielded {
                continue;
            }
        }
        let mut cs = [0u64; CS_REGS];
        let rc = yield_callee_checked(ctx, seed, &mut cs);
        let cs_ok = (0..CS_USED).all(|i| cs[i] == seed.wrapping_add(i as u64 + 1));
        record(cpu, K_YIELD, [me.raw(), round, u64::from(cs_ok), rc, 0]);
    }
    ALT_DONE.fetch_add(1, Ordering::AcqRel);
}

fn sleeper(ctx: &KThreadCtx, _arg: usize) {
    let ci = ctx.home().0 as usize;
    let blocked = SLEEP_Q[ci]
        .wait_until(ctx, || {
            TOKENS[ci].load(Ordering::Acquire) == TOKEN + ci as u64
        })
        .unwrap_or(u32::MAX);
    record(
        ctx.home(),
        K_WOKEN,
        [
            ctx.id().raw(),
            TOKENS[ci].load(Ordering::Acquire),
            blocked as u64,
            ctx.pool().counters().early_wakes,
            0,
        ],
    );
}

/// Wake `target_cpu`'s sleeper from this (other) CPU once that CPU's holder is spinning. With one
/// CPU (`SINGLE`) there is no holder: the sleeper is woken locally once it has blocked.
fn issue_remote_wake(ctx: &KThreadCtx, target: usize) {
    let sleeper = SLEEPERS[target].load(Ordering::Acquire);
    if SINGLE.load(Ordering::Acquire) {
        let blocked = || {
            KThreadId::from_raw(sleeper)
                .and_then(|id| ctx.pool().state_of(id))
                .is_some_and(|st| st == crate::kernel::kthread::KState::Blocked)
        };
        let _ = wait_for(ctx, 30_000, blocked);
    } else {
        HOLD_PERMIT[target].store(true, Ordering::Release);
        {
            let _m = ctx.masked();
            let _ = PERMIT_Q[target].wake_all(ctx.pool());
        }
        let _ = HOLD_Q[target].wait_until(ctx, || HOLDING[target].load(Ordering::Acquire));
    }
    TOKENS[target].store(TOKEN + target as u64, Ordering::Release);
    // The request, recorded BEFORE the wake: the reschedule IPI it causes may land on the target
    // before this CPU writes its outcome record.
    record(
        ctx.home(),
        K_WAKEREQ,
        [
            sleeper,
            target as u64,
            u64::from(HOLDING[target].load(Ordering::Acquire)),
            TOKEN + target as u64,
            0,
        ],
    );
    let woken = {
        let _m = ctx.masked();
        // The queue's own wake path: collect the registration, then the pool's wake.
        let mut outcome = 0u64;
        if let Some(id) = KThreadId::from_raw(sleeper) {
            let n = SLEEP_Q[target].wake_all(ctx.pool());
            outcome = if n == 1 {
                wake_code(KWake::Enqueued)
            } else {
                wake_code(ctx.pool().wake(id))
            };
        }
        outcome
    };
    record(
        ctx.home(),
        K_WAKE,
        [sleeper, target as u64, woken, TOKEN + target as u64, 1],
    );
    WAKE_DONE[target].store(true, Ordering::Release);
}

/// Wakes CPU 0's sleeper from another CPU, after the director finished every other CPU's wake
/// (so no CPU this waker needs is held).
fn waker(ctx: &KThreadCtx, _arg: usize) {
    let _ = ZERO_Q.wait_until(ctx, || ZERO_GO.load(Ordering::Acquire));
    issue_remote_wake(ctx, 0);
}

fn blip(ctx: &KThreadCtx, _arg: usize) {
    let id = ctx.id();
    record(
        ctx.home(),
        K_BLIP,
        [
            id.raw(),
            id.slot() as u64,
            id.generation() as u64,
            arch::current_sp() as u64,
            0,
        ],
    );
}

/// The stale-id probe's target: a new incarnation in the slot the old id names, parked so that a
/// wake which skipped the incarnation check would visibly enqueue it.
fn stale_target(ctx: &KThreadCtx, arg: usize) {
    blip(ctx, arg);
    let _ = STALE_Q.wait_until(ctx, || STALE_OPEN.load(Ordering::Acquire));
}

fn holder(ctx: &KThreadCtx, arg: usize) {
    let _ = GATE_QS[arg % GATE_QS_N].wait_until(ctx, || GATE_OPEN.load(Ordering::Acquire));
}

/// The lost-wakeup probe: the first evaluation of the condition wakes the thread itself — a wake
/// arriving after the failed check and the registration, before the park — and the second finds
/// the condition true. The park must return at once; a lost wake leaves the thread Blocked.
fn lost_wake_probe(ctx: &KThreadCtx, _arg: usize) {
    let me = ctx.id();
    let pool = ctx.pool();
    let first = AtomicBool::new(true);
    let blocked = LOST_Q
        .wait_until(ctx, || {
            if first.swap(false, Ordering::AcqRel) {
                LOST_FLAG.store(true, Ordering::Release);
                let _ = pool.wake(me);
                return false;
            }
            LOST_FLAG.load(Ordering::Acquire)
        })
        .unwrap_or(u32::MAX);
    record(
        ctx.home(),
        K_PROBE,
        [
            me.raw(),
            P_LOST_WAKE,
            blocked as u64,
            pool.counters().early_wakes,
            0,
        ],
    );
}

/// Every thread the director created (the whole pool can be created more than once over a run).
static CREATED: [AtomicU64; 96] = [const { AtomicU64::new(0) }; 96];
/// The role each remembered thread was created with: a final record names the role of ITS
/// incarnation, not whatever role a later thread in the same slot holds.
static CREATED_ROLE: [AtomicU64; 96] = [const { AtomicU64::new(0) }; 96];
static N_CREATED: AtomicUsize = AtomicUsize::new(0);

fn remember(id: Option<KThreadId>) -> Option<KThreadId> {
    if let Some(id) = id {
        let n = N_CREATED.fetch_add(1, Ordering::AcqRel);
        if n < CREATED.len() {
            CREATED_ROLE[n].store(role_of(id), Ordering::Release);
            CREATED[n].store(id.raw(), Ordering::Release);
        }
    }
    id
}

fn director(ctx: &KThreadCtx, _arg: usize) {
    let shared = ctx.shared();
    let pool = ctx.pool();
    let topo = shared.sched_domain().topo();
    let ceiling = (1u64 << W_CPUS) - 1;
    // 1. The population: wait for every present CPU (up to the production ceiling) to dispatch.
    let target = topo.present() & ceiling;
    let _ = wait_for(ctx, 60_000, || topo.dispatching() & ceiling == target);
    let pop = topo.dispatching() & ceiling;
    ACTIVE.store(true, Ordering::Release);
    record(
        ctx.home(),
        K_POP,
        [
            pop,
            topo.present(),
            KTHREAD_CAPACITY as u64,
            KTHREAD_STACK_BYTES as u64,
            ROUNDS,
        ],
    );
    {
        let (low, high) = pool.stack_bounds(ctx.id().slot());
        record(
            ctx.home(),
            K_DIRECTOR,
            [
                ctx.id().raw(),
                ctx.home().0 as u64,
                low as u64,
                high as u64,
                0,
            ],
        );
    }
    let cpus: [bool; W_CPUS] = core::array::from_fn(|c| pop & (1 << c) != 0);
    let second = (1..W_CPUS).find(|&c| cpus[c]);
    SINGLE.store(second.is_none(), Ordering::Release);
    // 2. Per CPU: a sleeper, then two alternators.
    let mut alts = 0u64;
    for c in 0..W_CPUS {
        if !cpus[c] {
            continue;
        }
        let cpu = CpuId(c as u8);
        if let Some(s) = remember(create(ctx, sleeper, 0, cpu, R_SLEEPER)) {
            SLEEPERS[c].store(s.raw(), Ordering::Release);
        }
        for _ in 0..2 {
            if remember(create(ctx, alternator, 0, cpu, R_ALT)).is_some() {
                alts += 1;
            }
        }
    }
    // 3. Remote wakes: this CPU wakes every other CPU's sleeper; a waker on the second CPU wakes
    //    this CPU's.
    if let Some(s) = second {
        let _ = remember(create(ctx, waker, 0, CpuId(s as u8), R_WAKER));
    }
    for c in 1..W_CPUS {
        if cpus[c] {
            issue_remote_wake(ctx, c);
        }
    }
    if second.is_none() {
        issue_remote_wake(ctx, 0);
    }
    ZERO_GO.store(true, Ordering::Release);
    {
        let _m = ctx.masked();
        let _ = ZERO_Q.wake_all(pool);
    }
    let _ = wait_for(ctx, 60_000, || WAKE_DONE[0].load(Ordering::Acquire));
    // 4. Switch-safety probes, from this kernel thread.
    {
        let _m = ctx.masked();
        let r = ctx.yield_now();
        record(
            ctx.home(),
            K_PROBE,
            [
                ctx.id().raw(),
                P_MASKED,
                r.err().map_or(0, refusal_code),
                u64::from(r.is_ok()),
                0,
            ],
        );
    }
    {
        static PROBE_LOCK: crate::kernel::lock::SpinLockIrq<()> =
            crate::kernel::lock::SpinLockIrq::new(());
        let _g = PROBE_LOCK.lock();
        let r = ctx.yield_now();
        drop(_g);
        record(
            ctx.home(),
            K_PROBE,
            [
                ctx.id().raw(),
                P_IRQ_GUARD,
                r.err().map_or(0, refusal_code),
                u64::from(r.is_ok()),
                0,
            ],
        );
    }
    // 5. The lost-wakeup probe, on the second CPU (or here on a single CPU).
    let probe_cpu = CpuId(second.unwrap_or(0) as u8);
    let _ = remember(create(ctx, lost_wake_probe, 0, probe_cpu, R_LOSTWAKE));
    // 6. Wait for the alternators.
    let _ = wait_for(ctx, 120_000, || ALT_DONE.load(Ordering::Acquire) >= alts);
    // 7. Create/exit/reuse cycles on the probe CPU, then a stale-id probe.
    let mut last: Option<KThreadId> = None;
    for _ in 0..CYCLES {
        let Some(id) = remember(create(ctx, blip, 0, probe_cpu, R_BLIP)) else {
            break;
        };
        let _ = wait_for(ctx, 20_000, || pool.state_of(id).is_none());
        last = Some(id);
    }
    if let Some(old) = last {
        // A new incarnation takes the slot `old` named (the free list is LIFO) and parks; `old` is
        // stale and must not reach it.
        let reuse = remember(create(ctx, stale_target, 0, probe_cpu, R_BLIP));
        if let Some(r) = reuse {
            let _ = wait_for(ctx, 20_000, || {
                pool.state_of(r) == Some(crate::kernel::kthread::KState::Blocked)
            });
        }
        let w = {
            let _m = ctx.masked();
            pool.wake(old)
        };
        record(
            ctx.home(),
            K_PROBE,
            [
                old.raw(),
                P_STALE_WAKE,
                wake_code(w),
                reuse.map_or(0, |r| r.raw()),
                reuse.map_or(0, |r| pool.state_of(r).map_or(0, |s| s as u64)),
            ],
        );
        STALE_OPEN.store(true, Ordering::Release);
        {
            let _m = ctx.masked();
            STALE_Q.wake_all(pool);
        }
        if let Some(r) = reuse {
            let _ = wait_for(ctx, 20_000, || pool.state_of(r).is_none());
        }
    }
    // 8. Exhaustion and recovery.
    let free_before = pool.free_slots();
    let mut held = 0u64;
    let err;
    loop {
        match create_raw(ctx, holder, held as usize, probe_cpu, R_HOLDER) {
            Ok(id) => {
                let _ = remember(Some(id));
                held += 1;
            }
            Err(crate::kernel::kthread::KCreateError::Exhausted) => {
                err = 1;
                break;
            }
            Err(_) => {
                err = 2;
                break;
            }
        }
    }
    let _ = wait_for(ctx, 20_000, || {
        GATE_QS.iter().map(|q| q.registered() as u64).sum::<u64>() == held
    });
    GATE_OPEN.store(true, Ordering::Release);
    {
        let _m = ctx.masked();
        for q in &GATE_QS {
            q.wake_all(pool);
        }
    }
    let _ = wait_for(ctx, 30_000, || pool.free_slots() == free_before);
    record(
        ctx.home(),
        K_EXHAUST,
        [held, free_before as u64, err, pool.free_slots() as u64, 0],
    );
    // 9. Every witness thread is gone: final states, user progress, then the dump.
    let n = N_CREATED.load(Ordering::Acquire).min(CREATED.len());
    let _ = wait_for(ctx, 30_000, || {
        (0..n).all(|i| {
            KThreadId::from_raw(CREATED[i].load(Ordering::Acquire))
                .is_none_or(|id| pool.state_of(id).is_none())
        })
    });
    for i in 0..n {
        if let Some(id) = KThreadId::from_raw(CREATED[i].load(Ordering::Acquire)) {
            let state = pool.state_of(id).map_or(0, |s| s as u64);
            let role = CREATED_ROLE[i].load(Ordering::Acquire);
            record(ctx.home(), K_FINAL, [id.raw(), state, role, 0, 0]);
        }
    }
    for c in 0..W_CPUS {
        if cpus[c] {
            record(
                ctx.home(),
                K_USER,
                [
                    c as u64,
                    USER_DISPATCHES[c].load(Ordering::Acquire),
                    KT_DISPATCHES[c].load(Ordering::Acquire),
                    pool.cpu_counters(CpuId(c as u8)).timer_interrupts,
                    pool.cpu_counters(CpuId(c as u8)).resched_interrupts,
                ],
            );
        }
    }
    #[cfg(feature = "kthread1-witness")]
    crate::kernel::apsched_witness::publish_s1_stop();
    ACTIVE.store(false, Ordering::Release);
    dump(ctx);
}

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

/// Every record, in sequence order, with a checksum per record and a seal over all of them, dumped
/// twice. The console is shared with raw byte writers that take no line lock (AArch64
/// `activate_asid` writes progress bytes straight to the UART), so a line can arrive damaged. A
/// damaged line fails its checksum and counts for nothing; the record is lost only when both
/// copies are damaged. The seal line carries its own checksum for the same reason.
fn dump(ctx: &KThreadCtx) {
    use core::fmt::Write;
    let total = SEQ.load(Ordering::Acquire);
    let mut overflow = 0u64;
    for r in &RINGS {
        overflow += r.overflow.load(Ordering::Acquire);
    }
    for _pass in 0..2 {
        let (emitted, seal) = dump_pass(ctx, total);
        let mut line = Line {
            buf: [0; 224],
            len: 0,
        };
        let _ = write!(
            line,
            "KT1_SEAL records={} taken={} overflow={} seal=0x{:08x}",
            emitted, total, overflow, seal
        );
        let body = &line.buf[..line.len];
        crate::kernel::printk::printk_emit_sync(format_args!(
            "{} crc=0x{:08x}",
            core::str::from_utf8(body).unwrap_or("KT1_SEAL invalid"),
            fnv(body)
        ));
    }
    crate::kernel::printk::printk_emit_sync(format_args!("KT1_DUMP_END"));
}

/// One pass of the dump: every record in sequence order. Returns the count and the seal.
fn dump_pass(ctx: &KThreadCtx, total: u64) -> (u64, u32) {
    use core::fmt::Write;
    let mut seal: u32 = 0x811C_9DC5;
    let mut emitted = 0u64;
    for seq in 0..total {
        let mut found = None;
        'rings: for r in &RINGS {
            let n = r.next.load(Ordering::Acquire);
            for i in 0..n {
                let s = &r.slots[i];
                if s[7].load(Ordering::Acquire) == 1 && s[0].load(Ordering::Relaxed) == seq {
                    found = Some(s);
                    break 'rings;
                }
            }
        }
        let Some(s) = found else {
            continue;
        };
        // The synchronous dump is long; a scheduling point every few lines lets this CPU's user
        // tasks run when a reschedule is pending (no new record is taken once inactive).
        if seq % 16 == 0 {
            let _ = ctx.sched_point();
        }
        let kc = s[1].load(Ordering::Relaxed);
        let mut line = Line {
            buf: [0; 224],
            len: 0,
        };
        let _ = write!(
            line,
            "KT1_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}",
            seq,
            kind_name(kc & 0xFF),
            (kc >> 8) & 0xFF,
            s[2].load(Ordering::Relaxed),
            s[3].load(Ordering::Relaxed),
            s[4].load(Ordering::Relaxed),
            s[5].load(Ordering::Relaxed),
            s[6].load(Ordering::Relaxed)
        );
        let body = &line.buf[..line.len];
        let crc = fnv(body);
        seal = (seal ^ crc).wrapping_mul(0x0100_0193);
        emitted += 1;
        // Written synchronously, past the log ring: with every CPU logging, the ring drops lines.
        crate::kernel::printk::printk_emit_sync(format_args!(
            "{} crc=0x{:08x}",
            core::str::from_utf8(body).unwrap_or("KT1_REC invalid"),
            crc
        ));
    }
    (emitted, seal)
}
