// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK2 — real contention on the production VM address-space lock on AArch64, and SGI work
//! deferred through the holder's masked hold.
//!
//! The lock is the same instance LOCK1 witnesses on RISC-V: `KernelState::vm_state_lock`, a
//! `SpinLockIrq<()>` of rank 5 that both CPUs reach through the shared mapping transaction
//! (`run_vm_map_transaction`: the guard-page query, then the install). On AArch64 the acquisition's
//! `irq_save` is `mrs daif; msr daifset, #2`, and the restore unmasks only if IRQs were unmasked
//! before; the kernel runs every exception with `PSTATE.I` set from entry, so the holder's critical
//! section is masked and an SGI made pending to it is taken only once the holder has released the
//! lock and returned to EL0 (or reached the idle window's `daifclr`).
//!
//! The interrupt evidence is derived from the GICv2, not copied from RISC-V's mailbox:
//!
//! * **publication** — the waiter, after it saw the holder inside its masked ownership, calls the
//!   production `smp::send_reschedule_sgi` (the `GICD_SGIR` write a remote wake uses);
//! * **outstanding under the mask** — the holder reads its own banked `GICD_SPENDSGIR` byte for the
//!   reschedule SGI (one pending bit per SOURCE interface; a read has no side effect) once when its
//!   hold begins and again after it saw the waiter's contention. The waiter's source bit clear at the
//!   start and set later is the controller holding work published during this masked hold. Repeat
//!   sends from the same source coalesce into that one pending bit (GICv2's rule), so the claim
//!   discharges every one of them;
//! * **claim** — the vector entry's `irq::claim_interrupt` reads `GICC_IAR`; the one reader
//!   (`gic_read_iar`) hands this module the token exactly as returned (INTID + source interface).
//!   Only a reschedule-SGI claim from the linked source discharges the round;
//! * **completion** — the vector tail's `irq::complete_interrupt` writes the token to `GICC_EOIR`;
//!   the one writer (`gic_write_eoir`) hands this module the token, which must be exactly the
//!   discharging claim's.
//!
//! The witness never reads `GICC_IAR` itself, never writes `GICC_EOIR` or `GICD_SGIR` except through
//! the production send owner, never synthesizes a claim and never runs from an IRQ handler that
//! could need `vm_state_lock`. All storage is preallocated atomics; the lock path is bounded,
//! lock-free and console-free; the only console output is the synchronous dump after both witness
//! tasks finished.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// The witness id stamped on `vm_state_lock` (and on nothing else).
pub const VM_LOCK_ID: u32 = 1;

/// Mutual rounds the LOCK2 build runs (`YARM_MUT_ROUNDS` / `smp2_record::MUT_ROUNDS`). Six per
/// direction.
pub const LOCK2_ROUNDS: u64 = 12;

/// Bounds on the hold hook's extension, on the waiter gate, and on the distributor observation
/// (which only covers the `GICD_SGIR` write landing at the distributor — never delivery: the holder
/// stays masked throughout and the loop exits the instant the bit is seen).
const HOLD_SPINS: u64 = 20_000_000;
const GATE_SPINS: u64 = 40_000_000;
const PEND_SPINS: u64 = 10_000_000;
/// Bound on the dump's wait for the other CPU to complete an interrupt it has claimed.
const QUIESCE_SPINS: u64 = 50_000_000;

const SLOTS: usize = 1024;
const MAX_CPU: usize = 8;
/// GICv2 special INTIDs (1020..=1023) — nothing to complete.
const FIRST_SPECIAL: u32 = 1020;

