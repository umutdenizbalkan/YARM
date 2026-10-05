// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK1 — real subdomain-lock contention and interrupt progress.
//!
//! The chosen lock is the production VM address-space lock (`KernelState::vm_state_lock`, a
//! `SpinLockIrq<()>`, rank 5), which serialises every mutation of `user_spaces`. In the SMP3
//! two-hart VM/IPI workload both harts reach it through the mapping transaction's install phase
//! (`run_vm_map_transaction` → `install_and_account` → `with_vm_then_memory_split_mut` →
//! `vm_state_lock.lock()`), so both genuinely acquire the same instance. Being a `SpinLockIrq`, the
//! whole critical section runs with supervisor interrupts masked (`sstatus.SIE = 0`): a software
//! interrupt published to the holder stays pending (`sip.SSIP = 1`) and is delivered only after the
//! holder returns to a point where `SIE` is set — exactly the masking contract §3 requires.
//!
//! This module records the **atomic** acquire/contend/release events of that one lock (never any
//! other `SpinLockIrq`, selected by a non-zero witness id), a default-off bounded hold hook that
//! makes a contended acquisition reproducible without waiting on any held-lock work, a per-round
//! direction gate, and a sealed `LOCK1_REC` dump an independent grader re-derives from. Everything
//! here is bounded, preallocated, lock-free and console-free on the lock path; the only console
//! output is the one synchronous dump after both witness tasks have finished.
//!
//! All storage is static atomics; nothing allocates, recurses, or takes another domain lock inside
//! the lock path.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// The witness id stamped on `vm_state_lock` (and on nothing else). Any non-zero value works; it
/// only has to be distinct from the 0 every other lock carries.
pub const VM_LOCK_ID: u32 = 1;

/// Mutual rounds the LOCK1 build runs (see `MUT_ROUNDS` / `YARM_MUT_ROUNDS`). Six per direction.
pub const LOCK1_ROUNDS: u64 = 12;

/// A bound on the hold hook's extension and on the waiter gate. Large enough that the contender
/// reliably arrives under TCG, finite so the hook releases even if it never does.
const HOLD_SPINS: u64 = 20_000_000;
const GATE_SPINS: u64 = 40_000_000;
/// Bounded observation of the waiter's IPI becoming pending (`sip.SSIP`) while the holder stays
/// masked. Small — it only covers the firmware's M→S reflection latency, never interrupt delivery.
const SSIP_SPINS: u64 = 5_000_000;

const SLOTS: usize = 1024;

// ── event kinds ─────────────────────────────────────────────────────────────────────────────
pub const K_ACQUIRE: u8 = 1;
pub const K_CONTENDED: u8 = 2;
pub const K_RELEASE: u8 = 3;
pub const K_HOLD: u8 = 4;
pub const K_IPI: u8 = 5;

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

// ── per-round contention state ──────────────────────────────────────────────────────────────
/// The mutual round currently under the gate, and whether its window is open. Lock events are
/// recorded only while open, and tagged with this round — so the P1/P2/P3 acquisitions that
/// precede the first gate, and anything after the last round completes, are never recorded.
static ROUND: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The logical CPU designated to hold first this round (the other hart is the waiter). Parity of
/// the round alternates it, giving six rounds with each hart as the contender.
static HOLDER_CPU: AtomicU8 = AtomicU8::new(0xff);
/// Set by the holder's hold hook once it is inside the critical section; the waiter's gate waits
/// for it so the holder reliably wins the race and the waiter reliably contends. Monotonic.
static HELD_ROUND: AtomicU64 = AtomicU64::new(0);
/// Bumped by the waiter when it observes the lock HELD; the hold hook watches this advance as its
/// atomic contention indication, then releases.
static CONTENTION_SEQ: AtomicU64 = AtomicU64::new(0);
/// Per-round MUT_OK completions seen; the window closes when both harts have finished the round.
static ROUND_OK: AtomicU8 = AtomicU8::new(0);
/// The last round whose holder actually extended its critical section. The install transaction
/// touches `vm_state_lock` more than once per request (the guard-page query, then the install), so
/// the hold hook extends only the FIRST acquisition of each round; later ones acquire and release
/// at production speed.
static LAST_HELD_ROUND: AtomicU64 = AtomicU64::new(0);

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

#[cfg(all(target_arch = "riscv64", not(feature = "hosted-dev")))]
fn this_cpu() -> u8 {
    crate::arch::riscv64::boot::riscv_current_logical_cpu().0
}
#[cfg(not(all(target_arch = "riscv64", not(feature = "hosted-dev"))))]
fn this_cpu() -> u8 {
    0
}

