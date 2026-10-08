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
//! This module records the acquire/contend/release events of that one lock (never any other
//! `SpinLockIrq`, selected by a non-zero witness id), a default-off bounded hold hook that makes a
//! contended acquisition reproducible without waiting on any held-lock work, a per-round direction
//! gate, and a sealed `LOCK1_REC` dump an independent grader re-derives from. Everything here is
//! bounded, preallocated, lock-free and console-free on the lock path; the only console output is
//! the one synchronous dump after both witness tasks have finished.
//!
//! QEMU-LOCK1-SEAL — the record carries what the grader needs to validate the complete ownership
//! history and to attribute the consumed IPI work, without changing the production algorithm:
//!
//! * every recorded acquisition has an identity, carried by the guard to its release record (an
//!   acquire is recorded after the CAS, a release intent before the unlocking store);
//! * contention is linked to the holder by the contention-counter VALUE it saw while it owned the
//!   lock, not by record order (an observer callback may land after the atomic it describes);
//! * the waiter publishes only after it saw the holder inside its masked ownership; the publication
//!   owner advances a (target, sender) generation before setting the mailbox bit; the masked holder
//!   positively reads its own mailbox and that generation; the consumption owner reports the sources
//!   it actually swapped out, and only a consumption that removed the linked bit discharges a round.
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
/// masked. It only covers the firmware's M→S reflection latency (the M-mode software-interrupt trap
/// that sets `sip.SSIP` fires regardless of the holder's cleared `sstatus.SIE`), never interrupt
/// delivery — the loop polls a bit and exits the instant it is set, so the bound is reached only when
/// the reflection has not yet landed. Sized generously so the reflection reliably lands under TCG.
const SSIP_SPINS: u64 = 60_000_000;

const SLOTS: usize = 1024;

// ── event kinds ─────────────────────────────────────────────────────────────────────────────
// Field layouts (`hart` is always the recording CPU). `acq` is the witness's acquisition identity:
// a fresh id per recorded acquisition, carried by the guard from the acquire record to its release
// record so a release names the exact acquisition it ends.
/// `[lock_id, round, acq, 0, 0]` — recorded AFTER the successful CAS.
pub const K_ACQUIRE: u8 = 1;
/// `[lock_id, round, k, 0, 0]` — the acquiring CPU observed the lock HELD; `k` is the contention
/// counter value this observation produced.
pub const K_CONTENDED: u8 = 2;
/// `[lock_id, round_of_acq, acq, 0, 0]` — release INTENT, recorded BEFORE the unlocking store.
pub const K_RELEASE: u8 = 3;
/// `[lock_id, round, acq, k_seen, sie | ssip<<1 | baseline<<8]` — the holder, inside acquisition
/// `acq` with interrupts masked, saw the contention counter reach `k_seen > baseline` (0 if the
/// bounded wait elapsed).
pub const K_HOLD: u8 = 4;
/// `[round, target, ok, generation, 0]` — the waiter's production reschedule publication to the holder;
/// `generation` is that publication's per-(target, sender) generation.
pub const K_IPI: u8 = 5;
/// `[link_round, sources, link_gen, snapshot_gen, discharged | sender<<8]` — a production mailbox
/// consumption (`ipi::take_arrival`) on a CPU with an armed link; `sources` are the bits actually
/// swapped out, `snapshot_gen` the sender's generation read after the swap.
pub const K_ARRIVAL: u8 = 6;
/// `[round, sender, outstanding, seen_gen, acq]` — the masked holder's positive observation of its
/// own mailbox, inside acquisition `acq`: whether the waiter's bit was still unconsumed, and the
/// waiter's publication generation at that moment.
pub const K_PENDING: u8 = 7;
/// `[round, holder, role, ok, 0]` — a hart passed the round gate (role 0 = holder, 1 = waiter;
/// `ok` = the waiter saw the holder inside its ownership before going on).
pub const K_GATE: u8 = 8;
/// `[round, 0, 0, 0, 0]` — a hart completed its mapping operation for the round (`*_MUT_OK`).
pub const K_DONE: u8 = 9;
/// `[old_round, new_round, old_gen, 0, 0]` — arming found an UNRESOLVED link on this CPU; it is
/// recorded (never silently overwritten) and the grader fails the earlier round's chain.
pub const K_LINKLOST: u8 = 10;

