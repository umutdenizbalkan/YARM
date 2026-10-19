// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3 — the RISC-V two-hart reschedule IPI and remote translation fence, through the SBI.
//!
//! Nothing here sends, enables or consumes anything until the boot hart released a secondary
//! ([`release_set`]: under `yarm.ap_user_dispatch=1` every trap-ready one, for the witnesses; by
//! default, AP-SCHED1, logical CPU 1 alone, for production dispatch) and the target published
//! itself ready. A one-hart boot releases nothing.
//!
//! # What the firmware does, and what reaches the supervisor
//!
//! The supervisor never touches the CLINT/ACLINT: on QEMU virt the IPI device is the M-mode
//! `aclint-mswi`, owned by OpenSBI v1.3.
//!
//! * **IPI** — [`send_reschedule`] publishes the work, then asks the firmware (`sbi_send_ipi`,
//!   EID `0x735049`) to interrupt the target hart. OpenSBI writes the target's MSWI, takes the
//!   machine software interrupt THERE, and raises `sip.SSIP` on it. What reaches the target
//!   supervisor is a supervisor software interrupt and nothing else: no source, no token, no
//!   claim. The target clears `sip.SSIP` itself ([`take_arrival`]); there is no PLIC claim and no
//!   end-of-interrupt, so none is performed.
//! * **Remote fence** — [`remote_invalidate_page`] asks the firmware (`sbi_remote_sfence_vma_asid`,
//!   EID `0x52464E43`) to run `SFENCE.VMA va, asid` on every target hart. OpenSBI v1.3 queues the
//!   request on each target, interrupts it, and SPINS (`tlb_sync`) until every target processed
//!   it — draining its own queue meanwhile, so two harts fencing each other cannot deadlock. The
//!   targets do the work in M-mode, whatever their supervisor state: masked, idle, or inside a
//!   lock. The firmware is therefore the completion owner; this port adds no software shootdown
//!   beside it and no acknowledgement object to go stale.
//!
//! # The mailbox
//!
//! One word per TARGET CPU; bit `s` = source CPU `s` published a reschedule request the target
//! has not consumed. The request itself is the enqueue the sender already committed on the
//! target's run queue: the IPI says "look", the run queue says what at.
//!
//! * **publication before notification** — `fetch_or` (AcqRel) and a `fence rw,rw` precede the
//!   SBI call, so the enqueue and the bit are visible before the target can be interrupted;
//! * **clear, then consume** — the target clears `sip.SSIP` BEFORE it swaps the word. A
//!   publication landing after the clear raises SSIP again and is taken by a later arrival; one
//!   landing before the swap is consumed by this one, and its own SSIP then yields an EMPTY
//!   arrival. Swapping first would let a publication that lands between swap and clear have its
//!   SSIP erased and never be consumed — a lost wake.
//!
//! Firmware requests, supervisor arrivals and consumed publications are counted apart: several
//! requests pending together merge into one `SSIP` (one arrival), and a bit published twice before
//! the target consumed it merges into one publication. Neither is loss, and neither is duplication.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::kernel::scheduler::{CpuId, MAX_CPUS};

/// `sip.SSIP` / `sie.SSIE` (bit 1).
pub const SSIP_BIT: usize = 1 << 1;
/// `scause` interrupt bit and the supervisor software interrupt code.
const INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
pub const IRQ_SUPERVISOR_SOFTWARE_CODE: usize = 1;

/// Where a supervisor software interrupt was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalOrigin {
    /// Taken from U-mode: the interrupted task continues; the woken work waits for this CPU's
    /// next scheduling point.
    User,
    /// Taken at this CPU's armed idle boundary (the stack-free `wfi` loop): an idle queue advance
    /// is owed.
    Idle,
}

/// One supervisor software interrupt, as its entry owner consumed it. Carried to the shared trap
/// wrapper; the consumption already happened and cannot happen twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpiArrival {
    pub origin: ArrivalOrigin,
    /// The source CPUs whose publications this arrival consumed; `0` = an empty arrival (its
    /// publication was consumed by an earlier one).
    pub sources: u64,
}

impl IpiArrival {
    pub fn at_idle_boundary(&self) -> bool {
        self.origin == ArrivalOrigin::Idle
    }
}

