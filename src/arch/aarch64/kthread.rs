// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — AArch64 kernel-thread switch, first entry and exception vectors.
//!
//! Voluntary switch state (`KSwitchContext`): `x30` (lr), `sp` (EL1 runs on `SP_EL1`) and the
//! AAPCS64 callee-saved `x19..x29`. `TPIDR_EL1` (this CPU's logical id) is per CPU, not per thread,
//! and a kernel thread and its dispatcher run on the same CPU; `SP_EL0` and `TPIDR_EL0` belong to
//! the interrupted user and are not touched. Interrupted state: `x0..x30`, `ELR_EL1` and
//! `SPSR_EL1`, saved ON THE KERNEL THREAD'S STACK by the kernel-thread table's "current EL with
//! SPx" IRQ entry. No FP/SIMD register is saved: the kernel uses none (KERNEL-NOFP1), and the
//! interrupted user's FP/SIMD home was committed by the user vector before any kernel thread ran.

#[cfg(not(feature = "hosted-dev"))]
use crate::kernel::scheduler::CpuId;

/// Voluntary switch state. Offsets are used by the switch below.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KSwitchContext {
    lr: usize,      // 0
    sp: usize,      // 8
    x: [usize; 11], // 16..=96: x19..x29
}

impl KSwitchContext {
    pub const fn zeroed() -> Self {
        Self {
            lr: 0,
            sp: 0,
            x: [0; 11],
        }
    }
}

#[cfg(not(feature = "hosted-dev"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_aarch64_kthread_switch
    .type yarm_aarch64_kthread_switch, %function
