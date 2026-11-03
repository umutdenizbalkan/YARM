// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — RISC-V kernel-thread switch, first entry and interrupt vector.
//!
//! Voluntary switch state (`KSwitchContext`): `ra`, `sp` and the psABI callee-saved `s0..s11`.
//! `gp` and `tp` are not switched: the kernel relies on neither (the trap vector leaves the
//! interrupted user's values in them), and a kernel thread and its dispatcher run on the same hart.
//! Interrupted state: all 31 general registers plus `sepc` and `sstatus` (and `scause`/`stval` for
//! the handler), saved in the existing `RiscvTrapFrame` layout ON THE KERNEL THREAD'S STACK by
//! `yarm_riscv64_kthread_vector`. `sscratch` is not touched: it keeps naming this hart's trap-stack
//! top, which is also the independent hardware identity the handler checks. No FP/vector state:
//! the kernel uses none (KERNEL-NOFP1) and user tasks run with FS/VS Off.

#[cfg(not(feature = "hosted-dev"))]
use crate::kernel::scheduler::CpuId;

/// Voluntary switch state. Offsets are used by the switch below.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KSwitchContext {
    ra: usize,      // 0
    sp: usize,      // 8
    s: [usize; 12], // 16..=104: s0..s11
}

impl KSwitchContext {
    pub const fn zeroed() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }
}

#[cfg(not(feature = "hosted-dev"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_riscv64_kthread_switch
    .type yarm_riscv64_kthread_switch, @function
yarm_riscv64_kthread_switch:
    sd ra, 0(a0)
    sd sp, 8(a0)
    sd s0, 16(a0)
    sd s1, 24(a0)
    sd s2, 32(a0)
    sd s3, 40(a0)
    sd s4, 48(a0)
    sd s5, 56(a0)
    sd s6, 64(a0)
    sd s7, 72(a0)
    sd s8, 80(a0)
    sd s9, 88(a0)
    sd s10, 96(a0)
    sd s11, 104(a0)
    ld ra, 0(a1)
    ld sp, 8(a1)
    ld s0, 16(a1)
    ld s1, 24(a1)
    ld s2, 32(a1)
    ld s3, 40(a1)
    ld s4, 48(a1)
    ld s5, 56(a1)
    ld s6, 64(a1)
    ld s7, 72(a1)
    ld s8, 80(a1)
    ld s9, 88(a1)
    ld s10, 96(a1)
    ld s11, 104(a1)
    ret

    .global yarm_riscv64_kthread_trampoline
    .type yarm_riscv64_kthread_trampoline, @function
yarm_riscv64_kthread_trampoline:
    mv a0, s0
    mv a1, s1
    call yarm_kthread_first_entry
    unimp

    // The kernel-thread trap vector, installed in `stvec` only while a kernel thread runs. The
    // trap was taken in S-mode on the kernel thread's stack, so `sp` is that stack: the frame is
    // pushed there, below the interrupted `sp`, in the `RiscvTrapFrame` layout (304 bytes).
    .balign 4
    .global yarm_riscv64_kthread_vector
    .type yarm_riscv64_kthread_vector, @function
yarm_riscv64_kthread_vector:
    addi sp, sp, -304
    sd x1,   0(sp)
    sd x3,  16(sp)
    sd x4,  24(sp)
    sd x5,  32(sp)
    sd x6,  40(sp)
    sd x7,  48(sp)
    sd x8,  56(sp)
    sd x9,  64(sp)
    sd x10, 72(sp)
    sd x11, 80(sp)
    sd x12, 88(sp)
    sd x13, 96(sp)
    sd x14, 104(sp)
    sd x15, 112(sp)
    sd x16, 120(sp)
    sd x17, 128(sp)
    sd x18, 136(sp)
    sd x19, 144(sp)
    sd x20, 152(sp)
    sd x21, 160(sp)
    sd x22, 168(sp)
    sd x23, 176(sp)
    sd x24, 184(sp)
    sd x25, 192(sp)
    sd x26, 200(sp)
    sd x27, 208(sp)
    sd x28, 216(sp)
    sd x29, 224(sp)
    sd x30, 232(sp)
    sd x31, 240(sp)
    addi t0, sp, 304
    sd t0,   8(sp)
    csrr t0, sepc
    sd t0, 248(sp)
    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, scause
    sd t0, 264(sp)
    csrr t0, stval
    sd t0, 272(sp)
    mv a0, sp
    call yarm_riscv64_kthread_trap
    ld t0, 248(sp)
    csrw sepc, t0
    ld t0, 256(sp)
    csrw sstatus, t0
    ld x1,   0(sp)
    ld x3,  16(sp)
    ld x4,  24(sp)
    ld x5,  32(sp)
    ld x6,  40(sp)
    ld x7,  48(sp)
    ld x8,  56(sp)
    ld x9,  64(sp)
    ld x10, 72(sp)
    ld x11, 80(sp)
    ld x12, 88(sp)
    ld x13, 96(sp)
    ld x14, 104(sp)
    ld x15, 112(sp)
    ld x16, 120(sp)
    ld x17, 128(sp)
    ld x18, 136(sp)
    ld x19, 144(sp)
    ld x20, 152(sp)
    ld x21, 160(sp)
    ld x22, 168(sp)
    ld x23, 176(sp)
    ld x24, 184(sp)
    ld x25, 192(sp)
    ld x26, 200(sp)
    ld x27, 208(sp)
    ld x28, 216(sp)
    ld x29, 224(sp)
    ld x30, 232(sp)
    ld x31, 240(sp)
    addi sp, sp, 304
    sret