// ── event kinds (`hart` is always the recording CPU) ─────────────────────────────────────────
/// `[lock_id, round, acq, 0, 0]` — recorded AFTER the successful CAS.
pub const K_ACQUIRE: u8 = 1;
/// `[lock_id, round, k, 0, 0]` — the acquiring CPU observed the lock HELD; `k` is the contention
/// counter value this observation produced.
pub const K_CONTENDED: u8 = 2;
/// `[lock_id, round_of_acq, acq, 0, 0]` — release INTENT, recorded BEFORE the unlocking store.
pub const K_RELEASE: u8 = 3;
/// `[lock_id, round, acq, k_seen, irq_unmasked | baseline<<8]` — the holder, inside acquisition
/// `acq`, saw the contention counter reach `k_seen > baseline` (0 if the bounded wait elapsed);
/// `irq_unmasked` is `PSTATE.I == 0` read there (must be 0).
pub const K_HOLD: u8 = 4;
/// `[round, target, ok, sgir, sender_sent_after]` — the waiter's production reschedule SGI to the
/// holder: the `GICD_SGIR` value the send owner wrote and its per-sender send count afterwards.
pub const K_SGI: u8 = 5;
/// `[link_round, token, link_mask, discharged, kind]` — a `GICC_IAR` claim on a CPU with an armed
/// link (`kind` 0 = reschedule SGI, 1 = other INTID, 2 = special).
pub const K_CLAIM: u8 = 6;
/// `[round, sender, pre | post<<16 | sender_mask<<32, outstanding, acq]` — the masked holder's two
/// distributor views inside acquisition `acq` (`view = sources | active<<8 | pending<<9 |
/// valid<<10`), taken when its hold began and after it saw the contention.
pub const K_PENDING: u8 = 7;
/// `[round, holder, role, ok, arrived]` — a CPU passed the round gate (role 0 = holder, 1 = waiter;
/// `ok` = the waiter saw the holder inside its ownership; `arrived` = the holder saw the waiter at
/// its gate before taking the lock).
pub const K_GATE: u8 = 8;
/// `[round, 0, 0, 0, 0]` — a CPU completed its mapping operation for the round (`*_MUT_OK`).
pub const K_DONE: u8 = 9;
/// `[old_round, new_round, 0, 0, 0]` — arming found an UNRESOLVED link on this CPU.
pub const K_LINKLOST: u8 = 10;
/// `[round, completed_token, claimed_token, 0, 0]` — the `GICC_EOIR` write that followed a
/// discharging claim on this CPU.
pub const K_COMPLETE: u8 = 11;

// ── the bounded event ring ──────────────────────────────────────────────────────────────────
struct Slot {
    state: AtomicU8,
    kind: AtomicU8,
    hart: AtomicU8,
    f: [AtomicU64; 5],
}
#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: Slot = Slot {
    state: AtomicU8::new(0),
    kind: AtomicU8::new(0),
    hart: AtomicU8::new(0),
    f: [const { AtomicU64::new(0) }; 5],
};
static SLOT: [Slot; SLOTS] = [EMPTY; SLOTS];
static NEXT: AtomicU32 = AtomicU32::new(0);
static OVERFLOW: AtomicU32 = AtomicU32::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// ── per-round contention state (identical in meaning to LOCK1's) ───────────────────────────────
static ROUND: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOLDER_CPU: AtomicU8 = AtomicU8::new(0xff);
static WAITER_CPU: AtomicU8 = AtomicU8::new(0xff);
static HELD_ROUND: AtomicU64 = AtomicU64::new(0);
/// Set by the waiter on entering its gate — after its marker syscall copied the step name from user
/// memory (which itself takes `vm_state_lock`). The holder's gate waits for it, so the waiter's
/// only lock() call while the holder owns the lock is its mapping syscall, issued after it published.
static WAITER_ARRIVED: AtomicU64 = AtomicU64::new(0);
static CONTENTION_SEQ: AtomicU64 = AtomicU64::new(0);
static ROUND_OK: AtomicU8 = AtomicU8::new(0);
static LAST_HELD_ROUND: AtomicU64 = AtomicU64::new(0);
static ACQ_NEXT: AtomicU64 = AtomicU64::new(0);

// ── controller obligations ───────────────────────────────────────────────────────────────────
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U64: AtomicU64 = AtomicU64::new(0);
/// `LINK[cpu]`: `round | sender_mask<<16` (0 = none) — armed by this CPU's hold while masked,
/// discharged only by this CPU's claim of the reschedule SGI from that source interface.
static LINK: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];
/// `INFLIGHT[cpu]`: `token | round<<32 | 1<<63` of the discharging claim, until its completion.
static INFLIGHT: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_ROW3: [AtomicU64; 3] = [ZERO_U64; 3];
/// `[cpu][kind]` claims and completions since arming (kind 0 = reschedule SGI, 1 = other, 2 = special).
static CLAIMS: [[AtomicU64; 3]; MAX_CPU] = [ZERO_ROW3; MAX_CPU];
static COMPLETIONS: [[AtomicU64; 3]; MAX_CPU] = [ZERO_ROW3; MAX_CPU];

