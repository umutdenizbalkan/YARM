// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::trap::TrapEvent;
use crate::kernel::boot::{KernelState, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::trapframe::TrapFrame;

#[cfg(target_arch = "riscv64")]
pub type ArchTrapContext = super::riscv64::trap::Riscv64TrapContext;
#[cfg(target_arch = "riscv64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::riscv64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "riscv64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::riscv64::trap::handle_trap_entry(kernel, cpu, context, frame)
}

#[cfg(target_arch = "x86_64")]
pub type ArchTrapContext = super::x86_64::trap::X86TrapContext;
#[cfg(target_arch = "x86_64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::x86_64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "x86_64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::x86_64::trap::handle_trap_entry(kernel, cpu, context, frame)
}

#[cfg(target_arch = "aarch64")]
pub type ArchTrapContext = super::aarch64::trap::Aarch64TrapContext;
#[cfg(target_arch = "aarch64")]
pub fn decode_trap_context(context: ArchTrapContext) -> TrapEvent {
    super::aarch64::trap::decode_trap_context(context)
}
#[cfg(target_arch = "aarch64")]
pub fn handle_trap_entry(
    kernel: &mut KernelState,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    super::aarch64::trap::handle_trap_entry(kernel, cpu, context, frame)
}


pub fn handle_trap_entry_shared(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    // AArch64 recv-timeout split-read: pre-read the scheduler tick under the
    // lighter scheduler lock BEFORE acquiring the global SharedKernel lock.
    // The deadline is stored per-CPU so the recv-timeout syscall handler can
    // call ipc_recv_until_deadline(deadline) instead of re-reading the tick
    // inside the global lock.  All other archs and syscalls take the normal path.
    #[cfg(target_arch = "aarch64")]
    {
        const ESR_EC_SVC64: u32 = 0x15;
        let esr_ec = (context.esr_el1 >> 26) & 0x3f;
        if esr_ec == ESR_EC_SVC64 {
            let (nr, timeout_ticks) = {
                frame
                    .as_deref()
                    .map(|f| {
                        (
                            f.user_gpr(crate::arch::aarch64::syscall_abi::REG_X8),
                            f.user_gpr(crate::arch::aarch64::syscall_abi::REG_X3) as u64,
                        )
                    })
                    .unwrap_or((usize::MAX, 0))
            };
            if nr == crate::kernel::syscall::SYSCALL_IPC_RECV_TIMEOUT_NR && timeout_ticks != 0 {
                crate::yarm_log!("YARM_LOCK_SPLIT_RECV_TIMEOUT path=shared_bridge");
                let now = shared.scheduler_tick_now_split_read();
                let deadline = now.wrapping_add(timeout_ticks);
                let cpu_idx = cpu.0 as usize;
                if cpu_idx < crate::kernel::scheduler::MAX_CPUS && deadline != 0 {
                    crate::kernel::scheduler::SPLIT_RECV_TIMEOUT_DEADLINE[cpu_idx]
                        .store(deadline, core::sync::atomic::Ordering::Release);
                }
            }
        }
    }
    let result = shared
        .with_cpu(cpu, |kernel| handle_trap_entry(kernel, cpu, context, frame))
        .map_err(|err| TrapHandleError::Syscall(err.into()))?;
    result
}

pub fn dispatch_trap_entry_with_shared_kernel(
    shared: &crate::runtime::SharedKernel,
    cpu: CpuId,
    context: ArchTrapContext,
    frame: Option<&mut TrapFrame>,
) -> Result<(), TrapHandleError> {
    handle_trap_entry_shared(shared, cpu, context, frame)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "x86_64",
    target_arch = "aarch64"
)))]
compile_error!("unsupported target_arch for arch::trap_entry");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_arch_decoder_is_callable() {
        let _ = decode_trap_context;
        let _ = handle_trap_entry;
    }
}
