// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::arch::syscall_abi;
use crate::kernel::task::UserRegisterContext;
use crate::kernel::vm::VirtAddr;

/// Register-width syscall/trap argument frame.
///
/// `usize` is intentionally used here because these fields mirror machine
/// register width at the ABI boundary.
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapFrame {
    pub syscall_num: usize,
    pub args: [usize; syscall_abi::TRAPFRAME_ARG_REGS],
    pub ret0: usize,
    pub ret1: usize,
    pub ret2: usize,
    pub error: usize,
    pub saved_pc: usize,
    pub saved_sp: usize,
}

const _: [(); syscall_abi::TRAPFRAME_ARG_REGS] = [(); 6];
const _: () = assert!(core::mem::offset_of!(TrapFrame, syscall_num) == 0);
const _: () = assert!(core::mem::offset_of!(TrapFrame, args) == core::mem::size_of::<usize>());
const _: () = assert!(
    core::mem::offset_of!(TrapFrame, ret0)
        == core::mem::size_of::<usize>() * (1 + syscall_abi::TRAPFRAME_ARG_REGS)
);
const _: () = assert!(
    core::mem::offset_of!(TrapFrame, saved_pc)
        == core::mem::size_of::<usize>() * (1 + syscall_abi::TRAPFRAME_ARG_REGS + 4)
);
const _: () = assert!(
    core::mem::offset_of!(TrapFrame, saved_sp)
        == core::mem::size_of::<usize>() * (1 + syscall_abi::TRAPFRAME_ARG_REGS + 5)
);

impl TrapFrame {
    pub const fn new(syscall_num: usize, args: [usize; syscall_abi::TRAPFRAME_ARG_REGS]) -> Self {
        Self {
            syscall_num,
            args,
            ret0: 0,
            ret1: 0,
            ret2: 0,
            error: 0,
            saved_pc: 0,
            saved_sp: 0,
        }
    }

    pub const fn zeroed() -> Self {
        Self::new(0, [0; syscall_abi::TRAPFRAME_ARG_REGS])
    }

    pub const fn syscall_num(&self) -> usize {
        self.syscall_num
    }

    pub fn set_syscall_num(&mut self, value: usize) {
        self.syscall_num = value;
    }

    pub const fn arg(&self, index: usize) -> usize {
        self.args[index]
    }

    pub fn set_arg(&mut self, index: usize, value: usize) {
        self.args[index] = value;
    }

    pub const fn ret0(&self) -> usize {
        self.ret0
    }

    pub const fn ret1(&self) -> usize {
        self.ret1
    }

    pub const fn ret2(&self) -> usize {
        self.ret2
    }

    pub fn set_ret2(&mut self, value: usize) {
        self.ret2 = value;
    }

    pub const fn saved_pc(&self) -> usize {
        self.saved_pc
    }

    pub const fn saved_sp(&self) -> usize {
        self.saved_sp
    }

    pub fn set_saved_pc(&mut self, value: usize) {
        self.saved_pc = value;
    }

    pub fn set_saved_sp(&mut self, value: usize) {
        self.saved_sp = value;
    }

    pub fn set_ok(&mut self, ret0: usize, ret1: usize, ret2: usize) {
        self.ret0 = ret0;
        self.ret1 = ret1;
        self.ret2 = ret2;
        self.error = 0;
    }

    /// Marks the frame as failed and clears return registers to avoid exposing
    /// stale data when `error != 0`.
    pub fn set_err(&mut self, code: usize) {
        self.ret0 = 0;
        self.ret1 = 0;
        self.ret2 = 0;
        self.error = code;
    }

    /// Convenience shorthand for `self.error_code().is_some()`.
    pub const fn is_error(&self) -> bool {
        self.error != 0
    }

    pub fn capture_user_context(&self) -> UserRegisterContext {
        UserRegisterContext {
            instruction_ptr: VirtAddr(self.saved_pc as u64),
            stack_ptr: VirtAddr(self.saved_sp as u64),
            arg0: self.args[0],
            arg1: self.args[1],
        }
    }

    pub fn apply_user_context(&mut self, context: UserRegisterContext) {
        self.saved_pc = context.instruction_ptr.0 as usize;
        self.saved_sp = context.stack_ptr.0 as usize;
        self.args[0] = context.arg0;
        self.args[1] = context.arg1;
    }

    pub const fn error_code(&self) -> Option<usize> {
        if self.error == 0 {
            None
        } else {
            Some(self.error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_zeroes_return_fields() {
        let frame = TrapFrame::new(1, [2, 3, 4, 5, 6, 7]);
        assert_eq!(frame.ret0(), 0);
        assert_eq!(frame.ret1(), 0);
        assert_eq!(frame.ret2(), 0);
        assert_eq!(frame.error_code(), None);
        assert!(!frame.is_error());
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.saved_pc(), 0);
        assert_eq!(frame.saved_sp(), 0);
    }

    #[test]
    fn set_ok_clears_error() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_err(7);
        frame.set_ok(11, 22, 33);
        assert_eq!(frame.ret0(), 11);
        assert_eq!(frame.ret1(), 22);
        assert_eq!(frame.ret2(), 33);
        assert_eq!(frame.error_code(), None);
    }

    #[test]
    fn capture_and_apply_user_context_roundtrip() {
        let mut frame = TrapFrame::new(0, [5, 6, 0, 0, 0, 0]);
        frame.set_saved_pc(0x4000);
        frame.set_saved_sp(0x8000);
        let ctx = frame.capture_user_context();
        assert_eq!(ctx.instruction_ptr, VirtAddr(0x4000));
        assert_eq!(ctx.stack_ptr, VirtAddr(0x8000));
        assert_eq!(ctx.arg0, 5);
        assert_eq!(ctx.arg1, 6);

        frame.apply_user_context(UserRegisterContext {
            instruction_ptr: VirtAddr(0x5000),
            stack_ptr: VirtAddr(0x9000),
            arg0: 7,
            arg1: 8,
        });
        assert_eq!(frame.saved_pc(), 0x5000);
        assert_eq!(frame.saved_sp(), 0x9000);
        assert_eq!(frame.arg(0), 7);
        assert_eq!(frame.arg(1), 8);
    }

    #[test]
    fn set_err_clears_returns_and_sets_error_code() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_ok(55, 66, 77);
        frame.set_err(9);
        assert_eq!(frame.ret0(), 0);
        assert_eq!(frame.ret1(), 0);
        assert!(frame.is_error());
        assert_eq!(frame.error_code(), Some(9));
    }
}
