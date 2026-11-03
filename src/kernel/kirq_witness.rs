// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1-ACCEPTANCE §2 — the KERNEL-THREAD window of the device-interrupt witnesses
//! (`kthread-irq-witness`, layered on `riscv-uart-irq-witness`, `aarch64-pl011-irq-witness` or
//! `x86_64-uart-irq-witness`).
//!
//! The port witnesses exchange eight items whose interrupt lands on the idle wait (odd) or on the
//! receiver's user spin (even). This adds four more, items 9..=12, whose interrupt lands on a
//! running KERNEL THREAD. One witness thread, created by the selection owner on CPU 0 once the
//! eight are done, does for each item:
//!
//! 1. waits (yielding, so the receiver runs) until the item's byte is in the device;
//! 2. masks interrupts, asks the port fixture to open its gate for the item (the device may raise
//!    its line from now on), and enters [`spin_unmasked`]: patterned registers are set, interrupts
//!    are unmasked INSIDE the block, and it spins until this CPU's device-interrupt counter moves;
//! 3. masks again, checks every patterned register, restores the saved interrupt state, and
//!    reaches a scheduling point (the delivery woke the receiver and left a reschedule pending).
//!
//! It owns no part of the interrupt path. The interrupt is taken by the port's kernel-thread
//! vector, authenticated there, claimed once, settled by `settle_external_interrupt_at_bridge`
//! through the production route, and completed once; the port fixture's existing hooks drain the
//! device and record the origin. The window only decides WHEN the device may raise its line, as
//! the idle and user gates do.
//!
//! After the last item the thread keeps running (yielding) until the CPU's timer has interrupted
//! kernel threads three more times, so a boot shows timer service and kernel-thread progress
//! continuing after the last device interrupt, and then exits.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::kernel::kthread::KThreadCtx;
use crate::kernel::scheduler::{CpuId, TaskPriority};
use crate::runtime::SharedKernel;

#[cfg(target_arch = "aarch64")]
use crate::arch::aarch64::pl011_irq_witness::kirq_port as port;
#[cfg(target_arch = "riscv64")]
use crate::arch::riscv64::uart_irq_witness::kirq_port as port;
#[cfg(target_arch = "x86_64")]
use crate::arch::x86_64::uart_irq_witness::kirq_port as port;

/// The eight idle/user items of the port witness.
pub const BASE_ITEMS: u32 = 8;
/// Kernel-thread items after them.
pub const KTHREAD_ITEMS: u32 = 4;
pub const TOTAL_ITEMS: u32 = BASE_ITEMS + KTHREAD_ITEMS;

/// Bound on the spin for one item's interrupt (a cap, not a timing: ~tens of seconds under QEMU).
const SPIN_CAP: u64 = 1 << 32;
/// Bound on scheduling points spent waiting for one item's byte, or for the closing timer ticks.
const WAIT_CAP: u64 = 50_000_000;
/// Timer interrupts on kernel threads required after the last device interrupt.
const TICKS_AFTER: u64 = 3;
const SEED: u64 = 0x4B49_5251_0000_0000;

static STARTED: AtomicBool = AtomicBool::new(false);

/// The selection owner ran on `cpu`: create the window thread once, on CPU 0, when the port's
/// next item is the first kernel-thread item.
pub(crate) fn maybe_start(shared: &SharedKernel, cpu: CpuId) {
    if cpu.0 != 0
        || STARTED.load(Ordering::Acquire)
        || !port::source_live()
        || port::next_item() <= BASE_ITEMS
        || STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    match shared.kthreads.create(window, 0, cpu, TaskPriority::Normal) {
        Ok(id) => crate::yarm_log!("KIRQ_WINDOW_START kthread={:#x} cpu={}", id.raw(), cpu.0),
        Err(e) => crate::yarm_log!("KIRQ_WINDOW_START result=fail reason={:?}", e),
    }
}

