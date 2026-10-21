// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VM-INVALIDATION2 — the x86_64 dispatch-gap TLB witness.
//!
//! Built only with `x86-vmi2-witness` and armed only by `yarm.x86_64_vmi2_witness=1`. It reaches
//! the REAL empty-`current`/root-still-loaded window and races a production mapping operation
//! against it, then grades through the production owners rather than a model:
//!
//! * the gap is a genuine yield of the observer task on CPU 1 (`current` cleared, A's root still
//!   loaded — the state AP-SCHED4's resident-root suppression re-selects through);
//! * at the gap the witness records the production target set (`live_cpu_bitmap_for_asid_split`)
//!   for A, so the recorded set is exactly what a concurrent shootdown would decide from;
//! * a bounded, INTERRUPTS-ENABLED hold keeps the gap open while the mutator on CPU 0 performs a
//!   production `VmAnonMap` replace of OBS in A — its displacement drives the real shootdown, and
//!   CPU 1 services that shootdown's 0xF1 IPI from inside the hold, so no acknowledgement is ever
//!   awaited while its owner is prevented from running (no manufactured deadlock). The hold is
//!   released the moment the mutator reports the round done, or at a tick budget.
//!
//! Residency grading is honest: the POSITIVE evidence this regression requires is that the gap
//! window occurred and the production target set NAMED CPU 1 there (recorded from the owner). The
//! observer's post-resume bytes are recorded and checked SEPARATELY, and the grader is told that a
//! QEMU TLB model need not retain a stale entry — so a coherent read corroborates but does not by
//! itself prove the architectural invalidation. Zero gap occurrences cannot qualify.

#![cfg(feature = "x86-vmi2-witness")]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::kernel::scheduler::CpuId;
use crate::kernel::vm::{Asid, Mapping, PageFlags, PhysAddr, VirtAddr};
use crate::runtime::SharedKernel;

#[cfg(not(feature = "hosted-dev"))]
core::arch::global_asm!(include_str!("vmi2_witness.S"));

pub const CODE_VA: u64 = 0x2000_0000;
pub const STACK_TOP: u64 = 0x2001_0ff0;
pub const STACK_VA: u64 = 0x2001_0000;
pub const OBS_STK_VA: u64 = 0x2002_0000;
pub const OBS_TLS_VA: u64 = 0x2009_0000;
pub const MBX_VA: u64 = 0x2007_0000;
pub const OBS_VA: u64 = 0x2008_0000;
/// The boot task (spawner + mutator). The observer is the thread it spawns, so its TID is assigned
/// by the spawn and read from the mailbox, not fixed here.
pub const MUTATOR_TID: u64 = 9_701;
pub const ROUNDS: u64 = 8;
/// The seed OBS holds before any replace: nonzero and distinct from every mutator sentinel.
pub const OBS_SEED: u64 = 0x5644_4932_5345_4544;
/// Mutator sentinel base (matches `NEWVAL_BASE` in the program); round added to the low byte.
pub const NEWVAL_BASE: u64 = 0x5644_4932_4932_0000;

const M_T1_READY: u64 = 0x00;
const M_MUT_DONE: u64 = 0x08;
const M_T1_POST: u64 = 0x10;
const M_OBS_NEW: u64 = 0x18;
const M_T1_DONE: u64 = 0x20;
const M_T0_DONE: u64 = 0x28;
const M_OBS_TID: u64 = 0x30;

/// How long the gap-hold keeps the window open, in `pause`-spin iterations, before releasing on a
/// budget. Bounded so a stuck mutator can never wedge the boot; the smoke's timeout is the
/// backstop. Interrupts are enabled throughout the spin.
const HOLD_SPIN_BUDGET: u64 = 20_000_000;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ASID: AtomicU64 = AtomicU64::new(u64::MAX);
static MBX_PHYS: AtomicU64 = AtomicU64::new(0);

/// Per-round records. Index is `round - 1` (rounds are 1..=ROUNDS).
static GAP_SEEN: [AtomicBool; ROUNDS as usize] = [const { AtomicBool::new(false) }; ROUNDS as usize];
static GAP_NAMED: [AtomicBool; ROUNDS as usize] = [const { AtomicBool::new(false) }; ROUNDS as usize];
static GAP_TARGETS: [AtomicU64; ROUNDS as usize] = [const { AtomicU64::new(0) }; ROUNDS as usize];
static GAP_CR3: [AtomicU64; ROUNDS as usize] = [const { AtomicU64::new(0) }; ROUNDS as usize];
static POST_VAL: [AtomicU64; ROUNDS as usize] = [const { AtomicU64::new(0) }; ROUNDS as usize];
static NEW_VAL: [AtomicU64; ROUNDS as usize] = [const { AtomicU64::new(0) }; ROUNDS as usize];
static POST_SEEN: AtomicU64 = AtomicU64::new(0);
static DUMPED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn witness_asid() -> Option<Asid> {
    match ASID.load(Ordering::Acquire) {
        u64::MAX => None,
        raw => Some(Asid(raw as u16)),
    }
}

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    static yarm_vmi2_start: u8;
    static yarm_vmi2_end: u8;
}