pub fn arm() {
    ARMED.store(true, Ordering::Release);
}

fn record(kind: u8, hart: u8, f: [u64; 5]) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let i = NEXT.fetch_add(1, Ordering::AcqRel) as usize;
    let Some(slot) = SLOT.get(i) else {
        OVERFLOW.fetch_add(1, Ordering::AcqRel);
        return;
    };
    slot.kind.store(kind, Ordering::Relaxed);
    slot.hart.store(hart, Ordering::Relaxed);
    for (d, v) in slot.f.iter().zip(f) {
        d.store(v, Ordering::Relaxed);
    }
    slot.state.store(2, Ordering::Release);
}

#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn this_cpu() -> u8 {
    (crate::arch::aarch64::read_mpidr_el1() & 0xff) as u8
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn this_cpu() -> u8 {
    0
}

/// `PSTATE.I == 0` on this CPU (IRQ delivery unmasked). One system-register read.
#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn irq_unmasked() -> u64 {
    let daif: u64;
    // SAFETY: a plain system-register read.
    unsafe {
        core::arch::asm!("mrs {0}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    u64::from(daif & (1 << 7) == 0)
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn irq_unmasked() -> u64 {
    0
}

/// This CPU's distributor view of the reschedule SGI, packed (`valid` = the controller is known).
#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn view() -> u64 {
    crate::arch::aarch64::smp::sgi_pending_view().map_or(0, |(sources, active, pending)| {
        u64::from(sources) | u64::from(active) << 8 | u64::from(pending) << 9 | 1 << 10
    })
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn view() -> u64 {
    0
}

#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn interface_mask(cpu: u8) -> u64 {
    u64::from(crate::arch::aarch64::smp::interface_mask(
        crate::kernel::scheduler::CpuId(cpu),
    ))
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn interface_mask(_cpu: u8) -> u64 {
    0
}

#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn resched_intid() -> u32 {
    u32::from(crate::arch::gicv2_sgi::RESCHEDULE_SGI_INTID)
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn resched_intid() -> u32 {
    1
}

/// This CPU's production reschedule-SGI send count (`GICD_SGIR` writes by the send owner).
#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn sgi_sent(cpu: u8) -> u64 {
    u64::from(crate::arch::aarch64::smp::sgi_counters(crate::kernel::scheduler::CpuId(cpu)).4)
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn sgi_sent(_cpu: u8) -> u64 {
    0
}

fn claim_kind(token: u32) -> usize {
    let intid = token & 0x3ff;
    if intid >= FIRST_SPECIAL {
        2
    } else if intid == resched_intid() {
        0
    } else {
        1
    }
}

// ── the lock-path hooks (called from `SpinLockIrq` for the witnessed instance only) ──────────

/// The acquisition observed the lock already HELD. Recorded once per `lock()` call, inside an open
/// round window.
pub fn note_contended(id: u32) {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let k = CONTENTION_SEQ.fetch_add(1, Ordering::AcqRel) + 1;
    record(
        K_CONTENDED,
        this_cpu(),
        [u64::from(id), ROUND.load(Ordering::Acquire), k, 0, 0],
    );
}

/// The CAS succeeded. Returns the acquisition token the guard keeps (`round << 32 | acq`, or 0 when
/// the window is closed and nothing is recorded).
pub fn note_acquired(id: u32) -> u64 {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return 0;
    }
    let round = ROUND.load(Ordering::Acquire);
    let acq = ACQ_NEXT.fetch_add(1, Ordering::AcqRel) + 1;
    record(K_ACQUIRE, this_cpu(), [u64::from(id), round, acq, 0, 0]);
    (round << 32) | (acq & 0xffff_ffff)
}

/// Release INTENT (the unlocking store follows), for exactly the acquisitions recorded.
pub fn note_released(id: u32, token: u64) {
    if id != VM_LOCK_ID || token == 0 {
        return;
    }
    record(
        K_RELEASE,
        this_cpu(),
        [u64::from(id), token >> 32, token & 0xffff_ffff, 0, 0],
    );
}

/// The default-off, bounded hold hook: only the round's designated holder, only its first recorded
/// acquisition of the round, holding the lock with IRQs masked. Reads the distributor view, opens
/// the waiter's gate, waits (bounded) for the waiter's contention, reads the view again (bounded
/// poll for the waiter's source bit), records the masked state and both views, and arms the round's
/// delivery link only if the controller holds a request from the waiter that was NOT pending when
/// the hold began. It never waits for delivery, a completion, a syscall or a remote ACK.
pub fn maybe_hold(id: u32, token: u64) {
    if id != VM_LOCK_ID || token == 0 || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let me = this_cpu();
    if me != HOLDER_CPU.load(Ordering::Acquire) {
        return;
    }
    let round = token >> 32;
    let acq = token & 0xffff_ffff;
    if round != ROUND.load(Ordering::Acquire) || LAST_HELD_ROUND.load(Ordering::Acquire) >= round {
        return;
    }
    LAST_HELD_ROUND.store(round, Ordering::Release);
    let sender = WAITER_CPU.load(Ordering::Acquire);
    let mask = interface_mask(sender) & 0xff;
    let pre = view();
    let baseline = CONTENTION_SEQ.load(Ordering::Acquire);
    // Release the waiter's gate: the holder is inside the critical section (after its CAS).
    HELD_ROUND.store(round, Ordering::Release);
    let mut left = HOLD_SPINS;
    let mut seen = CONTENTION_SEQ.load(Ordering::Acquire);
    while seen <= baseline {
        if left == 0 {
            break;
        }
        left -= 1;
        core::hint::spin_loop();
        seen = CONTENTION_SEQ.load(Ordering::Acquire);
    }
    let k_seen = if seen > baseline { seen } else { 0 };
    let mut post = view();
    let mut polls = PEND_SPINS;
    while post & mask == 0 && polls != 0 {
        polls -= 1;
        core::hint::spin_loop();
        post = view();
    }
    record(
        K_HOLD,
        me,
        [
            u64::from(id),
            round,
            acq,
            k_seen,
            irq_unmasked() | ((baseline & 0xffff_ffff) << 8),
        ],
    );
    let valid = (pre >> 10) & (post >> 10) & 1;
    let outstanding = u64::from(
        valid == 1 && mask != 0 && pre & mask == 0 && post & mask != 0 && (post >> 8) & 1 == 0,
    );
    record(
        K_PENDING,
        me,
        [
            round,
            u64::from(sender),
            pre | (post << 16) | (mask << 32),
            outstanding,
            acq,
        ],
    );
    if outstanding == 0 {
        return;
    }
    if let Some(cell) = LINK.get(me as usize) {
        let old = cell.swap(round | (mask << 16), Ordering::AcqRel);
        if old != 0 {
            record(K_LINKLOST, me, [old & 0xffff, round, 0, 0, 0]);
        }
    }
}

// ── the controller hooks (called from the one `GICC_IAR` reader and `GICC_EOIR` writer) ───────

/// A claim: `token` is the `GICC_IAR` value exactly as returned. Counted; recorded on a CPU with an
/// armed link, which it DISCHARGES only if it is the reschedule SGI from the linked source.
pub fn note_claim(token: u32) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let cpu = this_cpu();
    let kind = claim_kind(token);
    if let Some(row) = CLAIMS.get(cpu as usize) {
        row[kind].fetch_add(1, Ordering::AcqRel);
    }
    let Some(cell) = LINK.get(cpu as usize) else {
        return;
    };
    let link = cell.load(Ordering::Acquire);
    if link == 0 {
        return;
    }
    let round = link & 0xffff;
    let mask = (link >> 16) & 0xff;
    let source = (token >> 10) & 0x7;
    let discharged = u64::from(kind == 0 && (1u64 << source) == mask);
    if discharged == 1 {
        // Only this CPU arms or discharges its own link and in-flight claim.
        cell.store(0, Ordering::Release);
        if let Some(f) = INFLIGHT.get(cpu as usize) {
            f.store(
                u64::from(token) | (round << 32) | (1 << 63),
                Ordering::Release,
            );
        }
    }
    record(
        K_CLAIM,
        cpu,
        [round, u64::from(token), mask, discharged, kind as u64],
    );
}