/// Small fixed bound on logical CPUs the witness tracks (two harts in this workload).
const MAX_CPU: usize = 8;

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
/// The mutual round currently under the gate, and whether its window is open. An ACQUISITION is
/// recorded iff the window is open when it succeeds; its release is then recorded unconditionally
/// (the guard carries the token), so every recorded acquisition has exactly one recorded release and
/// no release is recorded for an unrecorded acquisition. Contention observations are recorded while
/// the window is open.
static ROUND: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The designated holder (and waiter) of the current round; parity alternates them.
static HOLDER_CPU: AtomicU8 = AtomicU8::new(0xff);
static WAITER_CPU: AtomicU8 = AtomicU8::new(0xff);
/// Set by the holder's hold hook once it is inside the critical section (after its CAS); the waiter
/// gate waits for it, so the waiter's own lock() call begins while the holder owns the lock.
static HELD_ROUND: AtomicU64 = AtomicU64::new(0);
/// Bumped by an acquirer when it observes the lock HELD; the hold hook watches it advance.
static CONTENTION_SEQ: AtomicU64 = AtomicU64::new(0);
/// Per-round MUT_OK completions seen; the window closes when both harts have finished the round.
static ROUND_OK: AtomicU8 = AtomicU8::new(0);
/// The last round whose holder extended its critical section (only the first acquisition per round).
static LAST_HELD_ROUND: AtomicU64 = AtomicU64::new(0);
/// Acquisition identities (0 = "not recorded").
static ACQ_NEXT: AtomicU64 = AtomicU64::new(0);

// ── §3 publication generations and arrival links ────────────────────────────────────────────
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U64: AtomicU64 = AtomicU64::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_ROW: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];
/// `PUB_GEN[target][sender]`: how many times `sender` has published its bit into `target`'s
/// mailbox. Bumped by the production publication owner BEFORE its `fetch_or`, so any consumer whose
/// swap observed that bit reads a generation at least as new. Only `sender` ever writes its column
/// for a given target (sends from one CPU are sequential), so it is a per-sender counter.
static PUB_GEN: [[AtomicU64; MAX_CPU]; MAX_CPU] = [ZERO_ROW; MAX_CPU];
/// `ARRIVAL_LINK[cpu]`: the unresolved obligation of a masked holder whose waiter bit was positively
/// outstanding — packed `round | sender<<16 | generation<<24` (0 = none). Armed by this CPU's hold hook
/// while masked; discharged only by this CPU's first mailbox consumption that actually swaps out the
/// sender's bit.
static ARRIVAL_LINK: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];

const fn encode_link(round: u64, sender: u8, generation: u64) -> u64 {
    (round & 0xffff) | ((sender as u64) << 16) | (generation << 24)
}

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

/// This CPU's unconsumed mailbox sources (the production observation accessor; non-consuming).
#[cfg(all(target_arch = "riscv64", not(feature = "hosted-dev")))]
fn mailbox(cpu: u8) -> u64 {
    crate::arch::riscv64::ipi::pending(crate::kernel::scheduler::CpuId(cpu))
}
#[cfg(not(all(target_arch = "riscv64", not(feature = "hosted-dev"))))]
fn mailbox(_cpu: u8) -> u64 {
    0
}

fn pub_gen(target: u8, sender: u8) -> u64 {
    PUB_GEN
        .get(target as usize)
        .and_then(|row| row.get(sender as usize))
        .map_or(0, |g| g.load(Ordering::Acquire))
}

// ── the lock-path hooks (called from `SpinLockIrq` for the witnessed instance only) ──────────

