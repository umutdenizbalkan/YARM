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
/// The `user_gprs` lane holding the AArch64 platform register **x18**, which the kernel
/// restores the task's TLS base into on every return to EL0.
///
/// 199E-A64CALL: this was `15`. The vector glue fills `user_gprs` with a straight
/// `for idx in 0..31 { lane[idx] = frame.gprs[idx] }` over `x0..x30`, so lane N *is* xN — a
/// lane of 15 addressed **x15**, not x18. Every AArch64 return to EL0 therefore wrote
/// `tls.unwrap_or(0)` over the user's live x15 (and, TLS being unset for these tasks, that
/// meant zeroing it), while x18 — the register this constant is named for and the one the TLS
/// design targets — was never written at all.
///
/// It stayed silent for as long as no userspace code happened to keep a live value in x15
/// across a syscall. init's `spawn_v5_cap` does: LLVM parks a pointer in x15 across the `svc`,
/// and the first instruction after the call, `ldr x8, [x15]`, then faulted reading 0x0.
///
/// Writing lane 18 is only sound because the AArch64 USERSPACE target now builds with
/// `+reserve-x18`, so no user code keeps a live value in the platform register the kernel owns.
pub const REG_X18_TLS: usize = 18;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
