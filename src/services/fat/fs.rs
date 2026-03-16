use crate::kernel::vfs::VfsBackend;
use crate::kernel::vfs::VfsLiteError;
use crate::services::blkcache::BlockCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatBackend {
    opened: Option<u64>,
    file_len: u64,
    cache: BlockCache,
}

impl Default for FatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FatBackend {
    pub const fn new() -> Self {
        Self {
            opened: None,
            file_len: 1024,
            cache: BlockCache::new(),
        }
    }
}

impl VfsBackend for FatBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if path_ptr == 0 {
            return Err(VfsLiteError::BadFd);
        }
        self.opened = Some(300);
        Ok(300)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        if self.opened == Some(fd) {
            self.opened = None;
            Ok(0)
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.opened != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        let _ = self.cache.get(fd);
        Ok(core::cmp::min(len, self.file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        if self.opened != Some(fd) {
            return Err(VfsLiteError::BadFd);
        }
        self.file_len = self.file_len.saturating_add(len);
        self.cache.put(fd, self.file_len);
        Ok(len)
    }

    fn statx(&mut self, _path_ptr: u64) -> Result<u64, VfsLiteError> {
        Ok(self.file_len)
    }
}