/// The acquisition observed the lock already HELD — the actual failed acquisition. Recorded once
/// per `lock()` call, and only inside an open round window.
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

/// The CAS succeeded — this hart now holds the lock. Returns the acquisition token the guard keeps
/// (`round << 32 | acq`, or 0 when the window is closed and nothing is recorded).
pub fn note_acquired(id: u32) -> u64 {
    if id != VM_LOCK_ID || !ACTIVE.load(Ordering::Acquire) {
        return 0;
    }
    let round = ROUND.load(Ordering::Acquire);
    let acq = ACQ_NEXT.fetch_add(1, Ordering::AcqRel) + 1;
    record(K_ACQUIRE, this_cpu(), [u64::from(id), round, acq, 0, 0]);
    (round << 32) | (acq & 0xffff_ffff)
}

/// The guard is about to release the lock (release INTENT; the unlocking store follows). Recorded
/// exactly for the acquisitions `note_acquired` recorded, naming that acquisition.
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

/// The default-off, bounded hold hook. Runs holding the lock, with `SIE` masked, only for the
/// round's designated holder and only for its first recorded acquisition of the round. It waits —
/// bounded — until the contender has actually observed the lock HELD (the contention counter
/// advances), then records the holder's masked state, and positively observes its own mailbox:
/// whether the waiter's publication is still outstanding and its generation. It never waits for
/// held-lock work, interrupt delivery, a syscall completion or a remote ACK, and it releases when
/// the bound elapses even if the contender never arrives.
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
    if round != ROUND.load(Ordering::Acquire) {
        return;
    }
    // Extend only the first held acquisition of the round. Only the designated holder hart reaches
    // here for a given round, and within that hart the acquisitions are sequential, so a plain
    // load/store is a sound single-writer guard.
    if LAST_HELD_ROUND.load(Ordering::Acquire) >= round {
        return;
    }
    LAST_HELD_ROUND.store(round, Ordering::Release);
    let baseline = CONTENTION_SEQ.load(Ordering::Acquire);
    // Release the waiter's gate: the holder is now inside the critical section (after its CAS).
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
    // Corroboration only: the direct `sip.SSIP` peek, bounded; it never takes the interrupt.
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
        me,
        [
            u64::from(id),
            round,
            acq,
            k_seen,
            sie | (ssip << 1) | ((baseline & 0xffff_ffff) << 8),
        ],
    );
    // §3 positive evidence, still masked and still owning `acq`: is the waiter's bit outstanding in
    // this CPU's mailbox, and at which of the waiter's publication generations? The waiter published
    // before it contended, and `k_seen` was read after that contention, so a consumed-before-hold or
    // not-yet-published request shows here as `outstanding = 0`.
    let sender = WAITER_CPU.load(Ordering::Acquire);
    let outstanding = (mailbox(me) >> (u64::from(sender) & 63)) & 1;
    let seen_gen = pub_gen(me, sender);
    record(
        K_PENDING,
        me,
        [round, u64::from(sender), outstanding, seen_gen, acq],
    );
    if outstanding == 0 {
        return;
    }
    // Arm this round's obligation. Never overwrite an unresolved one silently.
    if let Some(cell) = ARRIVAL_LINK.get(me as usize) {
        let old = cell.swap(encode_link(round, sender, seen_gen), Ordering::AcqRel);
        if old != 0 {
            record(K_LINKLOST, me, [old & 0xffff, round, old >> 24, 0, 0]);
        }
    }
}

/// The production publication owner is about to set `sender`'s bit in `target`'s mailbox (called
/// immediately before the `fetch_or`, after every refusal). Advances that pair's generation.
pub fn note_publication(sender: u8, target: u8) {
    if let Some(g) = PUB_GEN
        .get(target as usize)
        .and_then(|row| row.get(sender as usize))
    {
        g.fetch_add(1, Ordering::AcqRel);
    }
}