/// A completion: `token` was just written to `GICC_EOIR`. Counted; recorded if it follows a
/// discharging claim on this CPU (naming both tokens, so a mismatch is visible).
pub fn note_completion(token: u32) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let cpu = this_cpu();
    if let Some(row) = COMPLETIONS.get(cpu as usize) {
        row[claim_kind(token)].fetch_add(1, Ordering::AcqRel);
    }
    let Some(f) = INFLIGHT.get(cpu as usize) else {
        return;
    };
    let inflight = f.swap(0, Ordering::AcqRel);
    if inflight == 0 {
        return;
    }
    record(
        K_COMPLETE,
        cpu,
        [
            (inflight >> 32) & 0x7fff_ffff,
            u64::from(token),
            inflight & 0xffff_ffff,
            0,
            0,
        ],
    );
}

// ── the per-round direction gate (called from the SMP2 marker hook on `*_MUT_NR3`) ───────────

/// Open round `round`'s window. The designated holder waits, bounded, until the waiter has entered
/// its gate (`WAITER_ARRIVED`), records whether it did, and returns to take the lock. The waiter
/// marks its arrival, waits, bounded, until the holder is inside its masked ownership
/// (`HELD_ROUND`), records whether that held, and only then publishes the production reschedule SGI
/// to the holder — before it returns to EL0 and issues its own mapping syscall, contending on the
/// unchanged acquisition path. No domain lock is held here (the off-lock DebugLog path); each wait is
/// for an atomic the other side sets without holding anything this side needs.
pub fn mut_round_gate(cpu: u8, round: u64) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let holder = if round % 2 == 1 { 0u8 } else { 1u8 };
    let waiter = holder ^ 1;
    HOLDER_CPU.store(holder, Ordering::Release);
    WAITER_CPU.store(waiter, Ordering::Release);
    ROUND.store(round, Ordering::Release);
    ROUND_OK.store(0, Ordering::Release);
    ACTIVE.store(true, Ordering::Release);
    if cpu == holder {
        let mut left = GATE_SPINS;
        while WAITER_ARRIVED.load(Ordering::Acquire) < round {
            if left == 0 {
                break;
            }
            left -= 1;
            core::hint::spin_loop();
        }
        let arrived = u64::from(WAITER_ARRIVED.load(Ordering::Acquire) >= round);
        record(K_GATE, cpu, [round, u64::from(holder), 0, 1, arrived]);
        return;
    }
    WAITER_ARRIVED.store(round, Ordering::Release);
    let mut left = GATE_SPINS;
    while HELD_ROUND.load(Ordering::Acquire) < round {
        if left == 0 {
            break;
        }
        left -= 1;
        core::hint::spin_loop();
    }
    let ok = u64::from(HELD_ROUND.load(Ordering::Acquire) >= round);
    record(K_GATE, cpu, [round, u64::from(holder), 1, ok, 0]);
    publish_sgi_to_holder(cpu, holder, round);
}