/// `(sstatus.SIE, sip.SSIP)` on the current hart — the masked state and the pending software
/// interrupt. One CSR read each; no effect on the CSRs.
#[cfg(all(target_arch = "riscv64", not(feature = "hosted-dev")))]
fn sie_ssip() -> (u64, u64) {
    let (sstatus, sip): (u64, u64);
    // SAFETY: two plain CSR reads, no memory touched.
    unsafe {
        core::arch::asm!("csrr {0}, sstatus", out(reg) sstatus, options(nomem, nostack, preserves_flags));
        core::arch::asm!("csrr {0}, sip", out(reg) sip, options(nomem, nostack, preserves_flags));
    }
    ((sstatus >> 1) & 1, (sip >> 1) & 1)
}
#[cfg(not(all(target_arch = "riscv64", not(feature = "hosted-dev"))))]
fn sie_ssip() -> (u64, u64) {
    (0, 0)
}

// ── the lock-path hooks (called from `SpinLockIrq` for the witnessed instance only) ──────────

/// The acquisition observed the lock already HELD — the actual failed acquisition. Recorded once
/// per `lock()` call, and only inside an open round window.
pub fn note_contended(id: u32) {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let seq = CONTENTION_SEQ.fetch_add(1, Ordering::AcqRel) + 1;
    record(
        K_CONTENDED,
        this_cpu(),
        [
            u64::from(id),
            ROUND.load(Ordering::Acquire),
            u64::from(HOLDER_CPU.load(Ordering::Acquire)),
            seq,
            0,
        ],
    );
}

/// The CAS succeeded — this hart now holds the lock.
pub fn note_acquired(id: u32) {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    record(
        K_ACQUIRE,
        this_cpu(),
        [
            u64::from(id),
            ROUND.load(Ordering::Acquire),
            u64::from(HOLDER_CPU.load(Ordering::Acquire)),
            0,
            0,
        ],
    );
}

/// The guard is about to release the lock.
pub fn note_released(id: u32) {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    record(
        K_RELEASE,
        this_cpu(),
        [u64::from(id), ROUND.load(Ordering::Acquire), 0, 0, 0],
    );
}

/// The default-off, bounded hold hook. Runs holding the lock, with `SIE` masked, only for the
/// round's designated holder. It makes the window reproducible by waiting — bounded — until the
/// contender has actually observed the lock HELD (an atomic indication), then records the holder's
/// masked/pending state and returns. It never waits for held-lock work, interrupt delivery, a
/// syscall completion or a remote ACK, and it releases when the bound elapses even if the
/// contender never arrives.
pub fn maybe_hold(id: u32) {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    if this_cpu() != HOLDER_CPU.load(Ordering::Acquire) {
        return;
    }
    let round = ROUND.load(Ordering::Acquire);
    // Extend only the first held acquisition of the round. Only the designated holder hart reaches
    // here for a given round, and within that hart the acquisitions are sequential, so a plain
    // load/store is a sound single-writer guard.
    if LAST_HELD_ROUND.load(Ordering::Acquire) >= round {
        return;
    }
    LAST_HELD_ROUND.store(round, Ordering::Release);
    let baseline = CONTENTION_SEQ.load(Ordering::Acquire);
    // Release the waiter's gate: the holder is now inside the critical section.
    HELD_ROUND.store(round, Ordering::Release);
    let mut left = HOLD_SPINS;
    while CONTENTION_SEQ.load(Ordering::Acquire) <= baseline {
        if left == 0 {
            break;
        }
        left -= 1;
        core::hint::spin_loop();
    }
    // Observe the waiter's IPI become PENDING on this (still masked) hart — the atomic `sip.SSIP`
    // indication, bounded. This reads a bit the firmware set; it never takes the interrupt (SIE
    // stays 0), so it is not waiting for delivery. If the reflection has not landed inside the
    // bound the round still credits on SIE being masked and the IPI published; the pending read is
    // recorded either way.
    let mut spins = SSIP_SPINS;
    let mut ssip;
    loop {
        let (_, p) = sie_ssip();
        ssip = p;
        if ssip != 0 || spins == 0 {
            break;
        }
        spins -= 1;
        core::hint::spin_loop();
    }
    let (sie, _) = sie_ssip();
    record(
        K_HOLD,
        this_cpu(),
        [
            u64::from(id),
            round,
            sie,
            ssip,
            u64::from(HOLDER_CPU.load(Ordering::Acquire)),
        ],
    );
}

// ── the per-round direction gate (called from the SMP3 marker hook on `*_MUT_NR3`) ───────────

