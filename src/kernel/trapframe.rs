#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapFrame {
    pub syscall_num: usize,
    pub args: [usize; 6],
    pub ret0: usize,
    pub ret1: usize,
    pub error: usize,
}

impl TrapFrame {
    pub const fn new(syscall_num: usize, args: [usize; 6]) -> Self {
        Self {
            syscall_num,
            args,
            ret0: 0,
            ret1: 0,
            error: 0,
        }
    }

    pub fn set_ok(&mut self, ret0: usize, ret1: usize) {
        self.ret0 = ret0;
        self.ret1 = ret1;
        self.error = 0;
    }

    pub fn set_err(&mut self, code: usize) {
        self.ret0 = 0;
        self.ret1 = 0;
        self.error = code;
    }
}
