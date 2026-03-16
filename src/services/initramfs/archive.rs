use crate::kernel::vfs::{VfsBackend, VfsLiteError};

pub const INITRAMFS_BUSYBOX_PATH_PTR: u64 = 0x494E_4954_4255_5359;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitramfsBackend {
    opened_fd: Option<u64>,
    file_len: u64,
}

impl Default for InitramfsBackend {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl InitramfsBackend {
    pub const fn new(file_len: u64) -> Self {
        Self {
            opened_fd: None,
            file_len,
        }
    }
}

impl VfsBackend for InitramfsBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if path_ptr != INITRAMFS_BUSYBOX_PATH_PTR {
            return Err(VfsLiteError::BadFd);
        }
        self.opened_fd = Some(10);
        Ok(10)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd == Some(fd) {
            self.opened_fd = None;
            Ok(0)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Ok(core::cmp::min(len, self.file_len))
    }

    fn write(&mut self, fd: u64, _len: u64) -> Result<u64, VfsLiteError> {
        if self.opened_fd != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        Err(VfsLiteError::Unsupported)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if path_ptr == INITRAMFS_BUSYBOX_PATH_PTR {
            Ok(self.file_len)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }
}