/// Why a reschedule IPI was not sent. Nothing is published or written on any refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiRefusal {
    /// A CPU never interrupts itself for a wake: its own enqueue is seen at its own next boundary.
    SelfTarget,
    /// The target never published itself ready (not released, not admitted, or the knob is off).
    NotReady,
    /// No hart id is known for the target — a guessed mask could interrupt another hart.
    NoHart,
    /// The firmware refused the request; the publication stays for the target's next arrival.
    Firmware(crate::arch::riscv64::sbi::SbiError),
}

/// `true` when this trap is a supervisor software interrupt taken from S-mode at this CPU's armed
/// idle boundary. Pure; the same shape as the timer's and the external interrupt's predicates.
pub fn is_accepted_s_mode_software_trap(
    scause: usize,
    sstatus: usize,
    boundary_armed: bool,
) -> bool {
    const SPP_BIT: usize = 1 << 8;
    (scause & INTERRUPT_BIT) != 0
        && (scause & !INTERRUPT_BIT) == IRQ_SUPERVISOR_SOFTWARE_CODE
        && (sstatus & SPP_BIT) != 0
        && boundary_armed
}

/// `true` when `scause` names the supervisor software interrupt (any origin).
pub fn is_software_interrupt(scause: usize) -> bool {
    (scause & INTERRUPT_BIT) != 0 && (scause & !INTERRUPT_BIT) == IRQ_SUPERVISOR_SOFTWARE_CODE
}

/// The SBI hart mask for a CPU bitmap, through `hart_of`. `None` when any CPU has no hart id or a
/// hart id the mask cannot name — the caller then fails closed rather than fence a guessed set.
/// Pure.
pub fn hart_mask_for(cpus: u64, hart_of: impl Fn(usize) -> Option<usize>) -> Option<usize> {
    let mut mask = 0usize;
    for cpu in 0..64usize {
        if cpus & (1u64 << cpu) == 0 {
            continue;
        }
        let hart = hart_of(cpu)?;
        if hart >= usize::BITS as usize {
            return None;
        }
        mask |= 1usize << hart;
    }
    Some(mask)
}

// ─────────────────────────────── state ───────────────────────────────

/// Per-target mailbox (bit = source CPU).
static PENDING: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// CPUs that can take and service the reschedule IPI. Published by each CPU for itself, last.
static READY: AtomicU64 = AtomicU64::new(0);
/// Set by the boot hart immediately before its first user entry, right after it publishes the
/// boot state: the end of the window in which it holds `&mut KernelState` through the
/// `BootExclusive` token (BLR1; formerly `borrow_kernel_for_boot`). A parked secondary is released
/// by this flag and the IPI that announces it; it touches no shared kernel state before.
static BOOT_BORROW_ENDED: AtomicBool = AtomicBool::new(false);
/// AP-SCHED1 — the secondaries the boot hart RELEASED, one bit per logical CPU, published before
/// the flag above. A parked hart leaves the park only on its own bit: `wfi` may return for any
/// reason, and a global flag would let an unreleased hart follow a released one out.
static RELEASED: AtomicU64 = AtomicU64::new(0);

/// Which trap-ready secondaries the boot hart releases: the policy's rule
/// ([`crate::arch::riscv_hart_admission::release_set`]). AP-SCHED2 §2: in production, every
/// trap-ready secondary that holds a dispatch-sized trap stack; with the diagnostic knob, every
/// trap-ready secondary. The boot CPU (bit 0) is never in the set.
pub fn release_set(trap_ready_cpus: u64, dispatch_capable_cpus: u64, diagnostic: bool) -> u64 {
    crate::arch::riscv_hart_admission::release_set(
        trap_ready_cpus,
        dispatch_capable_cpus,
        diagnostic,
    )
}

/// Per-CPU counters: `[publications, merged_publications, firmware_requests, firmware_refusals]`
/// as SENDER, and `[arrivals, from_user, at_idle, consumed_publications, empty_arrivals,
/// park_releases]` as TARGET.
static SENT: [[AtomicU32; 4]; MAX_CPUS] = [const { [const { AtomicU32::new(0) }; 4] }; MAX_CPUS];
static TAKEN: [[AtomicU32; 6]; MAX_CPUS] = [const { [const { AtomicU32::new(0) }; 6] }; MAX_CPUS];
/// Remote fences: `[requests, completed_ok, refused]` per requester.
static FENCES: [[AtomicU32; 3]; MAX_CPUS] = [const { [const { AtomicU32::new(0) }; 3] }; MAX_CPUS];

