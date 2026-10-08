// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK3 — real contention on the production VM address-space lock on x86_64, and the
//! reschedule IPI deferred through the holder's masked hold.
//!
//! The lock is the instance LOCK1 and LOCK2 witness: `KernelState::vm_state_lock`, a
//! `SpinLockIrq<()>` of rank 5 that both CPUs reach through the shared mapping transaction
//! (`run_vm_map_transaction`: the guard-page query, then the install). On x86_64 the acquisition's
//! `irq_save` is `pushfq; cli` and the restore executes `sti` only if `IF` was set before; every
//! kernel entry runs with `IF = 0` (interrupt gates, and `FMASK` clears it on `syscall`), so the
//! holder's critical section is masked and a fixed IPI made pending to it is taken only once the
//! holder has released the lock and returned to ring 3 (`sysretq`/`iretq` restore the user `IF`), or
//! reached an idle `sti; hlt`.
//!
//! The interrupt evidence is the local APIC's and the one ICR writer's, not a copy of RISC-V's mailbox
//! generations or the GIC's source token:
//!
//! * **publication** — the waiter, after it saw the holder inside its masked ownership, calls the
//!   production reschedule owner for that direction (`smp::send_reschedule_ipi_to` towards the AP,
//!   `smp::c2c_send_reschedule_ipi_to` towards the BSP — the NR6 and NR7 remote-delivery owners). Every
//!   ICR write on either CPU is recorded by the one writer (`smp::write_icr`), so the grader sees the
//!   exact destination and vector of every IPI sent, by whom and when;
//! * **outstanding under the mask** — the holder reads its own LAPIC's `IRR` and `ISR` words for the
//!   reschedule vector (0xF1; reads have no side effect) once when its hold begins and again after it
//!   saw the waiter's contention, with its own TLB-request generation each time. The `IRR` bit clear at
//!   the start and set later, not in service, with no TLB request published to it in between, is the
//!   local APIC holding a fixed interrupt on that vector accepted during this masked hold. The `IRR` has
//!   one bit per vector and no source: a second IPI on the same vector before delivery coalesces into
//!   it, and the attribution to the waiter is the ICR record (the only 0xF1 write to the holder inside
//!   the window), not the controller;
//! * **hardware entry and completion** — the 0xF1 handler (`yarm_ap_remote_wake_stub`, pure assembly,
//!   the same gate on both CPUs) records, in this module's ring, every arrival it takes once armed:
//!   the privilege level and RIP it interrupted, whether a TLB request was outstanding (so TLB work is
//!   never credited to the reschedule), its `ISR`/`IRR` words, and the production arrival ordinal — and,
//!   after its one `EOI` write, the `ISR` word again. The handler takes no lock and calls no Rust.
//!
//! The witness never writes an EOI, never sends except through the production owners, never runs the
//! handler and never synthesizes an arrival. The lock path is bounded, lock-free and console-free; the
//! only console output is the synchronous dump after both witness tasks finished.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// The witness id stamped on `vm_state_lock` (and on nothing else).
pub const VM_LOCK_ID: u32 = 1;

/// Mutual rounds the LOCK3 build runs (`YARM_SMP1_MUTUAL_ROUNDS`). Six per direction.
pub const LOCK3_ROUNDS: u64 = 12;

/// The reschedule vector — on this port the same vector the TLB shootdown uses (0xF1).
pub const RESCHED_VECTOR: u32 = 0xF1;

/// Bounds on the hold hook's extension, on the waiter gate, and on the `IRR` observation (which only
/// covers the ICR write landing at the holder's local APIC — never delivery: the holder stays masked
/// throughout and the loop exits the instant the bit is seen).
const HOLD_SPINS: u64 = 20_000_000;
const GATE_SPINS: u64 = 40_000_000;
const PEND_SPINS: u64 = 10_000_000;
/// Bound on the dump's wait for the other CPU to finish an arrival it has entered.
const QUIESCE_SPINS: u64 = 50_000_000;

pub const SLOTS: usize = 1024;
pub const MAX_CPU: usize = 8;

