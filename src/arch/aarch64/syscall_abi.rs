// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// AArch64 syscall/trap ABI shape constants for the prototype kernel ABI.

pub const TRAPFRAME_ARG_REGS: usize = 6;
/// Inline IPC payload lanes exposed by the current cross-architecture ABI.
///
/// AArch64 can support more register arguments, but YARM currently keeps the
/// same two-word inline payload floor as x86_64 for portable syscall semantics.
pub const IPC_REGISTER_WORDS: usize = 2;

pub const REG_X0: usize = 0;
pub const REG_X1: usize = 1;
pub const REG_X2: usize = 2;
pub const REG_X3: usize = 3;
pub const REG_X4: usize = 4;
pub const REG_X5: usize = 5;
pub const REG_X8: usize = 8;
pub const REG_X18_TLS: usize = 15;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