fn bump(c: &AtomicU32) {
    c.fetch_add(1, Ordering::AcqRel);
}

/// `true` when `cpu` published itself able to take the reschedule IPI.
pub fn ready(cpu: CpuId) -> bool {
    READY.load(Ordering::Acquire) & (1u64 << (cpu.0 as u64 & 63)) != 0
}

/// Publish `cpu` as a target. Called by that CPU, after its trap path, its `sie.SSIE` and (for a
/// secondary) its scheduler admission are all in place — so no CPU is ever named before it can
/// take the interrupt.
pub fn mark_ready(cpu: CpuId) {
    READY.fetch_or(1u64 << (cpu.0 as u64 & 63), Ordering::AcqRel);
}

/// AP-SCHED2 §2 — withdraw `cpu` as a target: its admission failed after it had published
/// readiness, so no sender may treat it as a CPU that will consume a publication.
pub fn withdraw_ready(cpu: CpuId) {
    READY.fetch_and(!(1u64 << (cpu.0 as u64 & 63)), Ordering::AcqRel);
}

pub fn boot_borrow_ended() -> bool {
    BOOT_BORROW_ENDED.load(Ordering::Acquire)
}

/// Sender counters `(publications, merged, firmware_requests, firmware_refusals)`.
pub fn sent_counters(cpu: CpuId) -> (u32, u32, u32, u32) {
    let Some(r) = SENT.get(cpu.0 as usize) else {
        return (0, 0, 0, 0);
    };
    let g = |i: usize| r[i].load(Ordering::Acquire);
    (g(0), g(1), g(2), g(3))
}

/// Target counters `(arrivals, from_user, at_idle, consumed_publications, empty_arrivals,
/// park_releases)`.
pub fn taken_counters(cpu: CpuId) -> (u32, u32, u32, u32, u32, u32) {
    let Some(r) = TAKEN.get(cpu.0 as usize) else {
        return (0, 0, 0, 0, 0, 0);
    };
    let g = |i: usize| r[i].load(Ordering::Acquire);
    (g(0), g(1), g(2), g(3), g(4), g(5))
}

/// Remote fence counters `(requests, completed_ok, refused)`.
pub fn fence_counters(cpu: CpuId) -> (u32, u32, u32) {
    let Some(r) = FENCES.get(cpu.0 as usize) else {
        return (0, 0, 0);
    };
    let g = |i: usize| r[i].load(Ordering::Acquire);
    (g(0), g(1), g(2))
}

/// Unconsumed publications for `cpu` (observation only).
pub fn pending(cpu: CpuId) -> u64 {
    PENDING
        .get(cpu.0 as usize)
        .map_or(0, |p| p.load(Ordering::Acquire))
}

// ─────────────────────────────── hardware ───────────────────────────────

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn full_fence() {
    // SAFETY: a memory fence.
    unsafe { core::arch::asm!("fence rw, rw", options(nostack, preserves_flags)) };
}

#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
fn full_fence() {
    core::sync::atomic::fence(Ordering::SeqCst);
}

/// Clear `sip.SSIP` on this hart. The supervisor owns this bit: OpenSBI raised it and will not
/// lower it.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn clear_ssip() {
    // SAFETY: `sip.SSIP` is supervisor-writable; clearing it has no other effect.
    unsafe {
        core::arch::asm!("csrc sip, {0}", in(reg) SSIP_BIT, options(nostack, preserves_flags))
    };
}

#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
fn clear_ssip() {}

/// Enable `sie.SSIE` on this hart, returning the read-back `sie`. With `sstatus.SIE` clear this
/// only makes a pending SSIP wake a `wfi` (and trap from U-mode); S-mode code stays
/// non-interruptible except inside the idle loop's own `wfi`.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn enable_ssie_on_this_hart() -> usize {
    let sie: usize;
    // SAFETY: sets one interrupt-enable bit and reads the register back.
    unsafe {
        core::arch::asm!(
            "csrs sie, {b}",
            "csrr {out}, sie",
            b = in(reg) SSIP_BIT,
            out = out(reg) sie,
            options(nostack, preserves_flags)
        )
    };
    sie
}