/// Open round `round`'s window and order entry so the designated holder acquires first. The holder
/// returns at once; the waiter publishes a production reschedule IPI to the holder (so a real
/// software interrupt is pending while the holder sits in the masked critical section) and then
/// waits, bounded, until the holder is inside the section before it issues its own mapping syscall
/// and contends on the unchanged production acquisition path.
///
/// `cpu` is the hart running its `*_MUT_NR3` step; `round` is the mutual round. No domain lock is
/// held here (this runs on the off-lock DebugLog path), and the only wait is for the atomic
/// `HELD_ROUND`, which the holder sets without holding anything the waiter needs.
pub fn mut_round_gate(cpu: u8, round: u64) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let holder = if round % 2 == 1 { 0u8 } else { 1u8 };
    // Open the window for this round. Idempotent across the two harts' gate calls.
    HOLDER_CPU.store(holder, Ordering::Release);
    ROUND.store(round, Ordering::Release);
    ROUND_OK.store(0, Ordering::Release);
    ACTIVE.store(true, Ordering::Release);
    if cpu == holder {
        // The holder proceeds immediately to acquire the lock first.
        return;
    }
    // The waiter: publish the §3 IPI to the holder, then wait for the holder to take the lock.
    publish_ipi_to_holder(cpu, holder, round);
    let mut left = GATE_SPINS;
    while HELD_ROUND.load(Ordering::Acquire) < round {
        if left == 0 {
            break;
        }
        left -= 1;
        core::hint::spin_loop();
    }
}

/// Publish a production reschedule IPI from the waiter hart to the holder hart, through the exact
/// send/mailbox owner a remote wake uses. Records the outcome; never waits for delivery or an ACK.
#[cfg(all(target_arch = "riscv64", not(feature = "hosted-dev")))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    use crate::kernel::scheduler::CpuId;
    let ok =
        crate::arch::riscv64::ipi::send_reschedule(CpuId(waiter_cpu), CpuId(holder_cpu)).is_ok();
    record(
        K_IPI,
        waiter_cpu,
        [
            round,
            u64::from(holder_cpu),
            u64::from(ok),
            u64::from(waiter_cpu),
            0,
        ],
    );
}
#[cfg(not(all(target_arch = "riscv64", not(feature = "hosted-dev"))))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    record(
        K_IPI,
        waiter_cpu,
        [round, u64::from(holder_cpu), 0, u64::from(waiter_cpu), 0],
    );
}

/// Close the current round's window once both harts have finished it (`*_MUT_OK`). Called from the
/// SMP3 marker hook.
pub fn note_round_ok(round: u64) {
    if !ARMED.load(Ordering::Acquire) || round != ROUND.load(Ordering::Acquire) {
        return;
    }
    if ROUND_OK.fetch_add(1, Ordering::AcqRel) + 1 >= 2 {
        ACTIVE.store(false, Ordering::Release);
    }
}

// ── the sealed dump (called from the SMP3 dump, after both tasks finished) ────────────────────

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Print every lock event (twice, each with its own checksum) plus a one-line summary. Synchronous,
/// so no line is lost to another hart's mid-line console write.
pub fn dump() {
    let n = (NEXT.load(Ordering::Acquire) as usize).min(SLOTS);
    let overflow = OVERFLOW.load(Ordering::Acquire) != 0;
    for pass in 1..=2u32 {
        let emit = |line: alloc::string::String| {
            crate::kernel::printk::printk_emit_sync(format_args!(
                "{} pass={} crc=0x{:08x}",
                line,
                pass,
                fnv1a(line.as_bytes())
            ));
        };
        emit(alloc::format!(
            "LOCK1_META vm_lock_id={} rounds={} slots_used={} overflow={} dump_cpu={}",
            VM_LOCK_ID,
            LOCK1_ROUNDS,
            n,
            u8::from(overflow),
            this_cpu()
        ));
        for i in 0..n {
            let slot = &SLOT[i];
            // Bounded wait for the publisher's release store.
            let mut left = 10_000_000u64;
            while slot.state.load(Ordering::Acquire) != 2 && left != 0 {
                left -= 1;
                core::hint::spin_loop();
            }
            let kind = slot.kind.load(Ordering::Relaxed);
            let name = match kind {
                K_ACQUIRE => "acquire",
                K_CONTENDED => "contended",
                K_RELEASE => "release",
                K_HOLD => "hold",
                K_IPI => "ipi",
                _ => "unknown",
            };
            emit(alloc::format!(
                "LOCK1_REC seq={} kind={} hart={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}",
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
        emit(alloc::format!("LOCK1_DUMP_DONE records={}", n));
    }
}
