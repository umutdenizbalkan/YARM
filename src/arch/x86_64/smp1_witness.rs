// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP1 §3 — the two-CPU IPI and TLB witness (x86_64). Built only with `x86-smp1-witness`
//! and armed only by `yarm.x86_64_smp1_witness=1` on the cross-CPU reply profile.
//!
//! It exercises PRODUCTION owners and observes them; it performs none of the work it grades:
//!
//! * wake IPIs are the ones the direct NR6/NR7 transactions send after their enqueue commits;
//! * each W replacement is a genuine NR 3 `VmMap` issued by the OTHER task, so the install,
//!   the displaced-object pin, the shootdown (post, IPI, ACK wait) and the settle are all the
//!   VM owner's, and the invalidation + ACK are the target CPU's own 0xF1 stub;
//! * selection and resume are the schedulers' (the AP saved-frame path on CPU 1, the production
//!   idle advance on CPU 0).
//!
//! The witness's own kernel action is setup plus ONE probe: when a requester arms a round it
//! re-points the target's residency page R at a second frame through `page_table::map_page`,
//! which invalidates only on the CPU executing it (the requester's). A target that still reads
//! the OLD R after the round therefore never reloaded CR3 in between, so its view of W can only
//! have changed through the targeted `invlpg` — an incidental flush cannot make a broken IPI
//! pass. The user programs are in `smp1_witness.S`.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use crate::kernel::scheduler::CpuId;

#[cfg(not(feature = "hosted-dev"))]
core::arch::global_asm!(include_str!("smp1_witness.S"));

/// FXSAVE pattern image (and, at +0x400, the programs' capture area).
pub const PAT_VA: u64 = 0x2006_0000;
/// The one frame both address spaces map.
pub const MBX_VA: u64 = 0x2007_0000;
/// The page each task creates (NR 13) and the other task replaces (NR 3).
pub const W_VA: u64 = 0x2008_0000;
/// The residency probe.
pub const R_VA: u64 = 0x2009_0000;
/// Kernel-only alias of the probe's second frame, used to fill it at provisioning.
pub const R_ALT_VA: u64 = 0x200A_0000;
pub const OLD_R: u64 = 0x5151_5151_5151_5151;
pub const NEW_R: u64 = 0x5252_5252_5252_5252;

/// Distinct x87/SSE control settings and XMM seeds for the two tasks.
pub const SERVER_FCW: u16 = 0x027F;
pub const SERVER_MXCSR: u32 = 0x3F80;
pub const SERVER_SEED: u8 = 0x3C;
pub const CLIENT_FCW: u16 = 0x0F7F;
pub const CLIENT_MXCSR: u32 = 0x9F80;
pub const CLIENT_SEED: u8 = 0xC5;

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    static yarm_smp1_server_start: u8;
    static yarm_smp1_server_end: u8;
    static yarm_smp1_server_recv_cap_1: u8;
    static yarm_smp1_server_recv_cap_2: u8;
    static yarm_smp1_server_client_as_cap: u8;
    static yarm_smp1_client_start: u8;
    static yarm_smp1_client_end: u8;
    static yarm_smp1_client_send_cap: u8;
    static yarm_smp1_client_reply_cap_r9: u8;
    static yarm_smp1_client_reply_cap_1: u8;
    static yarm_smp1_client_reply_cap_2: u8;
    static yarm_smp1_client_server_as_cap: u8;
}