#[cfg(not(all(not(feature = "hosted-dev"), target_arch = "riscv64")))]
pub fn enable_ssie_on_this_hart() -> usize {
    SSIP_BIT
}

/// The hart id of logical CPU `cpu`, from the mapping the boot hart validated and claimed.
fn hart_of(cpu: usize) -> Option<usize> {
    crate::arch::riscv64::boot::hart_id_of_logical_cpu(cpu)
}

// ─────────────────────────────── the owners ───────────────────────────────

/// Send the reschedule IPI from `sender` to `target`: the remote half of an enqueue the caller
/// already COMMITTED on `target`'s run queue. Called with no domain lock held.
#[inline(never)]
pub fn send_reschedule(sender: CpuId, target: CpuId) -> Result<(), IpiRefusal> {
    if sender == target {
        return Err(IpiRefusal::SelfTarget);
    }
    if !ready(target) {
        return Err(IpiRefusal::NotReady);
    }
    let t = target.0 as usize;
    let hart = hart_of(t).ok_or(IpiRefusal::NoHart)?;
    let hart_mask = 1usize.checked_shl(hart as u32).ok_or(IpiRefusal::NoHart)?;
    let slot = PENDING.get(t).ok_or(IpiRefusal::NoHart)?;
    let bit = 1u64 << (sender.0 as u64 & 63);
    // QEMU-LOCK1-SEAL §3: advance this (target, sender) publication generation BEFORE the bit is
    // published, so any consumption that swaps the bit out reads a generation at least this new.
    #[cfg(feature = "riscv64-lock1-witness")]
    crate::kernel::lock1_witness::note_publication(sender.0, target.0);
    // PUBLICATION, then NOTIFICATION.
    let old = slot.fetch_or(bit, Ordering::AcqRel);
    full_fence();
    if let Some(row) = SENT.get(sender.0 as usize) {
        bump(&row[0]);
        if old & bit != 0 {
            bump(&row[1]);
        }
    }
    #[cfg(feature = "riscv64-smp3-witness")]
    crate::arch::riscv64::smp3_witness::note_ipi_published(sender, target, hart, old & bit != 0);
    let result = crate::arch::riscv64::sbi::send_ipi(hart_mask, 0);
    if let Some(row) = SENT.get(sender.0 as usize) {
        bump(if result.is_ok() { &row[2] } else { &row[3] });
    }
    #[cfg(feature = "riscv64-smp3-witness")]
    crate::arch::riscv64::smp3_witness::note_ipi_requested(
        sender,
        target,
        hart,
        crate::kernel::boot::smp3_record::PUB_WAKE,
        &result,
    );
    result.map_err(IpiRefusal::Firmware)
}

/// QEMU-SMP3 witness only — raise the reschedule IPI on THIS hart, through the same publication
/// and firmware call a remote wake uses. For the one case no remote hart can cover: tasks placed
/// on the secondary by the secondary itself, outside a trap, while it has no timer. The
/// consumption and dispatch that follow are the same production owners a remote wake reaches.
#[cfg(feature = "riscv64-smp3-witness")]
pub fn kick_self(cpu: CpuId) -> bool {
    if !ready(cpu) {
        return false;
    }
    let Some(hart) = hart_of(cpu.0 as usize) else {
        return false;
    };
    let Some(slot) = PENDING.get(cpu.0 as usize) else {
        return false;
    };
    let bit = 1u64 << (cpu.0 as u64 & 63);
    #[cfg(feature = "riscv64-lock1-witness")]
    crate::kernel::lock1_witness::note_publication(cpu.0, cpu.0);
    let old = slot.fetch_or(bit, Ordering::AcqRel);
    full_fence();
    crate::arch::riscv64::smp3_witness::note_kick_published(cpu, hart, old & bit != 0);
    let result = crate::arch::riscv64::sbi::send_ipi(1usize << hart, 0);
    if let Some(row) = SENT.get(cpu.0 as usize) {
        bump(&row[0]);
        bump(if result.is_ok() { &row[2] } else { &row[3] });
    }
    crate::arch::riscv64::smp3_witness::note_ipi_requested(
        cpu,
        cpu,
        hart,
        crate::kernel::boot::smp3_record::PUB_KICK,
        &result,
    );
    result.is_ok()
}

