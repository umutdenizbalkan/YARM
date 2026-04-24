// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};
use super::super::common::fs::{FdRecord, MAX_SERVICE_FDS, MAX_SERVICE_INODES, ServiceFsBackend};

use super::dir::find_inode_index;
use super::file::checked_append;
use super::inode::Ext4Inode;
use crate::blkcache::BlockCache;

/// Compatibility-only legacy path identifier; prefer `EXT4_DEMO_PATH`.
pub const EXT4_DEMO_PATH_PTR: u64 = 0x4040;
pub const EXT4_DEMO_PATH: &[u8] = b"/ext4/file.bin";
/// Compatibility-only legacy path identifier; prefer `EXT4_SERVICE_PATH`.
pub const EXT4_SERVICE_PATH_PTR: u64 = 0x2020;
pub const EXT4_SERVICE_PATH: &[u8] = b"/ext4/service.bin";
/// Compatibility-only legacy path identifier; prefer `EXT4_OVERSIZE_PATH`.
pub const EXT4_OVERSIZE_PATH_PTR: u64 = 0x3030;
pub const EXT4_OVERSIZE_PATH: &[u8] = b"/ext4/oversize.bin";

const EXT4_INLINE_PATH_MAX: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathRecord {
    inode: u64,
    len: u8,
    bytes: [u8; EXT4_INLINE_PATH_MAX],
}

#[derive(Debug)]
pub struct Ext4Backend {
    next_fd: u64,
    fds: [Option<FdRecord>; MAX_SERVICE_FDS],
    inodes: [Option<Ext4Inode>; MAX_SERVICE_INODES],
    paths: [Option<PathRecord>; MAX_SERVICE_INODES],
    max_file_len: u64,
    journal_seq: u64,
    cache: BlockCache,
}

impl Default for Ext4Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext4Backend {
    pub fn new() -> Self {
        let mut backend = Self {
            next_fd: 200,
            fds: [None; MAX_SERVICE_FDS],
            inodes: [None; MAX_SERVICE_INODES],
            paths: [None; MAX_SERVICE_INODES],
            max_file_len: 16 * 1024 * 1024,
            journal_seq: 0,
            cache: BlockCache::new(),
        };
        backend.seed_path(EXT4_DEMO_PATH_PTR, EXT4_DEMO_PATH);
        backend.seed_path(EXT4_SERVICE_PATH_PTR, EXT4_SERVICE_PATH);
        backend.seed_path(EXT4_OVERSIZE_PATH_PTR, EXT4_OVERSIZE_PATH);
        backend
    }

    pub const fn journal_seq(&self) -> u64 {
        self.journal_seq
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdRecord { fd, inode });
            Ok(fd)
        } else {
            Err(VfsError::NoFd)
        }
    }

    fn open_inode(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
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
        Err(VfsError::NoFd)
    }

    fn seed_path(&mut self, inode: u64, path: &[u8]) {
        let mut bytes = [0u8; EXT4_INLINE_PATH_MAX];
        bytes[..path.len()].copy_from_slice(path);
        if let Some(path_slot) = self.paths.iter_mut().find(|slot| slot.is_none()) {
            *path_slot = Some(PathRecord {
                inode,
                len: path.len() as u8,
                bytes,
            });
        }
        if let Some(inode_slot) = self.inodes.iter_mut().find(|slot| slot.is_none()) {
            *inode_slot = Some(Ext4Inode {
                path_ptr: inode,
                file_len: 0,
            });
        }
    }

    fn legacy_path_from_ptr(path_ptr: u64) -> Option<&'static [u8]> {
        match path_ptr {
            EXT4_DEMO_PATH_PTR => Some(EXT4_DEMO_PATH),
            EXT4_SERVICE_PATH_PTR => Some(EXT4_SERVICE_PATH),
            EXT4_OVERSIZE_PATH_PTR => Some(EXT4_OVERSIZE_PATH),
            _ => None,
        }
    }

    fn lookup_by_path(&self, path: &[u8]) -> Result<u64, VfsError> {
        self.paths
            .iter()
            .flatten()
            .find(|entry| &entry.bytes[..entry.len as usize] == path)
            .map(|entry| entry.inode)
            .ok_or(VfsError::InvalidPath)
    }

    fn metadata_by_path(&self, path: &[u8]) -> Result<u64, VfsError> {
        let inode = self.lookup_by_path(path)?;
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsError::BadFd)?;
        Ok(self.inodes[idx].ok_or(VfsError::BadFd)?.file_len)
    }

    fn open_inode_by_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        let inode = self.lookup_by_path(path)?;
        self.alloc_fd(inode)
    }

    fn open_inode_legacy_ptr(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        self.open_inode(path_ptr)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsError::BadFd)
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

    fn validate(&self) -> Result<(), VfsError> {
        Ok(())
    }
}