// ── event kinds (`hart` is always the recording CPU) ─────────────────────────────────────────
/// `[lock_id, round, acq, 0, 0]` — recorded AFTER the successful CAS.
pub const K_ACQUIRE: u8 = 1;
/// `[lock_id, round, k, 0, 0]` — the acquiring CPU observed the lock HELD; `k` is the contention
/// counter value this observation produced.
pub const K_CONTENDED: u8 = 2;
/// `[lock_id, round_of_acq, acq, 0, 0]` — release INTENT, recorded BEFORE the unlocking store.
pub const K_RELEASE: u8 = 3;
/// `[lock_id, round, acq, k_seen, if_set | baseline<<8]` — the holder, inside acquisition `acq`, saw
/// the contention counter reach `k_seen > baseline` (0 if the bounded wait elapsed); `if_set` is
/// `RFLAGS.IF` read there (must be 0).
pub const K_HOLD: u8 = 4;
/// `[round, target, vector, accepted, 0]` — the waiter called the production reschedule owner for
/// the holder; `accepted` = the ICR delivery-status bit read idle afterwards.
pub const K_IPI: u8 = 5;
/// Written by the 0xF1 handler: `[origin | tlb_outstanding<<8, interrupted_rip, req_gen |
/// ack_gen<<32, isr_word | irr_word<<32, arrival_ordinal]` (`origin` 1 = ring 0, 2 = ring 3).
pub const K_ENTRY: u8 = 6;
/// `[round, sender, isr_word | irr_word<<32, tlb_req_gen | outstanding<<32, acq]` — the masked
/// holder's second view of its LAPIC, after it saw the contention.
pub const K_PENDING: u8 = 7;
/// `[round, holder, role, ok, arrived]` — a CPU passed the round gate (role 0 = holder, 1 = waiter;
/// `ok` = the waiter saw the holder inside its ownership; `arrived` = the holder saw the waiter at its
/// gate before taking the lock, or the waiter found its previous ICR write accepted).
pub const K_GATE: u8 = 8;
/// `[round, 0, 0, 0, 0]` — a CPU completed its mapping operation for the round with its result and
/// its context checked (`SMP1_LOCK3_DONE`).
pub const K_DONE: u8 = 9;
/// Written by the 0xF1 handler right after its `EOI` write: `[isr_word | irr_word<<32,
/// arrival_ordinal, 0, 0, 0]`.
pub const K_EOI: u8 = 10;
/// `[round, acq, isr_word | irr_word<<32, tlb_req_gen, sender]` — the masked holder's first view of
/// its LAPIC, when its hold began (before it opened the waiter's gate).
pub const K_BEGIN: u8 = 11;
/// `[dest_apic, icr_low, dest_tlb_req_gen, 0, 0]` — an ICR write by the one writer (`write_icr`).
pub const K_ICR: u8 = 12;

// ── the bounded event ring (also written by the 0xF1 handler — layout is ABI) ─────────────────
/// One record. `#[repr(C)]` because the 0xF1 handler stores into it from assembly: `state` at 0,
/// `kind` at 1, `hart` at 2, `f` at 8, 48 bytes in all (pinned below).
#[repr(C)]
pub struct Slot {
    state: AtomicU8,
    kind: AtomicU8,
    hart: AtomicU8,
    _pad: [u8; 5],
    f: [AtomicU64; 5],
}
const _: () = assert!(core::mem::size_of::<Slot>() == 48);
const _: () = assert!(core::mem::offset_of!(Slot, f) == 8);
const _: () = assert!(core::mem::offset_of!(Slot, kind) == 1);
const _: () = assert!(core::mem::offset_of!(Slot, hart) == 2);

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: Slot = Slot {
    state: AtomicU8::new(0),
    kind: AtomicU8::new(0),
    hart: AtomicU8::new(0),
    _pad: [0; 5],
    f: [const { AtomicU64::new(0) }; 5],
};
#[unsafe(no_mangle)]
pub static YARM_LOCK3_SLOT: [Slot; SLOTS] = [EMPTY; SLOTS];
#[unsafe(no_mangle)]
pub static YARM_LOCK3_NEXT: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static YARM_LOCK3_OVERFLOW: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static YARM_LOCK3_ARMED: AtomicBool = AtomicBool::new(false);
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U64: AtomicU64 = AtomicU64::new(0);
/// Per-CPU arrivals the handler entered and completed since arming (indexed by `gs:[0]`, the
/// per-CPU record's `cpu_id`), and whether one is in progress (set at a counted entry, cleared at
/// its EOI — an EOI is counted only for an entry that was).
#[unsafe(no_mangle)]
pub static YARM_LOCK3_ENTRIES: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];
#[unsafe(no_mangle)]
pub static YARM_LOCK3_EOIS: [AtomicU64; MAX_CPU] = [ZERO_U64; MAX_CPU];
#[unsafe(no_mangle)]
pub static YARM_LOCK3_INSTUB: [AtomicU8; MAX_CPU] = [const { AtomicU8::new(0) }; MAX_CPU];

