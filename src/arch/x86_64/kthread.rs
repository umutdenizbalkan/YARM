// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — x86_64 kernel-thread switch, first entry and interrupt vectors.
//!
//! Voluntary switch state (`KSwitchContext`): `rsp` (pointing at the return address) and the SysV
//! callee-saved `rbx rbp r12 r13 r14 r15`. Interrupted state: the CPU's five-word frame (in long
//! mode a ring-0 interrupt always pushes `ss:rsp`, `rflags`, `cs:rip`, on the CURRENT stack — the
//! kernel thread's — since these gates use no IST) plus all fifteen general registers pushed by
//! the stub. `RFLAGS` is restored by `iretq`; `GS.base` is the kernel per-CPU record in every ring
//! (no `swapgs` anywhere, Stage 189C4), so a kernel-to-kernel switch has no per-CPU base to move.
//! No FP/SIMD state: the kernel uses none (KERNEL-NOFP1), and the interrupted user's FXSAVE home is
//! owned by the user trap path, which a kernel thread never enters.

/// Voluntary switch state. Offsets are used by the switch below.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KSwitchContext {
    rsp: usize, // 0
    rbx: usize, // 8
    rbp: usize, // 16
    r12: usize, // 24
    r13: usize, // 32
    r14: usize, // 40
    r15: usize, // 48
}

impl KSwitchContext {
    pub const fn zeroed() -> Self {
        Self {
            rsp: 0,
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
        }
    }
}

// The switch saves into `rdi` and loads from `rsi`, then `ret`s on the incoming stack: to the
// incoming thread's own call site of this routine, or — on a first entry — to the trampoline.
// The trampoline is entered with `rsp` 16-byte aligned, so its `call` gives the callee the SysV
// entry alignment (`rsp + 8` aligned).
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_64_kthread_switch
    .type yarm_x86_64_kthread_switch, @function
yarm_x86_64_kthread_switch:
    mov [rdi + 0], rsp
    mov [rdi + 8], rbx
    mov [rdi + 16], rbp
    mov [rdi + 24], r12
    mov [rdi + 32], r13
    mov [rdi + 40], r14
    mov [rdi + 48], r15
    mov rsp, [rsi + 0]
    mov rbx, [rsi + 8]
    mov rbp, [rsi + 16]
    mov r12, [rsi + 24]
    mov r13, [rsi + 32]
    mov r14, [rsi + 40]
    mov r15, [rsi + 48]
    ret

    .global yarm_x86_64_kthread_trampoline
    .type yarm_x86_64_kthread_trampoline, @function
yarm_x86_64_kthread_trampoline:
    mov rdi, r12
    mov rsi, r13
    call yarm_kthread_first_entry
    ud2
"#
);

unsafe extern "C" {
    fn yarm_x86_64_kthread_switch(prev: *mut KSwitchContext, next: *const KSwitchContext);
    fn yarm_x86_64_kthread_trampoline();
}

/// Save the running continuation into `prev` and resume `next`.
///
/// # Safety
/// Interrupts masked; `next` was saved by this routine or prepared by [`prepare_first_entry`], on
/// a stack that is not live anywhere else.
pub unsafe fn switch(prev: *mut KSwitchContext, next: *const KSwitchContext) {
    // SAFETY: per the caller's contract.
    unsafe { yarm_x86_64_kthread_switch(prev, next) }
}

/// Make `ctx` enter the trampoline on a fresh stack ending at `stack_top`, carrying `pool` and
/// `slot` in `r12`/`r13`.
///
/// # Safety
/// `stack_top` is the exclusive end of a writable stack no CPU is using.
pub unsafe fn prepare_first_entry(
    ctx: &mut KSwitchContext,
    stack_top: usize,
    pool: usize,
    slot: usize,
) {
    let top = stack_top & !0xF;
    let ret_slot = top - 8;
    // SAFETY: inside the caller's stack.
    unsafe {
        core::ptr::write(
            ret_slot as *mut usize,
            yarm_x86_64_kthread_trampoline as *const () as usize,
        )
    };
    *ctx = KSwitchContext::zeroed();
    ctx.rsp = ret_slot;
    ctx.r12 = pool;
    ctx.r13 = slot;
}

#[cfg(not(feature = "hosted-dev"))]
pub use bare::*;

#[cfg(not(feature = "hosted-dev"))]
mod bare {
    use crate::kernel::scheduler::CpuId;

    const RFLAGS_IF: usize = 1 << 9;