/// Publish the production reschedule SGI from the waiter to the holder through the send owner a
/// remote wake uses (`GICD_SGIR`). Records the value written and this sender's send count after.
/// Never waits for delivery.
#[cfg(all(target_arch = "aarch64", not(feature = "hosted-dev")))]
fn publish_sgi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    use crate::kernel::scheduler::CpuId;
    let sent = crate::arch::aarch64::smp::send_reschedule_sgi(CpuId(waiter_cpu), CpuId(holder_cpu));
    let (ok, sgir) = match sent {
        Ok(sgir) => (1, u64::from(sgir)),
        Err(_) => (0, 0),
    };
    let after = sgi_sent(waiter_cpu);
    record(
        K_SGI,
        waiter_cpu,
        [round, u64::from(holder_cpu), ok, sgir, after],
    );
}
#[cfg(not(all(target_arch = "aarch64", not(feature = "hosted-dev"))))]
fn publish_sgi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    record(K_SGI, waiter_cpu, [round, u64::from(holder_cpu), 0, 0, 0]);
}

/// A CPU finished its round (`*_MUT_OK`): record it, and close the window once both have.
pub fn note_round_ok(round: u64) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    record(K_DONE, this_cpu(), [round, 0, 0, 0, 0]);
    if round != ROUND.load(Ordering::Acquire) {
        return;
    }
    if ROUND_OK.fetch_add(1, Ordering::AcqRel) + 1 >= 2 {
        ACTIVE.store(false, Ordering::Release);
    }
}

