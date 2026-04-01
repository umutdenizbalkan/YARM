// x86_64 syscall/trap ABI shape constants for the prototype kernel ABI.
//
// Strategy lock-in (PR-5): user mode enters kernel syscalls via `int 0x80`
// routed through a dedicated IDT gate path (not `syscall/sysret` yet).
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
pub const IPC_REGISTER_WORDS: usize = 2;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
