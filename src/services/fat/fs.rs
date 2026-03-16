use crate::kernel::vfs::{VfsBackend, VfsLiteError};
use crate::services::blkcache::BlockCache;

const MAX_FAT_FILES: usize = 8;
const MAX_OPEN_FDS: usize = 8;
const FAT_CLUSTER_SIZE: u64 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatDirEntry {
    pub path_ptr: u64,
    pub start_cluster: u32,
    pub clusters: u32,
    pub file_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenFd {
    fd: u64,
    file_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatCluster {
    pub id: u32,
    pub next: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatBackend {
    next_fd: u64,
    next_cluster: u32,
    files: [Option<FatDirEntry>; MAX_FAT_FILES],
    open_fds: [Option<OpenFd>; MAX_OPEN_FDS],
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
            next_fd: 300,
            next_cluster: 2,
            files: [None; MAX_FAT_FILES],
            open_fds: [None; MAX_OPEN_FDS],
            cache: BlockCache::new(),
        }
    }

    fn alloc_fd(&mut self, file_idx: usize) -> Result<u64, VfsLiteError> {
        if let Some(slot) = self.open_fds.iter_mut().find(|slot| slot.is_none()) {
            let fd = self.next_fd;
            self.next_fd = self.next_fd.saturating_add(1);
            *slot = Some(OpenFd { fd, file_idx });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn find_file_idx(&self, path_ptr: u64) -> Option<usize> {
        self.files
            .iter()
            .position(|slot| slot.map(|e| e.path_ptr == path_ptr).unwrap_or(false))
    }

    fn alloc_file(&mut self, path_ptr: u64) -> Result<usize, VfsLiteError> {
        if let Some(idx) = self.find_file_idx(path_ptr) {
            return Ok(idx);
        }
        if let Some((idx, slot)) = self
            .files
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.is_none())
        {
            let start = self.next_cluster;
            self.next_cluster = self.next_cluster.saturating_add(1);
            *slot = Some(FatDirEntry {
                path_ptr,
                start_cluster: start,
                clusters: 1,
                file_len: 0,
            });
            Ok(idx)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn open_fd_lookup(&self, fd: u64) -> Option<OpenFd> {
        self.open_fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .copied()
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .open_fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn grow_clusters_if_needed(entry: &mut FatDirEntry) {
        let needed =
            ((entry.file_len + FAT_CLUSTER_SIZE.saturating_sub(1)) / FAT_CLUSTER_SIZE) as u32;
        if needed > entry.clusters {
            entry.clusters = needed;
        }
    }

    pub fn cluster_chain_head_for_path(&self, path_ptr: u64) -> Option<FatCluster> {
        let idx = self.find_file_idx(path_ptr)?;
        let entry = self.files[idx]?;
        Some(FatCluster {
            id: entry.start_cluster,
            next: if entry.clusters > 1 {
                Some(entry.start_cluster.saturating_add(1))
            } else {
                None
            },
        })
    }
}

impl VfsBackend for FatBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if path_ptr == 0 {
            return Err(VfsLiteError::BadFd);
        }
        let file_idx = self.alloc_file(path_ptr)?;
        self.alloc_fd(file_idx)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let opened = self.open_fd_lookup(fd).ok_or(VfsLiteError::BadFd)?;
        let file = self.files[opened.file_idx].ok_or(VfsLiteError::BadFd)?;
        let _ = self.cache.get(fd);
        Ok(core::cmp::min(len, file.file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let opened = self.open_fd_lookup(fd).ok_or(VfsLiteError::BadFd)?;
        let Some(mut file) = self.files[opened.file_idx] else {
            return Err(VfsLiteError::BadFd);
        };
        file.file_len = file.file_len.saturating_add(len);
        Self::grow_clusters_if_needed(&mut file);
        self.files[opened.file_idx] = Some(file);
        self.cache.put(fd, file.file_len);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        let idx = self.find_file_idx(path_ptr).ok_or(VfsLiteError::BadFd)?;
        Ok(self.files[idx].ok_or(VfsLiteError::BadFd)?.file_len)
    }
}
