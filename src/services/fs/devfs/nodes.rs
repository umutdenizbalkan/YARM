use crate::kernel::vfs::{VfsBackend, VfsError};

pub const DEV_CONSOLE_PATH_PTR: u64 = 0x434F_4E53_4F4C_4500;
pub const DEV_NULL_PATH_PTR: u64 = 0x4445_564E_554C_4C00;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevFsBackend {
    open_console_fd: Option<u64>,
    open_null_fd: Option<u64>,
}

impl VfsBackend for DevFsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if path_ptr == DEV_CONSOLE_PATH_PTR {
            self.open_console_fd = Some(3);
            return Ok(3);
        }
        if path_ptr == DEV_NULL_PATH_PTR {
            self.open_null_fd = Some(4);
            return Ok(4);
        }
        Err(VfsError::BadFd)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        if self.open_console_fd == Some(fd) {
            self.open_console_fd = None;
            return Ok(0);
        }
        if self.open_null_fd == Some(fd) {
            self.open_null_fd = None;
            return Ok(0);
        }
        Err(VfsError::BadFd)
    }

    fn read(&mut self, fd: u64, _len: u64) -> Result<u64, VfsError> {
        if self.open_console_fd == Some(fd) || self.open_null_fd == Some(fd) {
            return Ok(0);
        }
        Err(VfsError::BadFd)
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        if self.open_console_fd == Some(fd) || self.open_null_fd == Some(fd) {
            return Ok(len);
        }
        Err(VfsError::BadFd)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if path_ptr == DEV_CONSOLE_PATH_PTR || path_ptr == DEV_NULL_PATH_PTR {
            Ok(0)
        } else {
            Err(VfsError::BadFd)
        }
    }
}