// ── per-round contention state (identical in meaning to LOCK1's and LOCK2's) ──────────────────
static ROUND: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOLDER_CPU: AtomicU8 = AtomicU8::new(0xff);
static WAITER_CPU: AtomicU8 = AtomicU8::new(0xff);
static HELD_ROUND: AtomicU64 = AtomicU64::new(0);
/// Set by the waiter on entering its gate — after its marker syscall copied the step name from user
/// memory (which itself takes `vm_state_lock`). The holder's gate waits for it.
static WAITER_ARRIVED: AtomicU64 = AtomicU64::new(0);
/// Set by the holder as it leaves its own gate. The hold waits for it, so it can only begin at the
/// holder's first acquisition AFTER its marker syscall — its round's mapping syscall — and never
/// inside the copy of its own marker when the waiter's gate opened the window first.
static HOLDER_GATED: AtomicU64 = AtomicU64::new(0);
static CONTENTION_SEQ: AtomicU64 = AtomicU64::new(0);
static ROUND_OK: AtomicU8 = AtomicU8::new(0);
static LAST_HELD_ROUND: AtomicU64 = AtomicU64::new(0);
static ACQ_NEXT: AtomicU64 = AtomicU64::new(0);

/// The continuation VAs the programs return to after the round's two syscalls (META only).
static CONT: [AtomicU64; 4] = [ZERO_U64; 4];

pub fn arm() {
    YARM_LOCK3_ARMED.store(true, Ordering::Release);
}

/// `[server gate, server NR3, client gate, client NR3]` return addresses, for the dump's metadata.
pub fn record_continuations(c: [u64; 4]) {
    for (d, v) in CONT.iter().zip(c) {
        d.store(v, Ordering::Release);
    }
}

fn record(kind: u8, hart: u8, f: [u64; 5]) {
    if !YARM_LOCK3_ARMED.load(Ordering::Acquire) {
        return;
    }
    let i = YARM_LOCK3_NEXT.fetch_add(1, Ordering::AcqRel) as usize;
    let Some(slot) = YARM_LOCK3_SLOT.get(i) else {
        YARM_LOCK3_OVERFLOW.fetch_add(1, Ordering::AcqRel);
        return;
    };
    slot.kind.store(kind, Ordering::Relaxed);
    slot.hart.store(hart, Ordering::Relaxed);
    for (d, v) in slot.f.iter().zip(f) {
        d.store(v, Ordering::Relaxed);
    }
    slot.state.store(2, Ordering::Release);
}

#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn this_cpu() -> u8 {
    crate::arch::x86_64::descriptor_tables::current_cpu_id().0
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn this_cpu() -> u8 {
    0
}

/// `RFLAGS.IF` on this CPU (maskable interrupts enabled).
#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn if_set() -> u64 {
    let rflags: u64;
    // SAFETY: reads RFLAGS through the stack; changes nothing.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    (rflags >> 9) & 1
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn if_set() -> u64 {
    0
}

/// This CPU's local-APIC `(ISR, IRR)` words holding the reschedule vector's bit.
#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn view() -> (u64, u64) {
    let (isr, irr) = crate::arch::x86_64::smp::lapic_vector_words(RESCHED_VECTOR);
    (u64::from(isr), u64::from(irr))
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn view() -> (u64, u64) {
    (0, 0)
}

const VBIT: u64 = 1 << (RESCHED_VECTOR % 32);

#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn tlb_req_gen(cpu: u8) -> u64 {
    u64::from(crate::arch::x86_64::percpu::tlb_req_gen(
        crate::kernel::scheduler::CpuId(cpu),
    ))
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn tlb_req_gen(_cpu: u8) -> u64 {
    0
}

#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn wake_count(cpu: u8) -> u64 {
    u64::from(
        crate::arch::x86_64::percpu::remote_wake_arrivals(crate::kernel::scheduler::CpuId(cpu)).0,
    )
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn wake_count(_cpu: u8) -> u64 {
    0
}