fn mbx_read(off: u64) -> u64 {
    let phys = MBX_PHYS.load(Ordering::Acquire);
    if phys == 0 {
        return 0;
    }
    crate::kernel::boot::KernelState::phys_to_direct_map_ptr(phys + off)
        // SAFETY: an aligned u64 inside the witness's own mailbox frame; the direct map covers it.
        .map_or(0, |p| unsafe { core::ptr::read_volatile(p.cast::<u64>()) })
}

/// Provision A, the boot task (spawner + mutator), and its pages, from the bootstrap window. At
/// boot only CPU 0 is online, so the observer cannot be home-pinned to a secondary here; instead
/// the boot task spawns it as a THREAD at runtime (NR 11) and the production balanced placement
/// puts it on another CPU. Both share ASID A, so the mutator's replace directly displaces the page
/// the observer caches — no cross-space grant.
#[cfg(not(feature = "hosted-dev"))]
pub fn provision(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    let (asid, _root) = kernel.create_user_address_space()?;

    map_one(kernel, asid, OBS_VA)?;
    map_one(kernel, asid, STACK_VA)?;
    map_one(kernel, asid, OBS_STK_VA)?;
    map_one(kernel, asid, OBS_TLS_VA)?;
    let mbx = map_one(kernel, asid, MBX_VA)?;
    kernel.copy_to_user(asid, VirtAddr(MBX_VA), &[0u8; 0x1000])?;
    kernel.copy_to_user(asid, VirtAddr(OBS_VA), &OBS_SEED.to_le_bytes())?;

    // SAFETY (all `&raw const`): taking an extern static's address reads nothing.
    let start = &raw const yarm_vmi2_start;
    let len = &raw const yarm_vmi2_end as usize - start as usize;
    for i in 0..(len as u64).div_ceil(0x1000) {
        map_one(kernel, asid, CODE_VA + i * 0x1000)?;
    }
    // SAFETY: the program bytes live in this kernel image's text.
    let image = unsafe { core::slice::from_raw_parts(start, len) };
    kernel.copy_to_user(asid, VirtAddr(CODE_VA), image)?;

    kernel.register_task_with_class(MUTATOR_TID, crate::kernel::task::TaskClass::App)?;
    kernel.with_tcbs_mut(|tcbs| {
        let tcb = tcbs
            .iter_mut()
            .flatten()
            .find(|t| t.tid.0 == MUTATOR_TID)
            .ok_or(crate::kernel::boot::KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        tcb.user_context.instruction_ptr = VirtAddr(CODE_VA);
        tcb.user_context.stack_ptr = VirtAddr(STACK_TOP);
        Ok::<_, crate::kernel::boot::KernelError>(())
    })?;

    ASID.store(u64::from(asid.0), Ordering::Release);
    MBX_PHYS.store(mbx, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    kernel.enqueue_task(MUTATOR_TID)?;
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_WITNESS_PROVISIONED asid={} boot_tid={} rounds={} obs_va=0x{:x} mbx_phys=0x{:x} image={}",
        asid.0, MUTATOR_TID, ROUNDS, OBS_VA, mbx, len
    ));
    Ok(())
}

#[cfg(not(feature = "hosted-dev"))]
fn map_one(
    kernel: &mut crate::kernel::boot::KernelState,
    asid: Asid,
    va: u64,
) -> Result<u64, crate::kernel::boot::KernelError> {
    let phys = kernel.alloc_user_data_frame()?;
    kernel.map_user_page_in_asid_raw(
        asid,
        VirtAddr(va),
        Mapping {
            phys: PhysAddr(phys),
            flags: PageFlags {
                read: true,
                write: true,
                execute: true,
                user: true,
                cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
            },
        },
    )?;
    Ok(phys)
}

