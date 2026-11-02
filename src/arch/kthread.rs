// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — the architecture half of kernel threads, one interface for every port.
//!
//! * `KSwitchContext` / `switch` — the voluntary kernel-to-kernel switch: callee-saved registers,
//!   the stack pointer and the return address, saved into the outgoing context and loaded from the
//!   incoming one. It is distinct from the interrupted state an interrupt entry saves (every
//!   general register plus the interrupted PC and status, on the interrupted stack) and from a user
//!   task's `UserRegisterContext`. No FP/SIMD register is part of either: kernel code uses none
//!   (KERNEL-NOFP1).
//! * `prepare_first_entry` — a fresh stack whose first switch lands in the port's trampoline, which
//!   calls `kernel::kthread::yarm_kthread_first_entry(pool, slot)`.
//! * `irqs_enabled` / `irq_enable` / `irq_disable_save` / `irq_restore` — the CPU's interrupt mask.
//! * `enter_kthread_vectors` / `leave_kthread_vectors` — while a kernel thread runs, the CPU's
//!   vector base names the kernel-thread vectors (RISC-V `stvec`, x86_64 IDTR, AArch64
//!   `VBAR_EL1`), whose interrupt entries save the interrupted state on the thread's own stack.
//! * `current_sp` — the executing stack pointer.

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub use super::riscv64::kthread::*;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub use super::x86_64::kthread::*;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub use super::aarch64::kthread::*;

#[cfg(feature = "hosted-dev")]
pub use hosted::*;

/// The hosted model: the real x86_64 switch (the host is x86_64 SysV, the same callee-saved set
/// as the kernel port) and a per-host-thread interrupt mask, so the hosted suite runs genuine
/// kernel-thread bodies on their own stacks through the production scheduling owners. There are
/// no interrupts and no vectors.
#[cfg(feature = "hosted-dev")]
mod hosted {
    use crate::kernel::scheduler::CpuId;

    #[cfg(target_arch = "x86_64")]
    pub use crate::arch::x86_64::kthread::{KSwitchContext, prepare_first_entry, switch};

    crate::std::thread_local! {
        // A dispatcher runs masked (it is trap context in production).
        static IRQ_ENABLED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    }

    pub fn irqs_enabled() -> bool {
        IRQ_ENABLED.get()
    }

    pub fn irq_enable() {
        IRQ_ENABLED.set(true);
    }

    pub fn irq_disable_save() -> bool {
        IRQ_ENABLED.replace(false)
    }

    pub fn irq_restore(was_enabled: bool) {
        if was_enabled {
            irq_enable();
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn current_sp() -> usize {
        let sp: usize;
        // SAFETY: reads the stack pointer only.
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags))
        };
        sp
    }

    pub fn before_kthread_dispatch(_cpu: CpuId) {}

    pub fn enter_kthread_vectors(_cpu: CpuId) {}

    pub fn leave_kthread_vectors(_cpu: CpuId) {}

    // A non-x86_64 host has no switch: kernel threads can be created and observed, never run.
    #[cfg(not(target_arch = "x86_64"))]
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct KSwitchContext([usize; 8]);

    #[cfg(not(target_arch = "x86_64"))]
    impl KSwitchContext {
        pub const fn zeroed() -> Self {
            Self([0; 8])
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub unsafe fn prepare_first_entry(
        _c: &mut KSwitchContext,
        _top: usize,
        _pool: usize,
        _slot: usize,
    ) {
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub unsafe fn switch(_prev: *mut KSwitchContext, _next: *const KSwitchContext) {
        panic!("kernel-thread switch is not modelled on this host");
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub fn current_sp() -> usize {
        0
    }
}