/// The production mailbox consumption (`ipi::take_arrival`) on `cpu` swapped out `sources`. If
/// this CPU has an armed link, record the consumption; it DISCHARGES the link only if it actually
/// swapped out the linked sender's bit — an empty arrival or an unrelated source records but leaves
/// the obligation armed. The generation snapshot is read after the swap.
pub fn note_arrival(cpu: u8, sources: u64) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let Some(cell) = ARRIVAL_LINK.get(cpu as usize) else {
        return;
    };
    let link = cell.load(Ordering::Acquire);
    if link == 0 {
        return;
    }
    let round = link & 0xffff;
    let sender = ((link >> 16) & 0xff) as u8;
    let generation = link >> 24;
    let snapshot = pub_gen(cpu, sender);
    let discharged = (sources >> (u64::from(sender) & 63)) & 1;
    if discharged == 1 {
        // Only this CPU arms or discharges its own link, so a plain store suffices.
        cell.store(0, Ordering::Release);
    }
    record(
        K_ARRIVAL,
        cpu,
        [
            round,
            sources,
            generation,
            snapshot,
            discharged | (u64::from(sender) << 8),
        ],
    );
}

// ── the per-round direction gate (called from the SMP3 marker hook on `*_MUT_NR3`) ───────────

/// Open round `round`'s window and order entry so the designated holder acquires first. The holder
/// records its gate pass and returns at once. The waiter waits, bounded, until the holder is inside
/// its masked ownership (`HELD_ROUND`, stored after the holder's CAS), records whether that held,
/// and only THEN publishes the production reschedule IPI to the holder — so the work it publishes is
/// born while the holder owns the lock with interrupts masked — before issuing its own mapping
/// syscall and contending on the unchanged production acquisition path.
///
/// `cpu` is the hart running its `*_MUT_NR3` step; `round` is the mutual round. No domain lock is
/// held here (this runs on the off-lock DebugLog path), and the only wait is for the atomic
/// `HELD_ROUND`, which the holder sets without holding anything the waiter needs.
pub fn mut_round_gate(cpu: u8, round: u64) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let holder = if round % 2 == 1 { 0u8 } else { 1u8 };
    let waiter = holder ^ 1;
    // Open the window for this round. Idempotent across the two harts' gate calls.
    HOLDER_CPU.store(holder, Ordering::Release);
    WAITER_CPU.store(waiter, Ordering::Release);
    ROUND.store(round, Ordering::Release);
    ROUND_OK.store(0, Ordering::Release);
    ACTIVE.store(true, Ordering::Release);
    if cpu == holder {
        record(K_GATE, cpu, [round, u64::from(holder), 0, 1, 0]);
        return;
    }
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
    publish_ipi_to_holder(cpu, holder, round);
}

/// Publish a production reschedule IPI from the waiter hart to the holder hart, through the exact
/// send/mailbox owner a remote wake uses. Records the outcome and the publication's generation
/// (single writer: only this CPU publishes this sender's bit, and it is masked here). Never waits
/// for delivery or an ACK.
#[cfg(all(target_arch = "riscv64", not(feature = "hosted-dev")))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    use crate::kernel::scheduler::CpuId;
    let ok =
        crate::arch::riscv64::ipi::send_reschedule(CpuId(waiter_cpu), CpuId(holder_cpu)).is_ok();
    let generation = pub_gen(holder_cpu, waiter_cpu);
    record(
        K_IPI,
        waiter_cpu,
        [round, u64::from(holder_cpu), u64::from(ok), generation, 0],
    );
}
#[cfg(not(all(target_arch = "riscv64", not(feature = "hosted-dev"))))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    record(K_IPI, waiter_cpu, [round, u64::from(holder_cpu), 0, 0, 0]);
}

/// A hart finished its round (`*_MUT_OK`): record it, and close the window once both have.
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
                K_ARRIVAL => "arrival",
                K_PENDING => "pending",
                K_GATE => "gate",
                K_DONE => "done",
                K_LINKLOST => "linklost",
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