impl VfsBackend for Ext4Backend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if let Some(path) = Self::legacy_path_from_ptr(path_ptr) {
            self.openat_path(path)
        } else {
            self.open_inode_legacy_ptr(path_ptr)
        }
    }

    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        self.open_inode_by_path(path)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsError::BadFd)?;
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsError::BadFd)?;
        let file_len = self.inodes[idx].ok_or(VfsError::BadFd)?.file_len;
        let _ = self.cache.get(fd);
        Ok(core::cmp::min(len, file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let inode = self.inode_for_fd(fd).ok_or(VfsError::BadFd)?;
        let idx = find_inode_index(&self.inodes, inode).ok_or(VfsError::BadFd)?;
        let Some(mut inode_slot) = self.inodes[idx] else {
            return Err(VfsError::BadFd);
        };
        inode_slot.file_len = checked_append(inode_slot.file_len, len, self.max_file_len)?;
        self.inodes[idx] = Some(inode_slot);
        self.journal_seq = self.journal_seq.saturating_add(1);
        self.cache.put(fd, inode_slot.file_len);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if let Some(path) = Self::legacy_path_from_ptr(path_ptr) {
            self.statx_path(path)
        } else {
            let idx = find_inode_index(&self.inodes, path_ptr).ok_or(VfsError::BadFd)?;
            Ok(self.inodes[idx].ok_or(VfsError::BadFd)?.file_len)
        }
    }

    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        self.metadata_by_path(path)
    }
}

#[cfg(test)]
mod framing_tests {
    const VIRTIO_BLK_OP_READ: u16 = 1;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct VirtioBlkReqFrame {
        op: u16,
        _reserved: u16,
        sector: u64,
        len: u32,
        tag: u32,
    }

    impl VirtioBlkReqFrame {
        fn encode(self) -> [u8; 20] {
            let mut out = [0u8; 20];
            out[0..2].copy_from_slice(&self.op.to_le_bytes());
            out[2..4].copy_from_slice(&self._reserved.to_le_bytes());
            out[4..12].copy_from_slice(&self.sector.to_le_bytes());
            out[12..16].copy_from_slice(&self.len.to_le_bytes());
            out[16..20].copy_from_slice(&self.tag.to_le_bytes());
            out
        }

        fn decode(bytes: &[u8; 20]) -> Self {
            let mut op = [0u8; 2];
            op.copy_from_slice(&bytes[0..2]);
            let mut reserved = [0u8; 2];
            reserved.copy_from_slice(&bytes[2..4]);
            let mut sector = [0u8; 8];
            sector.copy_from_slice(&bytes[4..12]);
            let mut len = [0u8; 4];
            len.copy_from_slice(&bytes[12..16]);
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&bytes[16..20]);
            Self {
                op: u16::from_le_bytes(op),
                _reserved: u16::from_le_bytes(reserved),
                sector: u64::from_le_bytes(sector),
                len: u32::from_le_bytes(len),
                tag: u32::from_le_bytes(tag),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct VirtioBlkRespFrame {
        status: u8,
        _pad: [u8; 3],
        done_len: u32,
        tag: u32,
    }

    impl VirtioBlkRespFrame {
        fn encode(self) -> [u8; 12] {
            let mut out = [0u8; 12];
            out[0] = self.status;
            out[1..4].copy_from_slice(&self._pad);
            out[4..8].copy_from_slice(&self.done_len.to_le_bytes());
            out[8..12].copy_from_slice(&self.tag.to_le_bytes());
            out
        }

        fn decode(bytes: &[u8; 12]) -> Self {
            let mut done_len = [0u8; 4];
            done_len.copy_from_slice(&bytes[4..8]);
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&bytes[8..12]);
            Self {
                status: bytes[0],
                _pad: [bytes[1], bytes[2], bytes[3]],
                done_len: u32::from_le_bytes(done_len),
                tag: u32::from_le_bytes(tag),
            }
        }
    }

    #[test]
    fn ext4_request_frame_golden_vector_matches_contract() {
        let req = VirtioBlkReqFrame {
            op: VIRTIO_BLK_OP_READ,
            _reserved: 0,
            sector: 42,
            len: 4096,
            tag: 7,
        };
        let expected: [u8; 20] = [1, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 7, 0, 0, 0];
        assert_eq!(req.encode(), expected);
        assert_eq!(VirtioBlkReqFrame::decode(&expected), req);
    }

    #[test]
    fn ext4_response_frame_golden_vector_matches_contract() {
        let resp = VirtioBlkRespFrame {
            status: 0,
            _pad: [0; 3],
            done_len: 4096,
            tag: 7,
        };
        let expected: [u8; 12] = [0, 0, 0, 0, 0, 16, 0, 0, 7, 0, 0, 0];
        assert_eq!(resp.encode(), expected);
        assert_eq!(VirtioBlkRespFrame::decode(&expected), resp);
    }
}