"#
);

#[cfg(not(feature = "hosted-dev"))]
unsafe extern "C" {
    fn yarm_riscv64_kthread_switch(prev: *mut KSwitchContext, next: *const KSwitchContext);
    fn yarm_riscv64_kthread_trampoline();
    static yarm_riscv64_kthread_vector: u8;
    static yarm_riscv64_trap_vector: u8;
}

/// Save the running continuation into `prev` and resume `next`.
///
/// # Safety
/// Interrupts masked; `next` was saved by this routine or prepared by [`prepare_first_entry`].
#[cfg(not(feature = "hosted-dev"))]
pub unsafe fn switch(prev: *mut KSwitchContext, next: *const KSwitchContext) {
    // SAFETY: per the caller's contract.
    unsafe { yarm_riscv64_kthread_switch(prev, next) }
}

/// Make `ctx` enter the trampoline on a fresh stack ending at `stack_top`, carrying `pool` and
/// `slot` in `s0`/`s1`.
///
/// # Safety
/// `stack_top` is the exclusive end of a writable stack no hart is using.
#[cfg(not(feature = "hosted-dev"))]
pub unsafe fn prepare_first_entry(
    ctx: &mut KSwitchContext,
    stack_top: usize,
    pool: usize,
    slot: usize,
) {
    *ctx = KSwitchContext::zeroed();
    ctx.ra = yarm_riscv64_kthread_trampoline as *const () as usize;
    ctx.sp = stack_top & !0xF;
    ctx.s[0] = pool;
    ctx.s[1] = slot;
}

#[cfg(not(feature = "hosted-dev"))]
const SSTATUS_SIE: usize = 1 << 1;

#[cfg(not(feature = "hosted-dev"))]
pub fn irqs_enabled() -> bool {
    let sstatus: usize;
    // SAFETY: reads sstatus only.
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack)) };
    sstatus & SSTATUS_SIE != 0
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_enable() {
    // SAFETY: enabling supervisor interrupts on a kernel thread whose vector is installed.
    unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) SSTATUS_SIE, options(nomem, nostack)) };
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_disable_save() -> bool {
    crate::arch::irq_guard::irq_save().interrupts_were_enabled
}

#[cfg(not(feature = "hosted-dev"))]
pub fn irq_restore(was_enabled: bool) {
    crate::arch::irq_guard::irq_restore(crate::arch::riscv64::irq::Riscv64IrqState {
        interrupts_were_enabled: was_enabled,
    });
}

#[cfg(not(feature = "hosted-dev"))]
pub fn current_sp() -> usize {
    let sp: usize;
    // SAFETY: reads the stack pointer only.
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    sp
}

/// A kernel thread is about to run on `cpu`: an idle secondary's timer, parked behind the idle
/// backstop at its idle arrival, gets its own period back (as it does before resuming a task).
#[cfg(not(feature = "hosted-dev"))]
pub fn before_kthread_dispatch(cpu: CpuId) {
    crate::arch::riscv64::timer::unpark_ap_timer(cpu.0 as usize);
}

/// `stvec` names the kernel-thread vector (direct mode).
#[cfg(not(feature = "hosted-dev"))]
pub fn enter_kthread_vectors(_cpu: CpuId) {
    let v = core::ptr::addr_of!(yarm_riscv64_kthread_vector) as usize;
    // SAFETY: interrupts are masked; the vector is 4-byte aligned (direct mode).
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) v, options(nomem, nostack)) };
}

/// `stvec` names the production trap vector again.
#[cfg(not(feature = "hosted-dev"))]
pub fn leave_kthread_vectors(_cpu: CpuId) {
    let v = core::ptr::addr_of!(yarm_riscv64_trap_vector) as usize;
    // SAFETY: interrupts are masked; the production vector is what `enter_user` installs.
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) v, options(nomem, nostack)) };
}