/// The byte image of one program with every capability placeholder patched.
#[cfg(not(feature = "hosted-dev"))]
fn patched(start: *const u8, end: *const u8, patches: &[(*const u8, u32)]) -> alloc::vec::Vec<u8> {
    let len = end as usize - start as usize;
    // SAFETY: `start..end` is the program's bytes inside this kernel image's text.
    let mut image = unsafe { core::slice::from_raw_parts(start, len) }.to_vec();
    for &(at, value) in patches {
        let off = at as usize - start as usize;
        image[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }
    image
}

/// The server program: its receive cap at both receive sites, and a cap to the client's
/// address space for its NR 3.
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn server_image(recv_cap: u32, client_as_cap: u32) -> alloc::vec::Vec<u8> {
    // SAFETY: address-of only.
    unsafe {
        patched(
            &raw const yarm_smp1_server_start,
            &raw const yarm_smp1_server_end,
            &[
                (&raw const yarm_smp1_server_recv_cap_1, recv_cap),
                (&raw const yarm_smp1_server_recv_cap_2, recv_cap),
                (&raw const yarm_smp1_server_client_as_cap, client_as_cap),
            ],
        )
    }
}

/// The client program: the request SEND cap, its reply RECEIVE cap (NR6 arg5 and both receive
/// sites), and a cap to the server's address space for its NR 3.
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn client_image(
    send_cap: u32,
    reply_cap: u32,
    server_as_cap: u32,
) -> alloc::vec::Vec<u8> {
    // SAFETY: address-of only.
    unsafe {
        patched(
            &raw const yarm_smp1_client_start,
            &raw const yarm_smp1_client_end,
            &[
                (&raw const yarm_smp1_client_send_cap, send_cap),
                (&raw const yarm_smp1_client_reply_cap_r9, reply_cap),
                (&raw const yarm_smp1_client_reply_cap_1, reply_cap),
                (&raw const yarm_smp1_client_reply_cap_2, reply_cap),
                (&raw const yarm_smp1_client_server_as_cap, server_as_cap),
            ],
        )
    }
}

/// A valid FXSAVE image: `fcw`, FSW 0, FTW empty, `mxcsr`, ST0..7 zero, and XMM0..15 filled
/// from `seed` so no two registers — and no two tasks — share a value.
pub fn fp_pattern(seed: u8, fcw: u16, mxcsr: u32) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[0..2].copy_from_slice(&fcw.to_le_bytes());
    b[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    b[28..32].copy_from_slice(&0x0000_FFFFu32.to_le_bytes());
    for i in 0..16 {
        for k in 0..16 {
            b[160 + 16 * i + k] = seed.wrapping_add((i * 16 + k) as u8).wrapping_mul(0x9D) ^ 0x5A;
        }
    }
    b
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static SERVER_ASID: AtomicU16 = AtomicU16::new(0);
static CLIENT_ASID: AtomicU16 = AtomicU16::new(0);
static SERVER_R_NEW: AtomicU64 = AtomicU64::new(0);
static CLIENT_R_NEW: AtomicU64 = AtomicU64::new(0);
static T01_FINAL: AtomicBool = AtomicBool::new(false);
static T10_FINAL: AtomicBool = AtomicBool::new(false);
static SUMMARY: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Provisioning: the two address spaces and each probe's second frame.
pub(crate) fn record_targets(
    server_asid: u16,
    server_r_new: u64,
    client_asid: u16,
    client_r_new: u64,
) {
    SERVER_ASID.store(server_asid, Ordering::Release);
    SERVER_R_NEW.store(server_r_new, Ordering::Release);
    CLIENT_ASID.store(client_asid, Ordering::Release);
    CLIENT_R_NEW.store(client_r_new, Ordering::Release);
}

/// The VM-owner witness markers are confined to the one page the witness replaces.
pub fn watches(va: u64) -> bool {
    enabled() && va == W_VA
}

pub fn this_cpu() -> CpuId {
    crate::arch::x86_64::descriptor_tables::current_cpu_id()
}

/// The OTHER CPU's TLB mailbox as the requester sees it once its shootdown has returned: the
/// witness runs on exactly two CPUs, so the target of a request from this CPU is the other one.
/// Every field is written by its owner only — the request generation by the requester's post, the
/// ACK generation and the interrupted privilege level by the target's own 0xF1 stub.
pub fn target_mailbox() -> TargetMailbox {
    use crate::arch::x86_64::percpu;
    let requester = this_cpu();
    let target = CpuId(u8::from(requester.0 == 0));
    TargetMailbox {
        requester,
        target,
        req_gen: percpu::tlb_req_gen(target),
        ack_gen: percpu::tlb_ack_gen(target),
        origin: percpu::tlb_ack_origin_str(percpu::tlb_ack_origin(target)),
    }
}

pub struct TargetMailbox {
    requester: CpuId,
    target: CpuId,
    req_gen: u32,
    ack_gen: u32,
    origin: &'static str,
}

impl core::fmt::Display for TargetMailbox {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "requester_cpu={} target_cpu={} target_req_gen={} target_ack_gen={} target_origin={}",
            self.requester.0, self.target.0, self.req_gen, self.ack_gen, self.origin
        )
    }
}