yarm_aarch64_kthread_switch:
    mov x9, sp
    stp x30, x9, [x0, #0]
    stp x19, x20, [x0, #16]
    stp x21, x22, [x0, #32]
    stp x23, x24, [x0, #48]
    stp x25, x26, [x0, #64]
    stp x27, x28, [x0, #80]
    str x29, [x0, #96]
    ldp x30, x9, [x1, #0]
    ldp x19, x20, [x1, #16]
    ldp x21, x22, [x1, #32]
    ldp x23, x24, [x1, #48]
    ldp x25, x26, [x1, #64]
    ldp x27, x28, [x1, #80]
    ldr x29, [x1, #96]
    mov sp, x9
    ret

    .global yarm_aarch64_kthread_trampoline
    .type yarm_aarch64_kthread_trampoline, %function
yarm_aarch64_kthread_trampoline:
    mov x0, x19
    mov x1, x20
    bl yarm_kthread_first_entry
    brk #0

    // KTHREAD-CLOSURE1 §C — every current-EL/SPx entry first tests the stack pointer it would
    // use, without touching memory: a kernel thread's stack is the bottom 16 KiB of a 16 MiB
    // stride of the window, so bit 14 is clear on it once a 272-byte frame is reserved, and set
    // in the 16 KiB just below it (the top of its gap). Deeper in the gap bit 14 repeats, so this
    // test is not the defence: every overflow is a store into the gap, hence a synchronous
    // exception, and the synchronous entry always moves to the emergency stack itself. The test
    // only reports a shallow overflow found at entry (an interrupt) with the thread's own pc.
    // `sp` and `x0` are swapped arithmetically to get a scratch register (no store); both are
    // restored before the entry continues.
    .macro KT_SP_CHECK target
    sub sp, sp, #272
    add sp, sp, x0
    sub x0, sp, x0
    tbnz x0, #14, yarm_aarch64_kthread_overflow
    sub x0, sp, x0
    sub sp, sp, x0
    add sp, sp, #272
    b \target
    .endm

    // The kernel-thread vector table, in `VBAR_EL1` only while a kernel thread runs. Only the
    // "current EL with SPx" IRQ entry is serviced; a synchronous exception at EL1 is reported as
    // a kernel-thread fault and halts; every other entry keeps the production stub's handling.
    .balign 2048
    .global yarm_aarch64_kthread_vectors
    .type yarm_aarch64_kthread_vectors, %function
yarm_aarch64_kthread_vectors:
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64_sp0
    .balign 128
    KT_SP_CHECK yarm_aarch64_kthread_sync_entry
    .balign 128
    KT_SP_CHECK yarm_aarch64_kthread_irq_entry
    .balign 128
    KT_SP_CHECK yarm_aarch64_vector_fiq_current
    .balign 128
    KT_SP_CHECK yarm_aarch64_vector_serror_current
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64
    .balign 128
    b yarm_aarch64_vector_sync_lower_a32
    .balign 128
    b yarm_aarch64_vector_irq_lower_a32
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a32
    .balign 128
    b yarm_aarch64_vector_serror_lower_a32
    .balign 128

    // IRQ at EL1 on the kernel thread's SP_EL1 stack: x0..x30 at 0..=240, ELR_EL1 at 248,
    // SPSR_EL1 at 256 (272 bytes, 16-byte aligned). Restores ELR/SPSR before the GPRs (x9/x10 are
    // reloaded afterwards), then `eret` to the interrupted instruction.
    .global yarm_aarch64_kthread_irq_entry
    .type yarm_aarch64_kthread_irq_entry, %function
yarm_aarch64_kthread_irq_entry:
    sub sp, sp, #272
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    mrs x9, elr_el1
    mrs x10, spsr_el1
    stp x9, x10, [sp, #248]
    mov x0, sp
    bl yarm_aarch64_kthread_interrupt
    ldp x9, x10, [sp, #248]
    msr elr_el1, x9
    msr spsr_el1, x10
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    ldr x30, [sp, #240]
    add sp, sp, #272
    eret

    // KTHREAD-CLOSURE1 §C — a vector entry found its stack pointer in a guard gap: x0 holds it
    // (with the 272-byte reservation). Switch to this CPU's emergency stack (`TPIDR_EL1` is the
    // logical CPU) and report; never returns.
    .global yarm_aarch64_kthread_overflow
    .type yarm_aarch64_kthread_overflow, %function
yarm_aarch64_kthread_overflow:
    mov x3, x0
    adrp x1, YARM_AARCH64_KSTACK_EMERGENCY
    add x1, x1, :lo12:YARM_AARCH64_KSTACK_EMERGENCY
    mrs x2, tpidr_el1
    and x2, x2, #7
    add x1, x1, x2, lsl #14
    add sp, x1, #16384
    mrs x0, esr_el1
    mrs x1, elr_el1
    mrs x2, far_el1
    bl yarm_aarch64_kthread_stack_fault
    b .

    .global yarm_aarch64_kthread_sync_entry
    .type yarm_aarch64_kthread_sync_entry, %function
yarm_aarch64_kthread_sync_entry:
    // KTHREAD-CLOSURE1 §C: a synchronous exception on a kernel thread is always fatal, and its
    // stack may be the cause (a store that faulted in a guard gap without moving `sp` would fault
    // again at once in any handler on that stack). Report from this CPU's emergency stack.
    mov x3, sp
    adrp x1, YARM_AARCH64_KSTACK_EMERGENCY
    add x1, x1, :lo12:YARM_AARCH64_KSTACK_EMERGENCY
    mrs x2, tpidr_el1
    and x2, x2, #7
    add x1, x1, x2, lsl #14
    add sp, x1, #16384
    mrs x0, esr_el1
    mrs x1, elr_el1
    mrs x2, far_el1
    bl yarm_aarch64_kthread_fault
    brk #0
"#
);

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    fn yarm_aarch64_kthread_switch(prev: *mut KSwitchContext, next: *const KSwitchContext);
    fn yarm_aarch64_kthread_trampoline();
    static yarm_aarch64_kthread_vectors: u8;
    static yarm_aarch64_vector_table_el1: u8;
}

/// Save the running continuation into `prev` and resume `next`.
///
/// # Safety
/// Interrupts masked; `next` was saved by this routine or prepared by [`prepare_first_entry`].
#[cfg(not(feature = "hosted-dev"))]
pub unsafe fn switch(prev: *mut KSwitchContext, next: *const KSwitchContext) {
    // SAFETY: per the caller's contract.
    unsafe { yarm_aarch64_kthread_switch(prev, next) }
}

/// Make `ctx` enter the trampoline on a fresh stack ending at `stack_top`, carrying `pool` and
/// `slot` in `x19`/`x20`.
///
/// # Safety
/// `stack_top` is the exclusive end of a writable stack no CPU is using.
#[cfg(not(feature = "hosted-dev"))]
pub unsafe fn prepare_first_entry(
    ctx: &mut KSwitchContext,
    stack_top: usize,
    _write_top: usize,
    pool: usize,
    slot: usize,
) {
    *ctx = KSwitchContext::zeroed();
    ctx.lr = yarm_aarch64_kthread_trampoline as *const () as usize;
    ctx.sp = stack_top & !0xF;
    ctx.x[0] = pool;
    ctx.x[1] = slot;
}

/// KTHREAD-CLOSURE1 §C: kernel threads run on the kernel root the dispatcher already entered.
#[cfg(not(feature = "hosted-dev"))]
pub fn enter_kthread_root(_cpu: CpuId) {}

/// KTHREAD-CLOSURE1 §C: nothing to leave.
#[cfg(not(feature = "hosted-dev"))]
pub fn leave_kthread_root(_cpu: CpuId) {}

/// KTHREAD-CLOSURE1 §C: the protected kernel-thread stack window, once installed.
#[cfg(not(feature = "hosted-dev"))]
pub fn kstack_window() -> Option<usize> {
    crate::arch::aarch64::kstack_window::window()
}

#[cfg(not(feature = "hosted-dev"))]
const DAIF_I: usize = 1 << 7;

#[cfg(not(feature = "hosted-dev"))]
pub fn irqs_enabled() -> bool {
    let daif: usize;
    // SAFETY: reads DAIF only.
    unsafe { core::arch::asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack)) };
    daif & DAIF_I == 0
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_enable() {
    // SAFETY: unmask IRQ on a kernel thread whose vectors are installed.
    unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack)) };
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_disable_save() -> bool {
    let was = irqs_enabled();
    // SAFETY: mask IRQ.
    unsafe { core::arch::asm!("msr daifset, #2", options(nomem, nostack)) };
    was
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_restore(was_enabled: bool) {
    if was_enabled {
        irq_enable();
    }
}

#[cfg(not(feature = "hosted-dev"))]
pub fn current_sp() -> usize {
    let sp: usize;
    // SAFETY: reads the stack pointer only.
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags))
    };
    sp
}

#[cfg(not(feature = "hosted-dev"))]
fn set_vbar(base: usize) {
    // SAFETY: interrupts are masked; both tables are 2 KiB aligned vector tables in `.text`.
    unsafe { core::arch::asm!("msr VBAR_EL1, {}", "isb", in(reg) base, options(nostack)) };
}

/// A kernel thread is about to run on `cpu`: an idle secondary's timer, parked behind the idle
/// backstop at its idle arrival, gets its own period back (as it does before resuming a task).
#[cfg(not(feature = "hosted-dev"))]
pub fn before_kthread_dispatch(cpu: CpuId) {
    crate::arch::aarch64::smp::unpark_ap_timer(cpu.0 as usize);
    // The exception whose drain is entering this thread may still hold its claim; complete it
    // now, or the GIC's running priority would hold off the thread's timer and reschedule IRQs.
    let _ = crate::arch::aarch64::irq::complete_in_flight(cpu);
}

/// `VBAR_EL1` names the kernel-thread vector table.
#[cfg(not(feature = "hosted-dev"))]
pub fn enter_kthread_vectors(_cpu: CpuId) {
    set_vbar(core::ptr::addr_of!(yarm_aarch64_kthread_vectors) as usize);
}

/// `VBAR_EL1` names the production vector table again.
#[cfg(not(feature = "hosted-dev"))]
pub fn leave_kthread_vectors(_cpu: CpuId) {
    set_vbar(core::ptr::addr_of!(yarm_aarch64_vector_table_el1) as usize);
}

/// A synchronous exception on a kernel thread: kernel-fatal, reported by name.
/// KTHREAD-CLOSURE1 §C — an exception taken with the stack pointer in a guard gap, on this CPU's
/// emergency stack. Reported raw (the faulting thread may hold the console lock); this CPU halts.
#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_kthread_stack_fault(esr: u64, elr: u64, far: u64, sp: u64) -> ! {
    let shared = crate::arch::aarch64::boot::trap_shared_kernel();
    // A data abort reports the faulting address; anything else is located by its stack pointer.
    let ec = (esr >> 26) & 0x3F;
    let addr = if ec == 0x25 { far } else { sp };
    crate::kernel::kthread::report_kernel_thread_fault(
        shared.map(|s| &s.kthreads),
        "aarch64",
        crate::arch::aarch64::this_cpu_index(),
        esr,
        addr as usize,
        sp as usize + 272,
        elr as usize,
        crate::arch::aarch64::console::write_line_unlocked,
    );
    loop {
        // SAFETY: halting this CPU with interrupts masked.
        unsafe { core::arch::asm!("msr daifset, #0xf", "wfi", options(nomem, nostack)) };
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_kthread_fault(esr: u64, elr: u64, far: u64, sp: u64) -> ! {
    // KTHREAD-CLOSURE1 §C (on the emergency stack): a data abort in a guard gap, or any exception
    // taken with the thread's stack pointer in one, is that thread's overflow.
    let in_gap = |a: u64| {
        crate::arch::aarch64::boot::trap_shared_kernel()
            .is_some_and(|s| s.kthreads.run_view().gap_owner(a as usize).is_some())
    };
    if ((esr >> 26) & 0x3F == 0x25 && in_gap(far)) || in_gap(sp) {
        yarm_aarch64_kthread_stack_fault(esr, elr, far, sp - 272);
    }
    panic!(
        "KTHREAD_FAULT arch=aarch64 cpu={} esr={:#x} elr={:#x} far={:#x} result=fatal",
        crate::arch::aarch64::this_cpu_index(),
        esr,
        elr,
        far
    );
}

/// The kernel-thread IRQ bridge: authenticate the origin, claim the interrupt, service the timer,
/// the reschedule SGI or a device interrupt, complete it, record a pending reschedule, return to
/// the entry. Never switches; the timer and SGI paths never log.
///
/// KTHREAD1-ACCEPTANCE §2 — a device INTID is settled exactly as the production vector settles
/// one taken from EL0 or the idle boundary: `settle_external_interrupt_at_bridge` delivers it
/// through the production route, its own completion step is the AArch64 single-step
/// acknowledgement (inert: the claim's one completion belongs to the vector that took it), and
/// this bridge — that vector's tail — writes the one `GICC_EOIR` afterwards. Delivery may wake a
/// task; the thread is not switched, a reschedule is left for its next scheduling point.
/// GICv2: INTIDs 0..=15 are SGIs, 16..=31 PPIs, 32.. shared peripheral interrupts.
#[cfg(not(feature = "hosted-dev"))]
const FIRST_SPI_INTID: u16 = 32;

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_kthread_interrupt(frame: usize) {
    let Some(shared) = crate::arch::aarch64::boot::trap_shared_kernel() else {
        panic!("KTHREAD_IRQ_UNAUTHENTICATED arch=aarch64 reason=no_kernel");
    };
    let pool = &shared.kthreads;
    let hardware = CpuId(crate::arch::aarch64::this_cpu_index() as u8);
    let (cpu, id) = match pool.authenticate_interrupt(frame, hardware) {
        Ok(ok) => ok,
        Err(r) => panic!(
            "KTHREAD_IRQ_UNAUTHENTICATED arch=aarch64 cpu={} reason={}",
            hardware.0,
            r.marker()
        ),
    };
    let _ = id;
    // SAFETY: the entry saved ELR_EL1 (the interrupted PC) at offset 248.
    let elr = unsafe { *((frame + 248) as *const u64) };
    let Some(ack) = crate::arch::aarch64::irq::claim_interrupt() else {
        panic!(
            "KTHREAD_IRQ_UNAUTHENTICATED arch=aarch64 cpu={} reason=no_controller",
            cpu.0
        );
    };
    let intid = ack.intid();
    if crate::arch::aarch64::irq::intid_is_special(intid) {
        return;
    }
    if intid == crate::arch::aarch64::irq::ARCH_TIMER_PPI_INTID {
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_interrupt(cpu, id, 1, elr, frame, 0);
        let _ = pool.service_timer_interrupt(cpu);
        crate::arch::aarch64::irq::complete_interrupt(ack);
    } else if intid == crate::arch::gicv2_sgi::RESCHEDULE_SGI_INTID {
        crate::arch::aarch64::smp::note_sgi_arrival(
            cpu,
            ack.raw(),
            crate::arch::aarch64::smp::ArrivalOrigin::Kernel,
            elr,
        );
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_interrupt(
            cpu,
            id,
            2,
            elr,
            frame,
            crate::arch::aarch64::smp::sgi_source_cpu(ack.raw()).map_or(0, |c| 1u64 << c),
        );
        pool.note_resched_interrupt(cpu, true);
        crate::arch::aarch64::irq::complete_interrupt(ack);
    } else if intid < FIRST_SPI_INTID {
        // A private interrupt (SGI/PPI) other than the two above: this port routes none of them
        // on a kernel thread, so it stays fatal, as before. Device interrupts are SPIs.
        crate::arch::aarch64::irq::complete_interrupt(ack);
        panic!(
            "KTHREAD_IRQ_UNSUPPORTED arch=aarch64 cpu={} intid={} result=fatal",
            cpu.0, intid
        );
    } else {
        // SAFETY: the entry saved SPSR_EL1 (the interrupted PSTATE) at offset 256.
        let spsr = unsafe { *((frame + 256) as *const u64) };
        #[cfg(feature = "aarch64-pl011-irq-witness")]
        crate::arch::aarch64::pl011_irq_witness::note_kthread_claim_and_drain(
            intid,
            spsr,
            elr,
            cpu,
            id.raw(),
        );
        #[cfg(not(feature = "aarch64-pl011-irq-witness"))]
        let _ = spsr;
        let settled = crate::kernel::syscall_split::settle_external_interrupt_at_bridge(
            shared,
            cpu,
            u32::from(intid),
            crate::arch::hal_adapters::InterruptCompletion::ArchSingleStep { line: intid },
        );
        // THE completion for this claim, once.
        crate::arch::aarch64::irq::complete_interrupt(ack);
        #[cfg(feature = "aarch64-pl011-irq-witness")]
        crate::arch::aarch64::pl011_irq_witness::note_completion_written(ack);
        if let Some(Err(err)) = settled {
            // The EL0 bridge leaves its trap as an error here (a full notification queue); the
            // claim is completed either way. A kernel thread has no trap result to carry it.
            crate::yarm_log!(
                "KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED arch=aarch64 cpu={} kthread={:#x} intid={} err={:?}",
                cpu.0,
                id.raw(),
                intid,
                err
            );
        }
        pool.note_device_interrupt(cpu);
    }
}