/// THE consumption of a supervisor software interrupt, by its entry owner, exactly once per trap:
/// clear `sip.SSIP`, THEN swap this CPU's mailbox. See the module docs for why the order is the
/// correctness argument.
#[inline(never)]
pub fn take_arrival(cpu: CpuId, origin: ArrivalOrigin) -> IpiArrival {
    clear_ssip();
    let sources = PENDING
        .get(cpu.0 as usize)
        .map_or(0, |p| p.swap(0, Ordering::AcqRel));
    if let Some(row) = TAKEN.get(cpu.0 as usize) {
        bump(&row[0]);
        bump(match origin {
            ArrivalOrigin::User => &row[1],
            ArrivalOrigin::Idle => &row[2],
        });
        if sources == 0 {
            bump(&row[4]);
        } else {
            row[3].fetch_add(sources.count_ones(), Ordering::AcqRel);
        }
    }
    // QEMU-LOCK1-SEAL §3: the delivery end of the chain. The witness receives the ACTUAL sources
    // this consumption swapped out; it records only on a CPU with an armed link, and discharges that
    // link only when the linked sender's bit is among `sources`. Observes; takes nothing.
    #[cfg(feature = "riscv64-lock1-witness")]
    crate::kernel::lock1_witness::note_arrival(cpu.0, sources);
    IpiArrival { origin, sources }
}

/// A parked secondary's release: the boot hart's IPI woke its `wfi` (no trap — `sstatus.SIE` is
/// clear in the park). Consumed the same way, clear first; the "work" is the release flag.
pub fn take_park_release(cpu: CpuId) -> bool {
    clear_ssip();
    let released = boot_borrow_ended()
        && RELEASED.load(Ordering::Acquire) & (1u64 << (cpu.0 as u64 & 63)) != 0;
    if released && let Some(row) = TAKEN.get(cpu.0 as usize) {
        bump(&row[5]);
    }
    released
}

/// The boot hart, at the end of its boot borrow (immediately before its first user entry): make
/// itself a target, publish the release, and interrupt every released secondary
/// ([`release_set`]). A strict no-op when that set is empty — a one-hart boot, or a boot whose
/// trap-ready secondaries all lack a dispatch stack — so one-CPU operation is unchanged. Returns
/// the harts interrupted.
pub fn release_secondaries_at_boot_borrow_end(trap_ready_cpus: u64) -> usize {
    let diagnostic = crate::kernel::boot::ap_user_dispatch_enabled();
    let capable = crate::arch::riscv64::boot::riscv_dispatch_capable_bitmap();
    let released = release_set(trap_ready_cpus, capable, diagnostic);
    // Every trap-ready secondary left out stays parked, wake-only and masked. Said once, by name,
    // so a quiescent CPU is a reported fact rather than an absence: AP-SCHED2 §2 leaves only a
    // CPU with no dispatch-sized trap stack (a genuine resource limit) out of production.
    for cpu in 1..MAX_CPUS.min(64) {
        if trap_ready_cpus & !released & (1u64 << cpu) != 0 {
            crate::arch::riscv64::boot::early_sbi_marker(format_args!(
                "RISCV_AP_QUIESCENT cpu={} reason={} wake_only=1 sie=ssie_park_only",
                cpu,
                crate::arch::riscv_hart_admission::Exclusion::NoDispatchStack.marker()
            ));
        }
    }
    // AP-SCHED2 witness: the set the run must cover (observation only).
    #[cfg(feature = "riscv64-apsched-witness")]
    crate::arch::riscv64::apsched_witness::note_release_plan(released, trap_ready_cpus);
    crate::arch::riscv64::boot::early_sbi_marker(format_args!(
        "RISCV_AP_RELEASE_PLAN trap_ready=0x{:x} dispatch_capable=0x{:x} released=0x{:x} knob={}",
        trap_ready_cpus,
        capable,
        released,
        u8::from(diagnostic)
    ));
    if released == 0 {
        return 0;
    }
    let boot = CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
    let sie = enable_ssie_on_this_hart();
    mark_ready(boot);
    RELEASED.store(released, Ordering::Release);
    BOOT_BORROW_ENDED.store(true, Ordering::Release);
    full_fence();
    let mut sent = 0usize;
    let mut refused = 0usize;
    for cpu in 1..MAX_CPUS.min(64) {
        if released & (1u64 << cpu) == 0 {
            continue;
        }
        let Some(hart) = hart_of(cpu) else {
            refused += 1;
            continue;
        };
        #[cfg(feature = "riscv64-smp3-witness")]
        crate::arch::riscv64::smp3_witness::note_release_published(boot, CpuId(cpu as u8), hart);
        let r = crate::arch::riscv64::sbi::send_ipi(1usize << hart, 0);
        if let Some(row) = SENT.get(boot.0 as usize) {
            bump(if r.is_ok() { &row[2] } else { &row[3] });
        }
        #[cfg(feature = "riscv64-smp3-witness")]
        crate::arch::riscv64::smp3_witness::note_ipi_requested(
            boot,
            CpuId(cpu as u8),
            hart,
            crate::kernel::boot::smp3_record::PUB_RELEASE,
            &r,
        );
        if r.is_ok() {
            sent += 1;
        } else {
            refused += 1;
        }
    }
    crate::arch::riscv64::boot::early_sbi_marker(format_args!(
        "RISCV_SMP3_RELEASE boot_cpu=0 sie=0x{:x} ssie={} released_harts={} refused={} knob={} released=0x{:x}",
        sie,
        (sie >> 1) & 1,
        sent,
        refused,
        u8::from(diagnostic),
        released
    ));
    sent
}