/// The DebugLog observation point (both the split and the broad route call it). Every line it
/// emits is synchronous (`printk_emit_sync`), so none can be lost to a ring overflow.
///
/// `SMP1_ARM_R target=…` from a requester re-points the named target's R. The two target
/// verdicts, once both are in, release the summary: every number in it is read from the state
/// its owner keeps — the 0xF1 stub's per-CPU arrival counters split by the privilege level each
/// arrival interrupted, and each CPU's TLB mailbox generations.
pub fn observe_user_marker(msg: &str) {
    if !enabled() || !msg.starts_with("SMP1_") {
        return;
    }
    // Every witness verdict is echoed SYNCHRONOUSLY: the shared printk ring can drop a pushed
    // line while both CPUs log, and a grader must never mistake a dropped line for an absent
    // event. The grader keys on these echoes, not on `USER_LOG`.
    crate::kernel::printk::printk_emit_sync(format_args!(
        "SMP1_USER cpu={} {}",
        this_cpu().0,
        msg.trim_end()
    ));
    if let Some(rest) = msg.strip_prefix("SMP1_ARM_R target=") {
        let (asid, new_phys) = if rest.starts_with("server") {
            (
                SERVER_ASID.load(Ordering::Acquire),
                SERVER_R_NEW.load(Ordering::Acquire),
            )
        } else if rest.starts_with("client") {
            (
                CLIENT_ASID.load(Ordering::Acquire),
                CLIENT_R_NEW.load(Ordering::Acquire),
            )
        } else {
            return;
        };
        let flags = crate::kernel::vm::PageFlags::USER_RW;
        let result = crate::arch::x86_64::page_table::map_page(
            crate::kernel::vm::Asid(asid),
            crate::kernel::vm::VirtAddr(R_VA),
            crate::kernel::vm::PhysAddr(new_phys),
            flags,
        );
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP1_WITNESS_R_REPOINTED target={} asid={} va=0x{:x} new_phys=0x{:x} invalidated_on_cpu={} ok={}",
            if rest.starts_with("server") {
                "server"
            } else {
                "client"
            },
            asid,
            R_VA,
            new_phys,
            this_cpu().0,
            u8::from(result.is_ok())
        ));
        return;
    }
    if msg.starts_with("SMP1_T01_TARGET_") {
        T01_FINAL.store(true, Ordering::Release);
    } else if msg.starts_with("SMP1_T10_TARGET_") {
        T10_FINAL.store(true, Ordering::Release);
    } else {
        return;
    }
    if !(T01_FINAL.load(Ordering::Acquire) && T10_FINAL.load(Ordering::Acquire))
        || SUMMARY.swap(true, Ordering::AcqRel)
    {
        return;
    }
    use crate::arch::x86_64::percpu;
    for cpu in [CpuId(0), CpuId(1)] {
        let (w, k, u) = percpu::remote_wake_arrivals(cpu);
        crate::kernel::printk::printk_emit_sync(format_args!(
            "SMP1_WITNESS_CPU cpu={} wake_arrivals={} kernel_origin={} user_origin={} tlb_req_gen={} tlb_ack_gen={}",
            cpu.0,
            w,
            k,
            u,
            percpu::tlb_req_gen(cpu),
            percpu::tlb_ack_gen(cpu)
        ));
    }
    crate::kernel::printk::printk_emit_sync(format_args!("SMP1_WITNESS_SUMMARY result=ok"));
}