#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn apic_id(cpu: u8) -> u64 {
    u64::from(
        crate::arch::x86_64::percpu::read_record(crate::kernel::scheduler::CpuId(cpu)).apic_id,
    )
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn apic_id(cpu: u8) -> u64 {
    u64::from(cpu)
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
/// acquisition of the round after it left its own gate, holding the lock with `IF` clear. Reads its LAPIC view and TLB-request
/// generation, opens the waiter's gate, waits (bounded) for the waiter's contention, polls (bounded)
/// for the reschedule vector's `IRR` bit, and records the masked state and both views. It never waits
/// for delivery, an EOI, a syscall or a remote ACK.
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
    if round != ROUND.load(Ordering::Acquire)
        || LAST_HELD_ROUND.load(Ordering::Acquire) >= round
        || HOLDER_GATED.load(Ordering::Acquire) != round
    {
        return;
    }
    LAST_HELD_ROUND.store(round, Ordering::Release);
    let sender = WAITER_CPU.load(Ordering::Acquire);
    let (pre_isr, pre_irr) = view();
    let pre_tlb = tlb_req_gen(me);
    record(
        K_BEGIN,
        me,
        [
            round,
            acq,
            pre_isr | (pre_irr << 32),
            pre_tlb,
            u64::from(sender),
        ],
    );
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
    let (mut isr, mut irr) = view();
    let mut polls = PEND_SPINS;
    while irr & VBIT == 0 && polls != 0 {
        polls -= 1;
        core::hint::spin_loop();
        (isr, irr) = view();
    }
    record(
        K_HOLD,
        me,
        [
            u64::from(id),
            round,
            acq,
            k_seen,
            if_set() | ((baseline & 0xffff_ffff) << 8),
        ],
    );
    let post_tlb = tlb_req_gen(me);
    let outstanding = u64::from(
        pre_irr & VBIT == 0
            && pre_isr & VBIT == 0
            && irr & VBIT != 0
            && isr & VBIT == 0
            && pre_tlb == post_tlb,
    );
    record(
        K_PENDING,
        me,
        [
            round,
            u64::from(sender),
            isr | (irr << 32),
            (post_tlb & 0xffff_ffff) | (outstanding << 32),
            acq,
        ],
    );
}

// ── the ICR hook (called from the one writer, `smp::write_icr`, after the write) ─────────────

/// An ICR write: destination APIC id and the low word written. Records the destination CPU's TLB
/// request generation at that moment too (assuming CPU index = APIC id, which the dump's metadata
/// lets the grader check), so a TLB-shootdown send is told apart from a reschedule send.
pub fn note_icr(apic: u8, low: u32) {
    if !YARM_LOCK3_ARMED.load(Ordering::Acquire) {
        return;
    }
    let dest_tlb = if usize::from(apic) < MAX_CPU {
        tlb_req_gen(apic)
    } else {
        0
    };
    record(
        K_ICR,
        this_cpu(),
        [u64::from(apic), u64::from(low), dest_tlb, 0, 0],
    );
}

// ── the per-round direction gate (from the SMP1 marker hook on `SMP1_LOCK3_GATE`) ────────────

/// Open round `round`'s window. The designated holder waits, bounded, until the waiter has entered its
/// gate (`WAITER_ARRIVED`), records whether it did, and returns to take the lock. The waiter first
/// waits (bounded) for its own previous ICR write to be accepted — so nothing it sent earlier can
/// land in the holder's `IRR` during the hold — then marks its arrival, waits, bounded, until the
/// holder is inside its masked ownership (`HELD_ROUND`), records whether that held, and only then
/// publishes the production reschedule IPI to the holder — before it returns to ring 3 and issues its
/// own mapping syscall, contending on the unchanged acquisition path. No domain lock is held here (the
/// off-lock DebugLog path); each wait is for an atomic the other side sets without holding anything
/// this side needs.
pub fn mut_round_gate(cpu: u8, round: u64) {
    if !YARM_LOCK3_ARMED.load(Ordering::Acquire) {
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
        HOLDER_GATED.store(round, Ordering::Release);
        return;
    }
    let settled = u64::from(icr_accepted());
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
    record(K_GATE, cpu, [round, u64::from(holder), 1, ok, settled]);
    publish_ipi_to_holder(cpu, holder, round);
}

#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn icr_accepted() -> bool {
    crate::arch::x86_64::smp::lock3_icr_accepted()
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn icr_accepted() -> bool {
    true
}