/// Why a remote fence did not complete. On any of them the caller must NOT reclaim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceRefusal {
    /// A target CPU has no hart id the mask can name.
    NoHart,
    /// The firmware returned an error: the fence is NOT known to have run anywhere.
    Firmware(crate::arch::riscv64::sbi::SbiError),
}

/// Retire `virt` in `asid` on every hart of `targets` (a CPU bitmap, the requester excluded) and
/// return only once the firmware reports every target done. Called with no lock held, after the
/// requester's own PTE write and local `sfence.vma`.
///
/// Order is the contract, and QEMU cannot show all of it (see `tests/qemu_smp3_scope.rs`):
/// 1. the PTE store is complete on the requester (the page-table owner wrote it and fenced
///    locally);
/// 2. `fence rw, rw` — the store is globally visible before any target is asked to fence, so a
///    target's `SFENCE.VMA` cannot be ordered before the write it exists to expose;
/// 3. `sbi_remote_sfence_vma_asid` — `Ok` on OpenSBI v1.3 means every target executed it.
#[inline(never)]
pub fn remote_invalidate_page(
    requester: CpuId,
    targets: u64,
    asid: u16,
    virt: u64,
) -> Result<(), FenceRefusal> {
    let row = FENCES.get(requester.0 as usize);
    let Some(hart_mask) = hart_mask_for(targets, hart_of) else {
        if let Some(r) = row {
            bump(&r[2]);
        }
        return Err(FenceRefusal::NoHart);
    };
    full_fence();
    if let Some(r) = row {
        bump(&r[0]);
    }
    #[cfg(feature = "riscv64-smp3-witness")]
    let generation = crate::arch::riscv64::smp3_witness::note_fence_request(
        requester, targets, hart_mask, asid, virt,
    );
    let result = crate::arch::riscv64::sbi::remote_sfence_vma_asid(
        hart_mask,
        0,
        virt as usize,
        crate::kernel::vm::PAGE_SIZE,
        usize::from(asid),
    );
    if let Some(r) = row {
        bump(if result.is_ok() { &r[1] } else { &r[2] });
    }
    #[cfg(feature = "riscv64-smp3-witness")]
    crate::arch::riscv64::smp3_witness::note_fence_done(
        requester, hart_mask, asid, virt, generation, &result,
    );
    result.map_err(FenceRefusal::Firmware)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INT: usize = 1usize << (usize::BITS - 1);
    const SPP: usize = 1 << 8;

    #[test]
    fn only_an_idle_boundary_software_interrupt_is_accepted_from_s_mode() {
        assert!(is_accepted_s_mode_software_trap(INT | 1, SPP, true));
        assert!(
            !is_accepted_s_mode_software_trap(1, SPP, true),
            "not an exception"
        );
        assert!(
            !is_accepted_s_mode_software_trap(INT | 5, SPP, true),
            "not the timer"
        );
        assert!(
            !is_accepted_s_mode_software_trap(INT | 9, SPP, true),
            "not external"
        );
        assert!(
            !is_accepted_s_mode_software_trap(INT | 1, 0, true),
            "from U-mode"
        );
        assert!(
            !is_accepted_s_mode_software_trap(INT | 1, SPP, false),
            "unarmed"
        );
        assert!(is_software_interrupt(INT | 1));
        assert!(!is_software_interrupt(INT | 5));
    }

    #[test]
    fn a_hart_mask_names_exactly_the_mapped_harts_or_nothing() {
        // Boot hart 1 is CPU 0, hart 0 is CPU 1: the mask is by HART, never by CPU index.
        let swapped = |cpu| match cpu {
            0 => Some(1),
            1 => Some(0),
            _ => None,
        };
        assert_eq!(hart_mask_for(0b10, swapped), Some(0b01));
        assert_eq!(hart_mask_for(0b01, swapped), Some(0b10));
        assert_eq!(hart_mask_for(0b11, swapped), Some(0b11));
        assert_eq!(hart_mask_for(0, swapped), Some(0));
        // A CPU with no hart fails the whole mask closed.
        assert_eq!(hart_mask_for(0b100, swapped), None);
        assert_eq!(hart_mask_for(0b1, |_| Some(64)), None);
    }

    /// AP-SCHED1 — the production release names logical CPU 1 alone; the diagnostic knob keeps
    /// every trap-ready secondary; the boot CPU is never released; an empty set releases nothing.
    #[test]
    fn production_releases_every_dispatch_capable_trap_ready_secondary() {
        let capable = 0xff;
        assert_eq!(release_set(0b11, capable, false), 0b10);
        assert_eq!(release_set(0b1111, capable, false), 0b1110);
        assert_eq!(release_set(0b1101, capable, false), 0b1100);
        assert_eq!(
            release_set(0b1, capable, false),
            0,
            "a one-hart boot: nothing"
        );
        assert_eq!(release_set(0b1111, 0b0011, false), 0b10, "no stack: parked");
        assert_eq!(release_set(0b1111, 0b0001, true), 0b1110, "the knob");
        assert_eq!(release_set(0b1, capable, true), 0);
    }

    /// AP-SCHED1 — a parked hart leaves only on its OWN released bit: `wfi` returns for any reason,
    /// and the flag alone would let an unreleased hart follow a released one out.
    #[test]
    fn a_park_release_is_per_cpu() {
        let saved = (
            RELEASED.load(Ordering::Acquire),
            BOOT_BORROW_ENDED.load(Ordering::Acquire),
        );
        RELEASED.store(0b10, Ordering::Release);
        BOOT_BORROW_ENDED.store(true, Ordering::Release);
        assert!(take_park_release(CpuId(1)));
        assert!(!take_park_release(CpuId(2)), "an unreleased hart re-parks");
        BOOT_BORROW_ENDED.store(false, Ordering::Release);
        assert!(
            !take_park_release(CpuId(1)),
            "nothing before the boot borrow ends"
        );
        RELEASED.store(saved.0, Ordering::Release);
        BOOT_BORROW_ENDED.store(saved.1, Ordering::Release);
    }

    #[test]
    fn a_publication_racing_the_clear_is_consumed_or_raises_a_later_arrival() {
        // Model the mailbox with the production operations: the swap after the clear sees every
        // publication made before it, and a publication after it stays for the next arrival.
        let m = AtomicU64::new(0);
        m.fetch_or(1, Ordering::AcqRel); // sender 0 publishes
        // target: clear SSIP (modelled as nothing to lose), then swap
        assert_eq!(m.swap(0, Ordering::AcqRel), 1);
        m.fetch_or(1, Ordering::AcqRel); // a publication after the swap
        m.fetch_or(1, Ordering::AcqRel); // merged: same source before consumption
        assert_eq!(
            m.swap(0, Ordering::AcqRel),
            1,
            "merged into one publication"
        );
        assert_eq!(
            m.swap(0, Ordering::AcqRel),
            0,
            "an empty arrival, not a loss"
        );
    }
}
