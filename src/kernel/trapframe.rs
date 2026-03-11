/// Register-width syscall/trap argument frame.
///
/// `usize` is intentionally used here because these fields mirror machine
/// register width at the ABI boundary.
#[repr(C)]
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

    /// Marks the frame as failed and clears return registers to avoid exposing
    /// stale data when `error != 0`.
    pub fn set_err(&mut self, code: usize) {
        self.ret0 = 0;
        self.ret1 = 0;
        self.error = code;
    }

    pub const fn is_error(&self) -> bool {
        self.error != 0
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
        assert_eq!(frame.ret0, 0);
        assert_eq!(frame.ret1, 0);
        assert_eq!(frame.error, 0);
        assert!(!frame.is_error());
        assert_eq!(frame.error_code(), None);
    }

    #[test]
    fn set_ok_clears_error() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_err(7);
        frame.set_ok(11, 22);
        assert_eq!(frame.ret0, 11);
        assert_eq!(frame.ret1, 22);
        assert_eq!(frame.error, 0);
    }

    #[test]
    fn set_err_clears_returns_and_sets_error_code() {
        let mut frame = TrapFrame::new(0, [0; 6]);
        frame.set_ok(55, 66);
        frame.set_err(9);
        assert_eq!(frame.ret0, 0);
        assert_eq!(frame.ret1, 0);
        assert!(frame.is_error());
        assert_eq!(frame.error_code(), Some(9));
    }
}