/// The gap-hold, called from the x86 yield drain while `current` is cleared and before the
/// re-selection re-sets it. Records the production target set for A at the gap, then holds the
/// window open (interrupts enabled) until the mutator reports the round's replace done. A no-op
/// unless armed, this CPU is in the observer's gap, and A's root is still recorded loaded here.
#[cfg(not(feature = "hosted-dev"))]
pub fn gap_hold(shared: &SharedKernel, cpu: CpuId, outgoing: u64) {
    if !enabled() {
        return;
    }
    // The observer is the spawned thread; its TID is published to the mailbox at spawn.
    let observer = mbx_read(M_OBS_TID);
    if observer == 0 || outgoing != observer {
        return;
    }
    let Some(asid) = witness_asid() else {
        return;
    };
    // The gap is real only if nothing is current here and A's root is still loaded on this CPU.
    if shared.current_tid_split_read(cpu).is_some() {
        return;
    }
    if crate::arch::hal::active_address_space(cpu) != Some(asid) {
        return;
    }
    let round = mbx_read(M_T1_READY);
    if round == 0 || round > ROUNDS {
        return;
    }
    let idx = (round - 1) as usize;
    // The production target set, computed exactly as a concurrent unmap on CPU 0 would.
    let targets = shared.live_cpu_bitmap_for_asid_split(asid);
    GAP_TARGETS[idx].store(targets, Ordering::Release);
    GAP_CR3[idx].store(crate::arch::x86_64::page_table::read_hw_cr3(), Ordering::Release);
    GAP_NAMED[idx].store(targets & (1u64 << cpu.0) != 0, Ordering::Release);
    GAP_SEEN[idx].store(true, Ordering::Release);
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_GAP round={} cpu={} asid={} current=empty root=loaded target_set=0x{:x} named={}",
        round,
        cpu.0,
        asid.0,
        targets,
        u8::from(targets & (1u64 << cpu.0) != 0)
    ));
    // Hold the window open with interrupts ENABLED so the mutator's shootdown IPI is serviced and
    // acknowledged here; release on the mutator's done signal or the bounded budget. Never wait
    // for an acknowledgement with interrupts masked — that would stall the very owner we need.
    let saved_if = read_rflags() & (1 << 9) != 0;
    // SAFETY: enabling interrupts here is the same discipline the idle loop uses; we restore the
    // caller's IF before returning, and the spin body makes no memory claims.
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
    let mut spins = 0u64;
    while mbx_read(M_MUT_DONE) < round && spins < HOLD_SPIN_BUDGET {
        spins += 1;
        core::hint::spin_loop();
    }
    if !saved_if {
        // SAFETY: restoring the caller's masked-interrupt state.
        unsafe {
            core::arch::asm!("cli", options(nomem, nostack));
        }
    }
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_GAP_RELEASED round={} cpu={} mut_done={} spins={}",
        round,
        cpu.0,
        mbx_read(M_MUT_DONE),
        spins
    ));
}

fn read_rflags() -> u64 {
    let mut f: u64;
    // SAFETY: pushfq/pop reads RFLAGS with no side effects.
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) f, options(nomem));
    }
    f
}

/// The DebugLog observation point (both tasks reach it). Records the observer's post-resume byte
/// per round and, on the last marker, dumps.
pub fn observe_user_marker(cpu: CpuId, msg: &str) {
    if !enabled() || !msg.starts_with("VMI2_") {
        return;
    }
    if msg.starts_with("VMI2_T1_POST") {
        let r = POST_SEEN.fetch_add(1, Ordering::AcqRel);
        if (r as usize) < ROUNDS as usize {
            POST_VAL[r as usize].store(mbx_read(M_T1_POST), Ordering::Release);
            NEW_VAL[r as usize].store(mbx_read(M_OBS_NEW), Ordering::Release);
        }
        crate::kernel::printk::printk_emit_sync(format_args!(
            "VMI2_USER cpu={} {} post=0x{:x} new=0x{:x}",
            cpu.0,
            msg.trim_end(),
            mbx_read(M_T1_POST),
            mbx_read(M_OBS_NEW)
        ));
        return;
    }
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_USER cpu={} {}",
        cpu.0,
        msg.trim_end()
    ));
    let both_done = mbx_read(M_T1_DONE) != 0 && mbx_read(M_T0_DONE) != 0;
    if both_done && !DUMPED.swap(true, Ordering::AcqRel) {
        dump();
    }
}

fn dump() {
    let mut gaps = 0u64;
    let mut named = 0u64;
    let mut coherent = 0u64;
    let mut post_rounds = 0u64;
    for i in 0..ROUNDS as usize {
        let seen = GAP_SEEN[i].load(Ordering::Acquire);
        let is_named = GAP_NAMED[i].load(Ordering::Acquire);
        if seen {
            gaps += 1;
        }
        if seen && is_named {
            named += 1;
        }
        let post = POST_VAL[i].load(Ordering::Acquire);
        let new = NEW_VAL[i].load(Ordering::Acquire);
        if post != 0 || new != 0 {
            post_rounds += 1;
            if post == new {
                coherent += 1;
            }
        }
        crate::kernel::printk::printk_emit_sync(format_args!(
            "VMI2_RECORD round={} gap={} named={} target_set=0x{:x} cr3=0x{:x} post=0x{:x} new=0x{:x}",
            i + 1,
            u8::from(seen),
            u8::from(is_named),
            GAP_TARGETS[i].load(Ordering::Acquire),
            GAP_CR3[i].load(Ordering::Acquire),
            post,
            new
        ));
    }
    // The verdict the grader keys on. Positive window evidence is `gaps_named >= 1`; a run with no
    // gap at all (`gaps == 0`) cannot qualify. `coherent` corroborates the observer's bytes but is
    // not asserted as architectural proof (a QEMU TLB model need not keep a stale entry).
    crate::kernel::printk::printk_emit_sync(format_args!(
        "VMI2_WITNESS_SEAL arch=x86_64 rounds={} gaps_observed={} gaps_named={} post_rounds={} coherent={} result={}",
        ROUNDS,
        gaps,
        named,
        post_rounds,
        coherent,
        if gaps >= 1 && named == gaps { "ok" } else { "fail" }
    ));
    crate::kernel::printk::printk_emit_sync(format_args!("VMI2_DUMP_END"));
}