/// The hart this code runs on, from `sscratch`: while a kernel thread runs, `sscratch` still holds
/// this hart's canonical trap-stack top (every return path re-establishes it), which the boot code
/// maps to a logical CPU. `None` when `sscratch` is not a canonical top.
#[cfg(not(feature = "hosted-dev"))]
fn hardware_cpu() -> Option<CpuId> {
    let top: usize;
    // SAFETY: reads sscratch only.
    unsafe { core::arch::asm!("csrr {}, sscratch", out(reg) top, options(nomem, nostack)) };
    crate::arch::riscv64::boot::riscv_cpu_of_canonical_trap_stack_top(top)
}

/// The kernel-thread trap handler. Authenticates the origin, services a supervisor timer,
/// software or external interrupt, records a pending reschedule, and returns to the vector, which
/// restores the interrupted state. Any other trap on a kernel thread is kernel-fatal. Never
/// switches; the timer and software paths never log.
///
/// KTHREAD1-ACCEPTANCE §2 — a supervisor EXTERNAL interrupt is a third entry owner beside the
/// U-origin bridge and the witness build's idle-origin entry, disjoint from both by origin: this
/// vector is installed only while a kernel thread runs. It reads the PLIC claim ONCE, here, after
/// authentication, and hands it to the same settlement the bridge uses
/// (`settle_riscv_external_claim` → `settle_external_interrupt_at_bridge` → the production route
/// and the one completion owner). The claim's reachability is the same walk of the ACTIVE `satp`
/// the other owners use; an unreachable or empty claim settles with nothing completed, exactly as
/// there. Delivery may wake a task; the thread is not switched, a reschedule is left for its next
/// scheduling point.
#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_kthread_trap(frame: *mut crate::arch::riscv64::boot::RiscvTrapFrame) {
    const INTERRUPT: usize = 1 << 63;
    const SUPERVISOR_SOFTWARE: usize = 1;
    const SUPERVISOR_TIMER: usize = 5;
    const SUPERVISOR_EXTERNAL: usize = 9;
    // SAFETY: the vector passes the frame it just saved on this stack.
    let (scause, sepc, stval) =
        unsafe { ((*frame).scause as usize, (*frame).sepc, (*frame).stval) };
    let code = scause & !INTERRUPT;
    let is_irq = scause & INTERRUPT != 0;
    if !is_irq
        || (code != SUPERVISOR_TIMER && code != SUPERVISOR_SOFTWARE && code != SUPERVISOR_EXTERNAL)
    {
        crate::arch::riscv64::boot::early_sbi_marker(format_args!(
            "KTHREAD_FAULT arch=riscv64 scause={:#x} sepc={:#x} stval={:#x} result=fatal",
            scause, sepc, stval
        ));
        crate::arch::riscv64::boot::kthread_fatal_halt();
    }
    let Some(shared) = crate::arch::riscv64::boot::trap_shared_kernel_riscv() else {
        panic!("KTHREAD_IRQ_UNAUTHENTICATED arch=riscv64 reason=no_kernel");
    };
    let pool = &shared.kthreads;
    let Some(hardware) = hardware_cpu() else {
        panic!("KTHREAD_IRQ_UNAUTHENTICATED arch=riscv64 reason=sscratch_not_canonical");
    };
    let (cpu, id) = match pool.authenticate_interrupt(frame as usize, hardware) {
        Ok(ok) => ok,
        Err(r) => panic!(
            "KTHREAD_IRQ_UNAUTHENTICATED arch=riscv64 cpu={} scause={:#x} reason={}",
            hardware.0,
            scause,
            r.marker()
        ),
    };
    let _ = id;
    if code == SUPERVISOR_TIMER {
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_interrupt(cpu, id, 1, sepc, frame as usize, 0);
        let _ = pool.service_timer_interrupt(cpu);
    } else if code == SUPERVISOR_EXTERNAL {
        // THE claim for this origin, read once, here.
        let claim = crate::arch::external_irq_claim::claim_external_interrupt_once();
        #[cfg(feature = "riscv-uart-irq-witness")]
        if claim.in_flight().is_some() {
            crate::arch::riscv64::uart_irq_witness::note_kthread_claim_origin(sepc, id.raw());
        }
        let (result, reason) =
            crate::arch::riscv64::trap::settle_riscv_external_claim(shared, cpu, claim);
        if let Err(err) = result {
            // The user-origin bridge leaves its trap as an error here (a full notification
            // queue); the claim is completed either way. A kernel thread has no trap result to
            // carry it, so the refusal is reported by name and the thread continues.
            crate::yarm_log!(
                "KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED arch=riscv64 cpu={} kthread={:#x} reason={} err={:?}",
                cpu.0,
                id.raw(),
                reason,
                err
            );
        }
        pool.note_device_interrupt(cpu);
    } else {
        let arrival = crate::arch::riscv64::ipi::take_arrival(
            cpu,
            crate::arch::riscv64::ipi::ArrivalOrigin::KernelThread,
        );
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_interrupt(
            cpu,
            id,
            2,
            sepc,
            frame as usize,
            arrival.sources,
        );
        pool.note_resched_interrupt(cpu, arrival.sources != 0);
    }
}