/// Publish the production reschedule IPI from the waiter to the holder through the owner a remote
/// delivery in that direction uses — NR6's `send_reschedule_ipi_to` towards the AP, NR7's
/// `c2c_send_reschedule_ipi_to` towards the BSP. Records whether the ICR read idle afterwards. Never
/// waits for delivery.
#[cfg(all(target_arch = "x86_64", not(feature = "hosted-dev")))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    use crate::kernel::scheduler::CpuId;
    if holder_cpu == 0 {
        crate::arch::x86_64::smp::c2c_send_reschedule_ipi_to(CpuId(waiter_cpu), CpuId(holder_cpu));
    } else {
        crate::arch::x86_64::smp::send_reschedule_ipi_to(CpuId(waiter_cpu), CpuId(holder_cpu));
    }
    let accepted = u64::from(icr_accepted());
    record(
        K_IPI,
        waiter_cpu,
        [
            round,
            u64::from(holder_cpu),
            u64::from(RESCHED_VECTOR),
            accepted,
            0,
        ],
    );
}
#[cfg(not(all(target_arch = "x86_64", not(feature = "hosted-dev"))))]
fn publish_ipi_to_holder(waiter_cpu: u8, holder_cpu: u8, round: u64) {
    record(
        K_IPI,
        waiter_cpu,
        [
            round,
            u64::from(holder_cpu),
            u64::from(RESCHED_VECTOR),
            0,
            0,
        ],
    );
}

/// A CPU finished its round (`SMP1_LOCK3_DONE`): record it, and close the window once both have.
pub fn note_round_ok(round: u64) {
    if !YARM_LOCK3_ARMED.load(Ordering::Acquire) {
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

// ── the sealed dump (called after the SMP1 summary, once both tasks finished) ─────────────────

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// `(entries, eois, production arrivals)` for `cpu`. EOIs are read FIRST: both counters only grow and
/// an entry precedes its EOI, so `eois(t1) <= entries(t1) <= entries(t2)`; the pair reads equal only
/// when nothing was in progress at `t1` and nothing new entered by `t2`. The production count is read
/// last, after the entries it must cover.
fn counts(cpu: usize) -> (u64, u64, u64) {
    let eois = YARM_LOCK3_EOIS[cpu].load(Ordering::Acquire);
    let entries = YARM_LOCK3_ENTRIES[cpu].load(Ordering::Acquire);
    (entries, eois, wake_count(cpu as u8))
}

/// Print every event (twice, each line with its own checksum), the per-CPU handler counts and the
/// completion record. The other CPU's counts are the snapshot at which its entries and EOIs read
/// balanced — taken by a bounded wait, and printed exactly as observed (`settled=0` reports a bound
/// reached and prints the last reading). The dumping CPU is inside a syscall, not the handler, so its
/// own counts are already settled.
pub fn dump() {
    let me = this_cpu() as usize;
    let mut snap = [counts(0), counts(1)];
    let mut settled = [1u64; 2];
    for cpu in 0..2 {
        if cpu == me {
            continue;
        }
        let mut left = QUIESCE_SPINS;
        loop {
            let c = counts(cpu);
            snap[cpu] = c;
            if c.0 == c.1 {
                break;
            }
            if left == 0 {
                settled[cpu] = 0;
                break;
            }
            left -= 1;
            core::hint::spin_loop();
        }
    }
    let n = (YARM_LOCK3_NEXT.load(Ordering::Acquire) as usize).min(SLOTS);
    let overflow = YARM_LOCK3_OVERFLOW.load(Ordering::Acquire) != 0;
    let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::with_capacity(n + 4);
    let cont = |i: usize| CONT[i].load(Ordering::Acquire);
    lines.push(alloc::format!(
        "LOCK3_META vm_lock_id={} rounds={} slots_used={} overflow={} dump_cpu={} vector=0x{:x} apic0={} apic1={} s_gate=0x{:x} s_nr3=0x{:x} c_gate=0x{:x} c_nr3=0x{:x}",
        VM_LOCK_ID,
        LOCK3_ROUNDS,
        n,
        u8::from(overflow),
        me,
        RESCHED_VECTOR,
        apic_id(0),
        apic_id(1),
        cont(0),
        cont(1),
        cont(2),
        cont(3)
    ));
    for (i, slot) in YARM_LOCK3_SLOT.iter().enumerate().take(n) {
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
            K_IPI => "ipi",
            K_ENTRY => "entry",
            K_PENDING => "pending",
            K_GATE => "gate",
            K_DONE => "done",
            K_EOI => "eoi",
            K_BEGIN => "begin",
            K_ICR => "icr",
            _ => "unknown",
        };
        lines.push(alloc::format!(
            "LOCK3_REC seq={} kind={} hart={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}",
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
        let (entries, eois, wakes) = snap[cpu];
        lines.push(alloc::format!(
            "LOCK3_COUNTS cpu={} entries={} eois={} arrivals={} tlb_req_gen={} settled={}",
            cpu,
            entries,
            eois,
            wakes,
            tlb_req_gen(cpu as u8),
            s
        ));
    }
    lines.push(alloc::format!("LOCK3_DUMP_DONE records={}", n));
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
