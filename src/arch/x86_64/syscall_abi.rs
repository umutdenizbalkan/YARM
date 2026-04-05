// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// x86_64 syscall/trap ABI shape constants for the prototype kernel ABI.
//
// Fast path: user mode enters kernel through `syscall/sysret`.
// Compatibility fallback: `int 0x80` remains wired via the IDT trap path.
//
// Register mapping (System V AMD64 ABI / YARM convention):
//   syscall_num: rax
//   args[0..5]:  rdi, rsi, rdx, rcx, r8, r9
//   ret0:        rax  (overwritten on return)
//   ret1:        rdx
//   ret2:        rsi  (used for transfer cap return)
//   error:       separate from ret0 to avoid sign-extension ambiguity
//   saved_pc:    rcx (saved by `syscall`) / rip from the interrupt frame
//   saved_sp:    rsp (saved by hardware/entry stub on trap)

pub const TRAPFRAME_ARG_REGS: usize = 6;
/// Inline IPC payload lanes carried directly in syscall argument registers.
///
/// This is intentionally 2 on x86_64 because the generic syscall ABI reserves
/// args 0..=2 for cap/pointer/len and the final argument lane for transfer-cap
/// metadata, leaving exactly args[3] and args[4] as inline payload words.
pub const IPC_REGISTER_WORDS: usize = 2;

pub const PROFILE_IS_PLACEHOLDER: bool = false;