fn window(ctx: &KThreadCtx, _arg: usize) {
    let pool = ctx.pool();
    let cpu = ctx.home();
    let kid = ctx.id().raw();
    let counter = pool.device_interrupt_counter_addr(cpu);
    let spin_fn = spin_unmasked as *const () as usize;
    for item in BASE_ITEMS + 1..=TOTAL_ITEMS {
        let seed = SEED | ((item as u64) << 16);
        let mut waits = 0u64;
        let (regs_ok, arrived, delta, resched) = loop {
            if port::next_item() == item && port::byte_present() {
                let was = crate::arch::kthread::irq_disable_save();
                // SAFETY: the counter is an `AtomicU64` in the pool; a plain load is what the
                // spin does.
                let initial = unsafe { core::ptr::read_volatile(counter) };
                if port::open_gate(item, kid) {
                    let mut out = [0u64; REGS];
                    // SAFETY: see `spin_unmasked`; interrupts are masked on entry and on return.
                    let left = unsafe { spin_unmasked(counter, initial, seed, &mut out) };
                    // SAFETY: as above.
                    let now = unsafe { core::ptr::read_volatile(counter) };
                    let resched = pool.resched_pending(cpu);
                    crate::arch::kthread::irq_restore(was);
                    let ok = (0..USED).all(|i| out[i] == seed.wrapping_add(i as u64 + 1));
                    break (ok, left != 0, now.wrapping_sub(initial), resched);
                }
                crate::arch::kthread::irq_restore(was);
            }
            waits += 1;
            if waits > WAIT_CAP {
                crate::yarm_log!(
                    "KIRQ_WINDOW item={} kthread={:#x} cpu={} result=fail reason=no_byte",
                    item,
                    kid,
                    cpu.0
                );
                ctx.exit();
            }
            let _ = ctx.yield_now();
        };
        // The thread continues on its own stack: the delivery left a reschedule pending, which
        // this scheduling point honours (the woken receiver runs), and the thread resumes after.
        let switched = ctx.sched_point();
        crate::yarm_log!(
            "KIRQ_WINDOW item={} kthread={:#x} cpu={} regs_ok={} arrived={} device_irqs={} resched_pending={} switched={} resumed=1 spin_fn={:#x}",
            item,
            kid,
            cpu.0,
            regs_ok as u8,
            arrived as u8,
            delta,
            resched as u8,
            matches!(switched, Ok(true)) as u8,
            spin_fn
        );
    }
    // Timer service and kernel-thread progress after the last device interrupt.
    let ticks = |p: &crate::kernel::kthread::KThreadPool| p.cpu_counters(cpu).timer_interrupts;
    let (t0, d0) = (ticks(pool), pool.cpu_counters(cpu).device_interrupts);
    let mut rounds = 0u64;
    while ticks(pool) < t0 + TICKS_AFTER && rounds < WAIT_CAP {
        rounds += 1;
        let _ = ctx.yield_now();
    }
    crate::yarm_log!(
        "KIRQ_WINDOW_DONE kthread={:#x} cpu={} rounds_after={} timer_irqs_after={} device_irqs_after={} result={}",
        kid,
        cpu.0,
        rounds,
        ticks(pool) - t0,
        pool.cpu_counters(cpu).device_interrupts - d0,
        if ticks(pool) >= t0 + TICKS_AFTER {
            "ok"
        } else {
            "fail"
        }
    );
}

// ── the register-patterned, internally unmasked spin ───────────────────────────────────────────

const REGS: usize = 24;

/// Set patterned general registers, UNMASK interrupts, spin until `*counter != initial` (this
/// CPU's device-interrupt counter moved: the device interrupt was taken on this thread and
/// returned) or the cap runs out, MASK interrupts again, and store the registers. The interrupt
/// can only land inside this block: it is entered and left masked. Returns the iterations left
/// (0 = cap ran out).
///
/// # Safety
/// Interrupts masked on entry; `counter` is readable; `out` holds [`REGS`] slots.
#[cfg(target_arch = "riscv64")]
#[inline(never)]
unsafe fn spin_unmasked(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; REGS],
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
            "csrsi sstatus, 2",
            "2:",
            "ld a4, 0(a0)",
            "bne a4, a1, 3f",
            "addi a2, a2, -1",
            "bnez a2, 2b",
            "3:",
            "csrci sstatus, 2",
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
const USED: usize = 21;

/// See the RISC-V version.
///
/// # Safety
/// As the RISC-V version.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
unsafe fn spin_unmasked(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; REGS],
) -> u64 {
    let mut left: u64 = SPIN_CAP;
    // SAFETY: registers named below are declared clobbered; `out` holds 24 slots.
    unsafe {
        core::arch::asm!(
            "lea rax, [r8 + 1]", "lea r9, [r8 + 2]", "lea r10, [r8 + 3]", "lea r11, [r8 + 4]",
            "lea r12, [r8 + 5]", "lea r13, [r8 + 6]", "lea r14, [r8 + 7]", "lea r15, [r8 + 8]",
            "sti",
            "2:",
            "mov r8, [rdi]",
            "cmp r8, rsi",
            "jne 3f",
            "dec rdx",
            "jnz 2b",
            "3:",
            "cli",
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
const USED: usize = 8;

/// See the RISC-V version.
///
/// # Safety
/// As the RISC-V version.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
unsafe fn spin_unmasked(
    counter: *const u64,
    initial: u64,
    seed: u64,
    out: &mut [u64; REGS],
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
            "msr daifclr, #2",
            "2:",
            "ldr x4, [x0]",
            "cmp x4, x1",
            "b.ne 3f",
            "subs x2, x2, #1",
            "b.ne 2b",
            "3:",
            "msr daifset, #2",
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
const USED: usize = 23;
