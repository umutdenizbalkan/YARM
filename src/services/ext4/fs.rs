use crate::kernel::vfs_lite::{VfsBackend, VfsLiteError};
use crate::services::common::fs::{
    FdRecord, MAX_SERVICE_FDS, MAX_SERVICE_INODES, ServiceFsBackend,
};

use super::dir::find_inode_index;
use super::file::checked_append;
use super::inode::Ext4Inode;

#[derive(Debug)]
pub struct Ext4Backend {
    next_fd: u64,
    fds: [Option<FdRecord>; MAX_SERVICE_FDS],
    inodes: [Option<Ext4Inode>; MAX_SERVICE_INODES],
    max_file_len: u64,
    journal_seq: u64,
}

impl Default for Ext4Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext4Backend {
    pub const fn new() -> Self {
        Self {
            next_fd: 200,
            fds: [None; MAX_SERVICE_FDS],
            inodes: [None; MAX_SERVICE_INODES],
            max_file_len: 16 * 1024 * 1024,
            journal_seq: 0,
        }
    }

    pub const fn journal_seq(&self) -> u64 {
        self.journal_seq
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsLiteError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdRecord { fd, inode });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn open_inode(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        if let Some(inode) = self
            .inodes
            .iter()
            .flatten()
            .find(|inode| inode.path_ptr == path_ptr)
            .map(|inode| inode.path_ptr)
        {
            return self.alloc_fd(inode);
        }
        if let Some(slot) = self.inodes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(Ext4Inode {
                path_ptr,
                file_len: 0,
            });
            return self.alloc_fd(path_ptr);
        }
        Err(VfsLiteError::NoFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    fn inode_for_fd(&self, fd: u64) -> Option<u64> {
        self.fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .map(|entry| entry.inode)
    }
}

impl ServiceFsBackend for Ext4Backend {
    fn name(&self) -> &'static str {
        "ext4"
    }

    fn validate(&self) -> Result<(), VfsLiteError> {
        Ok(())
    }
}

impl VfsBackend for Ext4Backend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        self.open_inode(path_ptr)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsLiteError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsLiteError::BadFd)?;
        let file_len = self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len;
        Ok(core::cmp::min(len, file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsLiteError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsLiteError::BadFd)?;
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsLiteError::BadFd)?;
        let Some(mut inode_slot) = self.inodes[idx] else {
            return Err(VfsLiteError::BadFd);
        };
        inode_slot.file_len = checked_append(inode_slot.file_len, len, self.max_file_len)?;
        self.inodes[idx] = Some(inode_slot);
        self.journal_seq = self.journal_seq.saturating_add(1);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsLiteError> {
        let idx = find_inode_index(&self.inodes, path_ptr).ok_or(VfsLiteError::BadFd)?;
        Ok(self.inodes[idx].ok_or(VfsLiteError::BadFd)?.file_len)
    }
}
