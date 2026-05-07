// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// AArch64 syscall/trap ABI shape constants for the prototype kernel ABI.

pub const TRAPFRAME_ARG_REGS: usize = 6;

pub const REG_X0: usize = 0;
pub const REG_X1: usize = 1;
pub const REG_X2: usize = 2;
pub const REG_X3: usize = 3;
pub const REG_X4: usize = 4;
pub const REG_X5: usize = 5;
pub const REG_X8: usize = 8;
pub const REG_X18_TLS: usize = 15;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