    pub fn irqs_enabled() -> bool {
        let flags: usize;
        // SAFETY: reads RFLAGS only.
        unsafe {
            core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags))
        };
        flags & RFLAGS_IF != 0
    }

    pub fn irq_enable() {
        // SAFETY: enabling interrupts on a kernel thread whose vectors are installed.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }

    pub fn irq_disable_save() -> bool {
        crate::arch::irq_guard::irq_save().interrupts_were_enabled
    }

    pub fn irq_restore(was_enabled: bool) {
        crate::arch::irq_guard::irq_restore(crate::arch::x86_64::irq::X86IrqState {
            interrupts_were_enabled: was_enabled,
        });
    }

    pub fn current_sp() -> usize {
        let sp: usize;
        // SAFETY: reads the stack pointer only.
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags))
        };
        sp
    }

    /// A kernel thread is about to run on `cpu`. x86_64 parks no idle timer: nothing to restore.
    pub fn before_kthread_dispatch(_cpu: CpuId) {}

    /// Point this CPU's IDTR at the kernel-thread IDT: the production gates, except that the timer
    /// (0x20) and the reschedule IPI (0xF1) enter the kernel-thread stubs below.
    pub fn enter_kthread_vectors(_cpu: CpuId) {
        crate::arch::x86_64::descriptor_tables::load_kthread_idt();
    }

    /// Point this CPU's IDTR back at the production IDT.
    pub fn leave_kthread_vectors(_cpu: CpuId) {
        crate::arch::x86_64::descriptor_tables::load_production_idt();
    }

    // The kernel-thread interrupt stubs. Entered through an interrupt gate (IF cleared) from
    // ring 0 on the kernel thread's stack: the CPU aligned `rsp` to 16 and pushed five words, so
    // fifteen pushes leave it aligned for the `call`. The timer stub returns with `iretq`. The
    // reschedule stub, after its note, restores the registers and continues in the production 0xF1
    // handler, which owns that vector's remaining obligations (wake accounting, this CPU's TLB
    // mailbox acknowledgement, the one EOI) and `iretq`s to the interrupted instruction.
    core::arch::global_asm!(
        r#"
        .section .text, "ax", @progbits
        .macro YARM_KT_PUSH
        push rax
        push rcx
        push rdx
        push rbx
        push rbp
        push rsi
        push rdi
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15
        .endm
        .macro YARM_KT_POP
        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rdi
        pop rsi
        pop rbp
        pop rbx
        pop rdx
        pop rcx
        pop rax
        .endm

        .global yarm_x86_64_kthread_timer_stub
        .type yarm_x86_64_kthread_timer_stub, @function
    yarm_x86_64_kthread_timer_stub:
        YARM_KT_PUSH
        mov rdi, rsp
        mov esi, 0x20
        call yarm_x86_64_kthread_interrupt
        YARM_KT_POP
        iretq

        .global yarm_x86_64_kthread_resched_stub
        .type yarm_x86_64_kthread_resched_stub, @function
    yarm_x86_64_kthread_resched_stub:
        YARM_KT_PUSH
        mov rdi, rsp
        mov esi, 0xF1
        call yarm_x86_64_kthread_interrupt
        YARM_KT_POP
        jmp yarm_ap_remote_wake_stub
    "#
    );

    unsafe extern "C" {
        pub(crate) fn yarm_x86_64_kthread_timer_stub();
        pub(crate) fn yarm_x86_64_kthread_resched_stub();
    }

    /// The kernel-thread interrupt bridge. `frame` is the saved register block on the interrupted
    /// stack. Authenticates the origin, services the obligations, records a pending reschedule,
    /// and returns to the stub. Never logs, never switches.
    #[unsafe(no_mangle)]
    extern "C" fn yarm_x86_64_kthread_interrupt(frame: usize, vector: u64) {
        let Some(shared) = crate::arch::x86_64::descriptor_tables::trap_shared_kernel() else {
            panic!(
                "KTHREAD_IRQ_UNAUTHENTICATED arch=x86_64 vector={:#x} reason=no_kernel",
                vector
            );
        };
        let pool = &shared.kthreads;
        let hardware = crate::arch::x86_64::descriptor_tables::current_cpu_id();
        let (cpu, id) = match pool.authenticate_interrupt(frame, hardware) {
            Ok(ok) => ok,
            Err(r) => panic!(
                "KTHREAD_IRQ_UNAUTHENTICATED arch=x86_64 cpu={} vector={:#x} reason={}",
                hardware.0,
                vector,
                r.marker()
            ),
        };
        let _ = id;
        // The interrupted RIP is the first word of the CPU frame, above the 15 pushed registers.
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_interrupt(
            cpu,
            id,
            if vector == 0x20 { 1 } else { 2 },
            // SAFETY: the stub pushed 15 registers above the CPU's interrupt frame.
            unsafe { *((frame + 15 * 8) as *const u64) },
            frame,
            0,
        );
        match vector {
            0x20 => {
                let _ = pool.service_timer_interrupt(cpu);
            }
            _ => pool.note_resched_interrupt(cpu, true),
        }
    }
}