// ── the sealed dump (called from the SMP2 dump, after both tasks finished) ────────────────────

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn counts(cpu: usize) -> ([u64; 3], [u64; 3]) {
    let load = |row: &[AtomicU64; 3]| {
        [
            row[0].load(Ordering::Acquire),
            row[1].load(Ordering::Acquire),
            row[2].load(Ordering::Acquire),
        ]
    };
    (load(&CLAIMS[cpu]), load(&COMPLETIONS[cpu]))
}

/// Print every event (twice, each line with its own checksum), the per-CPU controller counts and the
/// completion record. Before printing, waits (bounded) for the other CPU to complete any interrupt
/// it has claimed, so the counts are a settled snapshot; `settled=0` reports a bound reached.
pub fn dump() {
    let me = this_cpu() as usize;
    let mut settled = [1u64; 2];
    for (cpu, s) in settled.iter_mut().enumerate() {
        if cpu == me {
            continue;
        }
        let mut left = QUIESCE_SPINS;
        loop {
            let (c, d) = counts(cpu);
            if c[0] == d[0] && c[1] == d[1] {
                break;
            }
            if left == 0 {
                *s = 0;
                break;
            }
            left -= 1;
            core::hint::spin_loop();
        }
    }
    let n = (NEXT.load(Ordering::Acquire) as usize).min(SLOTS);
    let overflow = OVERFLOW.load(Ordering::Acquire) != 0;
    let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::with_capacity(n + 4);
    lines.push(alloc::format!(
        "LOCK2_META vm_lock_id={} rounds={} slots_used={} overflow={} dump_cpu={} sgi_intid={} if0=0x{:x} if1=0x{:x}",
        VM_LOCK_ID,
        LOCK2_ROUNDS,
        n,
        u8::from(overflow),
        me,
        resched_intid(),
        interface_mask(0),
        interface_mask(1)
    ));
    for (i, slot) in SLOT.iter().enumerate().take(n) {
        let mut left = 10_000_000u64;
        while slot.state.load(Ordering::Acquire) != 2 && left != 0 {
            left -= 1;
            core::hint::spin_loop();
        }
        let name = match slot.kind.load(Ordering::Relaxed) {
            K_ACQUIRE => "acquire",
            K_CONTENDED => "contended",
            K_RELEASE => "release",
            K_HOLD => "hold",
            K_SGI => "sgi",
            K_CLAIM => "claim",
            K_PENDING => "pending",
            K_GATE => "gate",
            K_DONE => "done",
            K_LINKLOST => "linklost",
            K_COMPLETE => "complete",
            _ => "unknown",
        };
        lines.push(alloc::format!(
            "LOCK2_REC seq={} kind={} hart={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}",
            i,
            name,
            slot.hart.load(Ordering::Relaxed),
            slot.f[0].load(Ordering::Relaxed),
            slot.f[1].load(Ordering::Relaxed),
            slot.f[2].load(Ordering::Relaxed),
            slot.f[3].load(Ordering::Relaxed),
            slot.f[4].load(Ordering::Relaxed),
        ));
    }
    for (cpu, s) in settled.iter().enumerate() {
        let (c, d) = counts(cpu);
        let sent = sgi_sent(cpu as u8);
        lines.push(alloc::format!(
            "LOCK2_COUNTS cpu={} sgi_claim={} other_claim={} special_claim={} sgi_eoi={} other_eoi={} special_eoi={} sgi_sent={} settled={}",
            cpu, c[0], c[1], c[2], d[0], d[1], d[2], sent, s
        ));
    }
    lines.push(alloc::format!("LOCK2_DUMP_DONE records={}", n));
    for pass in 1..=2u32 {
        for line in &lines {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "{} pass={} crc=0x{:08x}",
                line,
                pass,
                fnv1a(line.as_bytes())
            ));
        }
    }
}
