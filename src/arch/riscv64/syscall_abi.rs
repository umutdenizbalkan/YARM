// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// RISC-V 64 syscall/trap ABI shape constants.

pub const TRAPFRAME_ARG_REGS: usize = 6;
/// Inline IPC payload lanes exposed by the current cross-architecture ABI.
///
/// RV64 has enough argument registers to grow this in a per-architecture ABI,
/// but the generic kernel/userspace contract currently pins it to the common
/// two-word minimum required by x86_64 compatibility.
pub const IPC_REGISTER_WORDS: usize = 2;

pub const PROFILE_IS_PLACEHOLDER: bool = false;
