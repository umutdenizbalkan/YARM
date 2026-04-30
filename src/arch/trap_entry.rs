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
