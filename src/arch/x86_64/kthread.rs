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

/// Make `ctx` enter the trampoline on a fresh stack ending at `stack_top` (the address the thread
/// runs at), carrying `pool` and `slot` in `r12`/`r13`. The return slot is written through
/// `write_top`, the same stack's end in a view the creating context can reach (KTHREAD-CLOSURE1
/// §C: the run view may be a window mapped only for kernel threads).
///
/// # Safety
/// `write_top` is the exclusive end of a writable stack no CPU is using, and `stack_top` names the
/// same bytes for the thread.
pub unsafe fn prepare_first_entry(
    ctx: &mut KSwitchContext,
    stack_top: usize,
    write_top: usize,
    pool: usize,
    slot: usize,
) {
    let top = stack_top & !0xF;
    let ret_slot = top - 8;
    let write_slot = (write_top & !0xF) - 8;
    // SAFETY: inside the caller's stack.
    unsafe {
        core::ptr::write(
            write_slot as *mut usize,
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

    /// KTHREAD-CLOSURE1 §C: the protected kernel-thread stack window, once installed.
    pub fn kstack_window() -> Option<usize> {
        crate::arch::x86_64::kstack_window::window()
    }

    /// KTHREAD-CLOSURE1 §C: kernel threads run on the kernel root the dispatcher already entered.
    pub fn enter_kthread_root(_cpu: CpuId) {}

    /// KTHREAD-CLOSURE1 §C: nothing to leave.
    pub fn leave_kthread_root(_cpu: CpuId) {}

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

        // KTHREAD1-ACCEPTANCE §2 — one stub per device vector (0x21 up to the end of the external
        // line range); each names its vector and continues in the device bridge below, then
        // `iretq`s to the interrupted instruction.
        .macro YARM_KT_DEV_STUB vec
        .global yarm_x86_64_kthread_dev_stub_\vec
        .type yarm_x86_64_kthread_dev_stub_\vec, @function
    yarm_x86_64_kthread_dev_stub_\vec:
        YARM_KT_PUSH
        mov rdi, rsp
        mov esi, \vec
        call yarm_x86_64_kthread_device_interrupt
        YARM_KT_POP
        iretq
        .endm
        .irp v, 33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95
        YARM_KT_DEV_STUB \v
        .endr

        .section .rodata, "a", @progbits
        .balign 8
        .global yarm_x86_64_kthread_dev_stubs
    yarm_x86_64_kthread_dev_stubs:
        .irp v, 33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95
        .quad yarm_x86_64_kthread_dev_stub_\v
        .endr
        .section .text, "ax", @progbits

        // KTHREAD-CLOSURE1 §C — #PF and #DF while a kernel thread runs. Both gates use an IST
        // stack (the production slots: 3 for #PF, 2 for #DF), so the CPU never pushes onto the
        // faulting stack. The CPU pushed an error code: the frame is err, rip, cs, rflags, rsp,
        // ss. Neither returns.
        .global yarm_x86_64_kthread_pf_stub
        .type yarm_x86_64_kthread_pf_stub, @function
    yarm_x86_64_kthread_pf_stub:
        mov rdi, rsp
        mov rsi, cr2
        mov edx, 14
        and rsp, -16
        call yarm_x86_64_kthread_fault
        ud2

        .global yarm_x86_64_kthread_df_stub
        .type yarm_x86_64_kthread_df_stub, @function
    yarm_x86_64_kthread_df_stub:
        mov rdi, rsp
        mov rsi, cr2
        mov edx, 8
        and rsp, -16
        call yarm_x86_64_kthread_fault
        ud2

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
        pub(crate) fn yarm_x86_64_kthread_pf_stub();
        pub(crate) fn yarm_x86_64_kthread_df_stub();
        pub(crate) fn yarm_x86_64_kthread_timer_stub();
        pub(crate) fn yarm_x86_64_kthread_resched_stub();
        /// The device stubs' addresses, for vectors [`KTHREAD_DEVICE_VECTOR_FIRST`] onward.
        pub(crate) static yarm_x86_64_kthread_dev_stubs: [u64; KTHREAD_DEVICE_VECTORS];
    }

    /// KTHREAD1-ACCEPTANCE §2 — the device vectors the kernel-thread IDT routes to the device
    /// bridge: every external line above the timer (0x21 .. 0x20 + `MAX_IRQ_LINES`).
    pub(crate) const KTHREAD_DEVICE_VECTOR_FIRST: usize = 0x21;
    pub(crate) const KTHREAD_DEVICE_VECTORS: usize =
        crate::arch::platform_constants::MAX_IRQ_LINES - 1;

    /// KTHREAD1-ACCEPTANCE §2 — the kernel-thread DEVICE bridge. A device vector taken while a
    /// kernel thread runs used to reach the production gate, whose dispatch saw a nested trap
    /// (the thread runs inside the trap that dispatched it) and halted. Here the origin is
    /// authenticated as for the timer, the vector is delivered through the production route by
    /// `settle_external_interrupt_at_bridge`, whose completion step is the x86 single step: the
    /// ONE LAPIC EOI. Delivery may wake a task; the thread is not switched, a reschedule is left
    /// for its next scheduling point.
    #[unsafe(no_mangle)]
    extern "C" fn yarm_x86_64_kthread_device_interrupt(frame: usize, vector: u64) {
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
        // SAFETY: the stub pushed 15 registers above the CPU's interrupt frame (RIP first).
        let rip = unsafe { *((frame + 15 * 8) as *const u64) };
        #[cfg(feature = "x86_64-uart-irq-witness")]
        crate::arch::x86_64::uart_irq_witness::note_kthread_entry_and_drain(
            vector,
            rip,
            cpu,
            id.raw(),
        );
        #[cfg(not(feature = "x86_64-uart-irq-witness"))]
        let _ = rip;
        let line = (vector - 0x20) as u16;
        let settled = crate::kernel::syscall_split::settle_external_interrupt_at_bridge(
            shared,
            cpu,
            u32::from(line),
            crate::arch::hal_adapters::InterruptCompletion::ArchSingleStep { line },
        );
        #[cfg(feature = "x86_64-uart-irq-witness")]
        crate::arch::x86_64::uart_irq_witness::note_after_dispatch(vector);
        if let Some(Err(err)) = settled {
            // The ring-3 bridge leaves its trap as an error here (a full notification queue); the
            // EOI is written either way. A kernel thread has no trap result to carry it.
            crate::yarm_log!(
                "KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED arch=x86_64 cpu={} kthread={:#x} vector={:#x} err={:?}",
                cpu.0,
                id.raw(),
                vector,
                err
            );
        }
        pool.note_device_interrupt(cpu);
    }

    /// KTHREAD-CLOSURE1 §C — a #PF or #DF taken while a kernel thread ran, on its IST stack.
    /// Reported raw (the faulting thread may hold any lock), then this CPU halts; a fault in a
    /// guard gap of the window is that slot's stack overflow.
    #[unsafe(no_mangle)]
    extern "C" fn yarm_x86_64_kthread_fault(frame: *const u64, cr2: u64, vector: u64) -> ! {
        // SAFETY: the stub passed the CPU's frame: err, rip, cs, rflags, rsp, ss.
        let (err, rip, rsp) = unsafe { (*frame, *frame.add(1), *frame.add(4)) };
        let pool =
            crate::arch::x86_64::descriptor_tables::trap_shared_kernel().map(|s| &s.kthreads);
        let cpu = crate::arch::x86_64::descriptor_tables::current_cpu_id().0 as usize;
        // A #DF carries no fault address: its stack pointer locates the overflow.
        let addr = if vector == 14 { cr2 } else { rsp };
        crate::kernel::kthread::report_kernel_thread_fault(
            pool,
            "x86_64",
            cpu,
            (vector << 32) | (err & 0xFFFF_FFFF),
            addr as usize,
            rsp as usize,
            rip as usize,
            crate::arch::x86_64::console::write_line,
        );
        loop {
            // SAFETY: halting this CPU with interrupts masked.
            unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) };
        }
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
