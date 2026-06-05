// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::fs::{FdRecord, ServiceFsBackend, MAX_SERVICE_FDS, MAX_SERVICE_INODES};
use super::super::common::vfs_ipc::{VfsBackend, VfsError};

use super::dir::find_inode_index;
use super::file::checked_append;
use super::inode::Ext4Inode;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockCache;

impl BlockCache {
    const fn new() -> Self {
        Self
    }
    fn get(&self, _fd: u64) -> Option<u64> {
        None
    }
    fn put(&mut self, _fd: u64, _len: u64) {}
}

/// Compatibility path-id constant used by mount/policy/interop tests.
pub const EXT4_DEMO_PATH_PTR: u64 = 0x4040;
pub const EXT4_DEMO_PATH: &[u8] = b"/ext4/file.bin";
/// Compatibility path-id constant used by mount/policy/interop tests.
pub const EXT4_SERVICE_PATH_PTR: u64 = 0x2020;
pub const EXT4_SERVICE_PATH: &[u8] = b"/ext4/service.bin";
/// Compatibility path-id constant used by mount/policy/interop tests.
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

pub const EXT4_SUPERBLOCK_OFFSET: usize = 1024;
const EXT4_MAGIC: u16 = 0xef53;
const EXT4_FEATURE_COMPAT_DIR_INDEX: u32 = 0x0020;
const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
const EXT4_FEATURE_INCOMPAT_INLINE_DATA: u32 = 0x8000;
const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const EXT4_FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0002;
const EXT4_FEATURE_RO_COMPAT_HUGE_FILE: u32 = 0x0008;
const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
const EXT4_FEATURE_RO_COMPAT_BIGALLOC: u32 = 0x0200;
const EXT4_FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x0400;
const EXT4_SUPPORTED_INCOMPAT: u32 = EXT4_FEATURE_INCOMPAT_FILETYPE
    | EXT4_FEATURE_INCOMPAT_EXTENTS
    | EXT4_FEATURE_INCOMPAT_64BIT
    | EXT4_FEATURE_INCOMPAT_FLEX_BG;
const EXT4_SUPPORTED_RO_COMPAT: u32 = EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
    | EXT4_FEATURE_RO_COMPAT_LARGE_FILE
    | EXT4_FEATURE_RO_COMPAT_HUGE_FILE
    | EXT4_FEATURE_RO_COMPAT_DIR_NLINK
    | EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE;
const EXT4_MAX_EXTENT_DEPTH: u16 = 5;
const EXT4_NDIR_BLOCKS: usize = 12;
const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
const EXT4_INDEX_FL: u32 = 0x0000_1000;
const EXT4_SYMLINK_LIMIT: u8 = 8;
const EXT4_DX_ROOT_INFO_OFFSET: usize = 24;
const EXT4_DX_ROOT_ENTRIES_OFFSET: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext4ImageError {
    Io,
    BadMagic,
    UnsupportedFeature(u32),
    UnsupportedLayout,
    NotFound,
    NotDirectory,
    IsDirectory,
    Malformed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Superblock {
    pub inodes_count: u32,
    pub blocks_count: u64,
    pub log_block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub inode_size: u16,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
}

impl Ext4Superblock {
    pub fn block_size(&self) -> u64 {
        1024u64 << self.log_block_size
    }

    pub fn parse(image: &[u8]) -> Result<Self, Ext4ImageError> {
        let sb = image
            .get(EXT4_SUPERBLOCK_OFFSET..EXT4_SUPERBLOCK_OFFSET + 1024)
            .ok_or(Ext4ImageError::Io)?;
        let magic = le_u16(sb, 56)?;
        if magic != EXT4_MAGIC {
            return Err(Ext4ImageError::BadMagic);
        }
        let feature_compat = le_u32(sb, 92)?;
        let feature_incompat = le_u32(sb, 96)?;
        let feature_ro_compat = le_u32(sb, 100)?;
        let unsupported = feature_incompat & !EXT4_SUPPORTED_INCOMPAT;
        if unsupported != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(unsupported));
        }
        if (feature_incompat & EXT4_FEATURE_INCOMPAT_INLINE_DATA) != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_INCOMPAT_INLINE_DATA,
            ));
        }
        let unsupported_ro = feature_ro_compat & !EXT4_SUPPORTED_RO_COMPAT;
        if unsupported_ro != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(unsupported_ro));
        }
        if (feature_ro_compat & EXT4_FEATURE_RO_COMPAT_METADATA_CSUM) != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
            ));
        }
        if (feature_ro_compat & EXT4_FEATURE_RO_COMPAT_BIGALLOC) != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_BIGALLOC,
            ));
        }
        let blocks_lo = le_u32(sb, 4)? as u64;
        let blocks_hi = le_u32(sb, 336).unwrap_or(0) as u64;
        let inode_size = le_u16(sb, 88).unwrap_or(128);
        Ok(Self {
            inodes_count: le_u32(sb, 0)?,
            blocks_count: blocks_lo | (blocks_hi << 32),
            log_block_size: le_u32(sb, 24)?,
            blocks_per_group: le_u32(sb, 32)?,
            inodes_per_group: le_u32(sb, 40)?,
            inode_size: if inode_size == 0 { 128 } else { inode_size },
            feature_compat,
            feature_incompat,
            feature_ro_compat,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext4FileType {
    Unknown,
    Regular,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4DirEntry {
    pub inode: u32,
    pub file_type: Ext4FileType,
    pub name_len: u8,
    name: [u8; 255],
}

impl Ext4DirEntry {
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    logical: u32,
    len: u16,
    start: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedInode {
    mode: u16,
    size: u64,
    flags: u32,
    block: [u8; 60],
}

impl ParsedInode {
    fn file_type(&self) -> Ext4FileType {
        match self.mode & 0xf000 {
            0x4000 => Ext4FileType::Directory,
            0x8000 => Ext4FileType::Regular,
            0xa000 => Ext4FileType::Symlink,
            _ => Ext4FileType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ext4Image<'a> {
    image: &'a [u8],
    sb: Ext4Superblock,
    desc_size: u16,
}

impl<'a> Ext4Image<'a> {
    pub fn mount(image: &'a [u8]) -> Result<Self, Ext4ImageError> {
        let sb = Ext4Superblock::parse(image)?;
        if sb.log_block_size > 6
            || sb.block_size() < 1024
            || sb.blocks_per_group == 0
            || sb.inodes_per_group == 0
            || sb.inode_size < 128
        {
            return Err(Ext4ImageError::Malformed);
        }
        let image_blocks = (image.len() as u64) / sb.block_size();
        if sb.blocks_count > image_blocks {
            return Err(Ext4ImageError::Io);
        }
        let desc_size = if (sb.feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT) != 0 {
            le_u16(
                image
                    .get(EXT4_SUPERBLOCK_OFFSET..EXT4_SUPERBLOCK_OFFSET + 1024)
                    .ok_or(Ext4ImageError::Io)?,
                254,
            )
            .unwrap_or(64)
        } else {
            32
        };
        if desc_size < 32 || desc_size % 8 != 0 || u64::from(desc_size) > sb.block_size() {
            return Err(Ext4ImageError::Malformed);
        }
        let mounted = Self {
            image,
            sb,
            desc_size,
        };
        mounted.validate_group_descriptor_table()?;
        Ok(mounted)
    }

    pub const fn superblock(&self) -> Ext4Superblock {
        self.sb
    }

    fn group_count(&self) -> Result<u32, Ext4ImageError> {
        if self.sb.blocks_per_group == 0 {
            return Err(Ext4ImageError::Malformed);
        }
        let groups = self
            .sb
            .blocks_count
            .saturating_add(u64::from(self.sb.blocks_per_group) - 1)
            / u64::from(self.sb.blocks_per_group);
        u32::try_from(groups).map_err(|_| Ext4ImageError::UnsupportedLayout)
    }

    fn validate_group_descriptor_table(&self) -> Result<(), Ext4ImageError> {
        let groups = self.group_count()? as usize;
        let bytes = groups
            .checked_mul(self.desc_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        let table_block = if self.sb.block_size() == 1024 { 2 } else { 1 };
        let start = self.block_offset(table_block)?;
        let end = start.checked_add(bytes).ok_or(Ext4ImageError::Io)?;
        self.image.get(start..end).ok_or(Ext4ImageError::Io)?;
        Ok(())
    }

    fn block_offset(&self, block: u64) -> Result<usize, Ext4ImageError> {
        usize::try_from(
            block
                .checked_mul(self.sb.block_size())
                .ok_or(Ext4ImageError::Io)?,
        )
        .map_err(|_| Ext4ImageError::Io)
    }

    fn group_desc(&self, group: u32) -> Result<&'a [u8], Ext4ImageError> {
        if group >= self.group_count()? {
            return Err(Ext4ImageError::Io);
        }
        let table_block = if self.sb.block_size() == 1024 { 2 } else { 1 };
        let start = self
            .block_offset(table_block)?
            .checked_add(group as usize * self.desc_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        self.image
            .get(start..start + self.desc_size as usize)
            .ok_or(Ext4ImageError::Io)
    }

    fn inode_table_block(&self, group: u32) -> Result<u64, Ext4ImageError> {
        let gd = self.group_desc(group)?;
        let lo = le_u32(gd, 8)? as u64;
        let hi = if self.desc_size as usize >= 64 {
            le_u32(gd, 40).unwrap_or(0) as u64
        } else {
            0
        };
        let block = lo | (hi << 32);
        if block == 0 || block >= self.sb.blocks_count {
            return Err(Ext4ImageError::Io);
        }
        Ok(block)
    }

    fn inode(&self, inode: u32) -> Result<ParsedInode, Ext4ImageError> {
        if inode == 0 || inode > self.sb.inodes_count {
            return Err(Ext4ImageError::NotFound);
        }
        let idx = inode - 1;
        let group = idx / self.sb.inodes_per_group;
        let index = idx % self.sb.inodes_per_group;
        let inode_table = self.inode_table_block(group)?;
        let off = self
            .block_offset(inode_table)?
            .checked_add(index as usize * self.sb.inode_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        let inode_end = off
            .checked_add(self.sb.inode_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        let raw = self.image.get(off..inode_end).ok_or(Ext4ImageError::Io)?;
        let size_lo = le_u32(raw, 4)? as u64;
        let size_hi = le_u32(raw, 108).unwrap_or(0) as u64;
        let mut block = [0u8; 60];
        block.copy_from_slice(raw.get(40..100).ok_or(Ext4ImageError::Malformed)?);
        Ok(ParsedInode {
            mode: le_u16(raw, 0)?,
            size: size_lo | (size_hi << 32),
            flags: le_u32(raw, 32)?,
            block,
        })
    }

    fn extents(&self, inode: &ParsedInode) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        if (inode.flags & EXT4_EXTENTS_FL) != 0 || inode.block[0..2] == 0xf30au16.to_le_bytes() {
            self.parse_extent_tree(&inode.block, 0)
        } else {
            self.indirect_extents(inode)
        }
    }

    fn indirect_extents(
        &self,
        inode: &ParsedInode,
    ) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        let block_size = self.sb.block_size() as usize;
        let blocks_needed = usize::try_from(inode.size.div_ceil(self.sb.block_size()))
            .map_err(|_| Ext4ImageError::UnsupportedLayout)?;
        let mut out = alloc::vec::Vec::new();
        for logical in 0..core::cmp::min(blocks_needed, EXT4_NDIR_BLOCKS) {
            let ptr = le_u32(&inode.block, logical * 4)?;
            if ptr != 0 {
                out.push(Extent {
                    logical: logical as u32,
                    len: 1,
                    start: u64::from(ptr),
                });
            }
        }
        if blocks_needed > EXT4_NDIR_BLOCKS {
            let single = le_u32(&inode.block, EXT4_NDIR_BLOCKS * 4)?;
            let ptrs_per_block = block_size / 4;
            let remaining = blocks_needed - EXT4_NDIR_BLOCKS;
            if single != 0 {
                let raw = self.block_bytes(u64::from(single))?;
                for idx in 0..core::cmp::min(remaining, ptrs_per_block) {
                    let ptr = le_u32(raw, idx * 4)?;
                    if ptr != 0 {
                        out.push(Extent {
                            logical: (EXT4_NDIR_BLOCKS + idx) as u32,
                            len: 1,
                            start: u64::from(ptr),
                        });
                    }
                }
            }
            if remaining > ptrs_per_block {
                let double = le_u32(&inode.block, (EXT4_NDIR_BLOCKS + 1) * 4)?;
                let triple = le_u32(&inode.block, (EXT4_NDIR_BLOCKS + 2) * 4)?;
                if double != 0 || triple != 0 {
                    return Err(Ext4ImageError::UnsupportedLayout);
                }
            }
        }
        Ok(out)
    }

    fn parse_extent_tree(
        &self,
        raw: &[u8],
        recursion_depth: u16,
    ) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        let header_depth = extent_header_depth(raw)?;
        if header_depth > EXT4_MAX_EXTENT_DEPTH || recursion_depth > EXT4_MAX_EXTENT_DEPTH {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        if header_depth == 0 {
            return parse_extent_leaf(raw);
        }
        let entries = le_u16(raw, 2)? as usize;
        let max_entries = le_u16(raw, 4)? as usize;
        if entries > max_entries {
            return Err(Ext4ImageError::Malformed);
        }
        let mut out = alloc::vec::Vec::new();
        let mut last_logical = None;
        for idx in 0..entries {
            let off = 12 + idx * 12;
            let logical = le_u32(raw, off)?;
            if last_logical.map(|prev| logical < prev).unwrap_or(false) {
                return Err(Ext4ImageError::Malformed);
            }
            last_logical = Some(logical);
            let leaf_lo = le_u32(raw, off + 4)? as u64;
            let leaf_hi = le_u16(raw, off + 8)? as u64;
            let child_block = leaf_lo | (leaf_hi << 32);
            let child = self.block_bytes(child_block)?;
            if extent_header_depth(child)? + 1 != header_depth {
                return Err(Ext4ImageError::Malformed);
            }
            out.extend(self.parse_extent_tree(child, recursion_depth + 1)?);
        }
        Ok(out)
    }

    fn block_bytes(&self, block: u64) -> Result<&'a [u8], Ext4ImageError> {
        if block >= self.sb.blocks_count {
            return Err(Ext4ImageError::Io);
        }
        let start = self.block_offset(block)?;
        let end = start
            .checked_add(self.sb.block_size() as usize)
            .ok_or(Ext4ImageError::Io)?;
        self.image.get(start..end).ok_or(Ext4ImageError::Io)
    }

    fn read_inode_bytes(&self, inode: u32) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        self.read_inode_bytes_from_meta(self.inode(inode)?)
    }

    fn read_inode_bytes_from_meta(
        &self,
        inode: ParsedInode,
    ) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        if inode.file_type() == Ext4FileType::Directory
            || inode.file_type() == Ext4FileType::Regular
            || (inode.file_type() == Ext4FileType::Symlink && inode.size > 60)
        {
            let mut out = alloc::vec![0u8; inode.size as usize];
            for ex in self.extents(&inode)? {
                if ex
                    .start
                    .checked_add(u64::from(ex.len))
                    .map(|end| end > self.sb.blocks_count)
                    .unwrap_or(true)
                {
                    return Err(Ext4ImageError::Io);
                }
                let src = self.block_offset(ex.start)?;
                let dst = ex.logical as usize * self.sb.block_size() as usize;
                if dst >= out.len() {
                    continue;
                }
                let len = core::cmp::min(
                    ex.len as usize * self.sb.block_size() as usize,
                    out.len().saturating_sub(dst),
                );
                if len == 0 {
                    continue;
                }
                out.get_mut(dst..dst + len)
                    .ok_or(Ext4ImageError::Malformed)?
                    .copy_from_slice(self.image.get(src..src + len).ok_or(Ext4ImageError::Io)?);
            }
            Ok(out)
        } else if inode.file_type() == Ext4FileType::Symlink && inode.size <= 60 {
            Ok(inode.block[..inode.size as usize].to_vec())
        } else {
            Err(Ext4ImageError::UnsupportedLayout)
        }
    }

    pub fn read_file(&self, path: &[u8]) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        let inode = self.lookup_path_follow(path)?;
        let meta = self.inode(inode)?;
        if meta.file_type() != Ext4FileType::Regular {
            return Err(Ext4ImageError::IsDirectory);
        }
        self.read_inode_bytes(inode)
    }

    pub fn read_symlink(&self, path: &[u8]) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        let inode = self.lookup_path(path)?;
        let meta = self.inode(inode)?;
        if meta.file_type() != Ext4FileType::Symlink {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        self.read_inode_bytes(inode)
    }

    pub fn read_dir(&self, path: &[u8]) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
        let inode = self.lookup_path(path)?;
        let meta = self.inode(inode)?;
        if meta.file_type() != Ext4FileType::Directory {
            return Err(Ext4ImageError::NotDirectory);
        }
        let _dir_index_linear_fallback = (meta.flags & EXT4_INDEX_FL) != 0
            && (self.sb.feature_compat & EXT4_FEATURE_COMPAT_DIR_INDEX) != 0;
        parse_dir_entries(self.read_inode_bytes(inode)?.as_slice())
    }

    fn lookup_dir_entry_inode(&self, dir_inode: u32, name: &[u8]) -> Result<u32, Ext4ImageError> {
        let inode = self.inode(dir_inode)?;
        if inode.file_type() != Ext4FileType::Directory {
            return Err(Ext4ImageError::NotDirectory);
        }
        if (inode.flags & EXT4_INDEX_FL) != 0
            && (self.sb.feature_compat & EXT4_FEATURE_COMPAT_DIR_INDEX) != 0
        {
            match self.htree_lookup_inode(&inode, name) {
                Ok(Some(found)) => return Ok(found),
                Ok(None) => return Err(Ext4ImageError::NotFound),
                Err(Ext4ImageError::UnsupportedLayout) => {}
                Err(err) => return Err(err),
            }
        }
        parse_dir_entries(self.read_inode_bytes(dir_inode)?.as_slice())?
            .iter()
            .find(|e| e.name() == name)
            .map(|e| e.inode)
            .ok_or(Ext4ImageError::NotFound)
    }

    fn htree_lookup_inode(
        &self,
        dir_inode: &ParsedInode,
        name: &[u8],
    ) -> Result<Option<u32>, Ext4ImageError> {
        let bytes = self.read_inode_bytes_by_meta(dir_inode)?;
        let block_size = self.sb.block_size() as usize;
        if bytes.len() < block_size || block_size < EXT4_DX_ROOT_ENTRIES_OFFSET + 8 {
            return Err(Ext4ImageError::Malformed);
        }
        if le_u32(&bytes, EXT4_DX_ROOT_INFO_OFFSET)? != 0 {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        let hash_version = *bytes
            .get(EXT4_DX_ROOT_INFO_OFFSET + 4)
            .ok_or(Ext4ImageError::Malformed)?;
        let info_len = *bytes
            .get(EXT4_DX_ROOT_INFO_OFFSET + 5)
            .ok_or(Ext4ImageError::Malformed)? as usize;
        let indirect_levels = *bytes
            .get(EXT4_DX_ROOT_INFO_OFFSET + 6)
            .ok_or(Ext4ImageError::Malformed)?;
        if !matches!(hash_version, 0 | 1 | 2 | 3) {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        if info_len < 8 || indirect_levels != 0 {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        let limit = le_u16(&bytes, EXT4_DX_ROOT_ENTRIES_OFFSET)? as usize;
        let count = le_u16(&bytes, EXT4_DX_ROOT_ENTRIES_OFFSET + 2)? as usize;
        if count == 0 || count > limit {
            return Err(Ext4ImageError::Malformed);
        }
        let entries_bytes = count.checked_mul(8).ok_or(Ext4ImageError::Io)?;
        let entries_end = EXT4_DX_ROOT_ENTRIES_OFFSET
            .checked_add(entries_bytes)
            .ok_or(Ext4ImageError::Io)?;
        if entries_end > block_size {
            return Err(Ext4ImageError::Malformed);
        }
        let mut last_hash = 0u32;
        for idx in 0..count {
            let off = EXT4_DX_ROOT_ENTRIES_OFFSET + idx * 8;
            let hash = le_u32(&bytes, off)?;
            if idx > 0 && hash < last_hash {
                return Err(Ext4ImageError::Malformed);
            }
            last_hash = hash;
            let logical_block = le_u32(&bytes, off + 4)? as usize;
            let start = logical_block
                .checked_mul(block_size)
                .ok_or(Ext4ImageError::Io)?;
            let end = start.checked_add(block_size).ok_or(Ext4ImageError::Io)?;
            let leaf = bytes.get(start..end).ok_or(Ext4ImageError::Io)?;
            for entry in parse_dir_entries(leaf)? {
                if entry.name() == name {
                    return Ok(Some(entry.inode));
                }
            }
        }
        Ok(None)
    }

    fn read_inode_bytes_by_meta(
        &self,
        inode: &ParsedInode,
    ) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        self.read_inode_bytes_from_meta(*inode)
    }

    pub fn lookup_path(&self, path: &[u8]) -> Result<u32, Ext4ImageError> {
        self.lookup_path_inner(path, false, 0)
    }

    pub fn lookup_path_follow(&self, path: &[u8]) -> Result<u32, Ext4ImageError> {
        self.lookup_path_inner(path, true, 0)
    }

    fn lookup_path_inner(
        &self,
        path: &[u8],
        follow_final: bool,
        depth: u8,
    ) -> Result<u32, Ext4ImageError> {
        if depth > EXT4_SYMLINK_LIMIT {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        if path == b"/" || path.is_empty() {
            return Ok(2);
        }
        let comps: alloc::vec::Vec<&[u8]> = path
            .split(|b| *b == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        let mut current = 2u32;
        let mut prefix = alloc::vec::Vec::new();
        prefix.push(b'/');
        for (idx, comp) in comps.iter().enumerate() {
            let inode = self.inode(current)?;
            if inode.file_type() != Ext4FileType::Directory {
                return Err(Ext4ImageError::NotDirectory);
            }
            let next = self.lookup_dir_entry_inode(current, comp)?;
            let next_inode = self.inode(next)?;
            let is_last = idx == comps.len() - 1;
            if next_inode.file_type() == Ext4FileType::Symlink && (!is_last || follow_final) {
                let target = self.read_inode_bytes(next)?;
                let mut resolved = alloc::vec::Vec::new();
                if target.first().copied() == Some(b'/') {
                    resolved.extend_from_slice(target.as_slice());
                } else {
                    resolved.extend_from_slice(prefix.as_slice());
                    if !prefix.ends_with(b"/") {
                        resolved.push(b'/');
                    }
                    resolved.extend_from_slice(target.as_slice());
                }
                for tail in comps.iter().skip(idx + 1) {
                    if !resolved.ends_with(b"/") {
                        resolved.push(b'/');
                    }
                    resolved.extend_from_slice(tail);
                }
                return self.lookup_path_inner(resolved.as_slice(), follow_final, depth + 1);
            }
            current = next;
            if !prefix.ends_with(b"/") {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(comp);
        }
        Ok(current)
    }
}

fn extent_header_depth(raw: &[u8]) -> Result<u16, Ext4ImageError> {
    if le_u16(raw, 0)? != 0xf30a {
        return Err(Ext4ImageError::UnsupportedLayout);
    }
    Ok(le_u16(raw, 6)?)
}

fn parse_extent_leaf(raw: &[u8]) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
    if extent_header_depth(raw)? != 0 {
        return Err(Ext4ImageError::UnsupportedLayout);
    }
    let entries = le_u16(raw, 2)? as usize;
    let max_entries = le_u16(raw, 4)? as usize;
    if entries > max_entries {
        return Err(Ext4ImageError::Malformed);
    }
    let mut out = alloc::vec::Vec::new();
    let mut last_logical = None;
    for idx in 0..entries {
        let off = 12 + idx * 12;
        let logical = le_u32(raw, off)?;
        if last_logical.map(|prev| logical < prev).unwrap_or(false) {
            return Err(Ext4ImageError::Malformed);
        }
        last_logical = Some(logical);
        let len = le_u16(raw, off + 4)?;
        let start_hi = le_u16(raw, off + 6)? as u64;
        let start_lo = le_u32(raw, off + 8)? as u64;
        out.push(Extent {
            logical,
            len: len & 0x7fff,
            start: (start_hi << 32) | start_lo,
        });
    }
    Ok(out)
}

fn parse_dir_entries(bytes: &[u8]) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
    let mut out = alloc::vec::Vec::new();
    let mut off = 0usize;
    while off + 8 <= bytes.len() {
        let inode = le_u32(bytes, off)?;
        let rec_len = le_u16(bytes, off + 4)? as usize;
        let name_len = bytes[off + 6];
        let file_type = match bytes[off + 7] {
            1 => Ext4FileType::Regular,
            2 => Ext4FileType::Directory,
            7 => Ext4FileType::Symlink,
            _ => Ext4FileType::Unknown,
        };
        if rec_len < 8 || off + rec_len > bytes.len() {
            return Err(Ext4ImageError::Malformed);
        }
        if inode != 0 && name_len != 0 {
            let name_src = bytes
                .get(off + 8..off + 8 + name_len as usize)
                .ok_or(Ext4ImageError::Malformed)?;
            let mut name = [0u8; 255];
            name[..name_len as usize].copy_from_slice(name_src);
            out.push(Ext4DirEntry {
                inode,
                file_type,
                name_len,
                name,
            });
        }
        off += rec_len;
    }
    Ok(out)
}

fn le_u16(bytes: &[u8], off: usize) -> Result<u16, Ext4ImageError> {
    Ok(u16::from_le_bytes(
        bytes
            .get(off..off + 2)
            .ok_or(Ext4ImageError::Io)?
            .try_into()
            .map_err(|_| Ext4ImageError::Io)?,
    ))
}

fn le_u32(bytes: &[u8], off: usize) -> Result<u32, Ext4ImageError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(off..off + 4)
            .ok_or(Ext4ImageError::Io)?
            .try_into()
            .map_err(|_| Ext4ImageError::Io)?,
    ))
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn ext4_superblock_and_extent_file_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.superblock().block_size(), 1024);
        assert_eq!(fs.lookup_path(b"/hello.txt"), Ok(12));
        assert_eq!(
            fs.read_file(b"/hello.txt").unwrap(),
            b"hello from ext4\n".to_vec()
        );
    }

    #[test]
    fn ext4_directory_listing_reports_file_types() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let entries = fs.read_dir(b"/").unwrap();
        assert!(entries
            .iter()
            .any(|e| e.name() == b"hello.txt" && e.file_type == Ext4FileType::Regular));
    }

    #[test]
    fn ext4_rejects_unknown_required_feature() {
        let mut img = tiny_ext4_image();
        let off = EXT4_SUPERBLOCK_OFFSET + 96;
        let unsupported = EXT4_SUPPORTED_INCOMPAT | 0x8000_0000;
        img[off..off + 4].copy_from_slice(&unsupported.to_le_bytes());
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(0x8000_0000))
        ));
    }

    #[test]
    fn ext4_depth1_extent_index_file_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.lookup_path(b"/depth1.bin"), Ok(13));
        assert_eq!(
            fs.read_file(b"/depth1.bin").unwrap(),
            b"depth one extent\n".to_vec()
        );
    }

    #[test]
    fn ext4_sparse_extent_hole_reads_as_zeroes() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let bytes = fs.read_file(b"/hole.bin").unwrap();
        assert_eq!(bytes.len(), 2048);
        assert!(bytes[..1024].iter().all(|byte| *byte == 0));
        assert_eq!(&bytes[1024..1040], b"after sparse gap");
    }

    #[test]
    fn ext4_inline_symlink_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.read_symlink(b"/link").unwrap(), b"hello.txt".to_vec());
    }

    #[test]
    fn ext4_invalid_extent_depth_is_rejected() {
        let mut img = tiny_ext4_image();
        put_u16(
            &mut img,
            5 * 1024 + 12 * 128 + 46,
            EXT4_MAX_EXTENT_DEPTH + 1,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.read_file(b"/depth1.bin"),
            Err(Ext4ImageError::UnsupportedLayout)
        );
    }

    #[test]
    fn ext4_invalid_extent_block_pointer_is_rejected() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 12 * 128 + 56, 99_999);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.read_file(b"/depth1.bin"), Err(Ext4ImageError::Io));
    }

    #[test]
    fn ext4_legacy_direct_block_file_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.read_file(b"/direct.bin").unwrap(),
            b"direct block file\n".to_vec()
        );
    }

    #[test]
    fn ext4_legacy_single_indirect_file_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let bytes = fs.read_file(b"/single.bin").unwrap();
        assert_eq!(bytes.len(), 13 * 1024);
        assert!(bytes[..12 * 1024].iter().all(|byte| *byte == 0));
        assert_eq!(&bytes[12 * 1024..12 * 1024 + 15], b"single indirect");
    }

    #[test]
    fn ext4_legacy_indirect_sparse_hole_reads_as_zeroes() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let bytes = fs.read_file(b"/sparsei.bin").unwrap();
        assert_eq!(bytes.len(), 2048);
        assert!(bytes[..1024].iter().all(|byte| *byte == 0));
        assert_eq!(&bytes[1024..1036], b"indirect gap");
    }

    #[test]
    fn ext4_invalid_legacy_block_pointer_is_rejected() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 15 * 128 + 40, 99_999);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.read_file(b"/direct.bin"), Err(Ext4ImageError::Io));
    }

    #[test]
    fn ext4_double_indirect_remains_unsupported() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.read_file(b"/double.bin"),
            Err(Ext4ImageError::UnsupportedLayout)
        );
    }

    #[test]
    fn ext4_external_symlink_read_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let target = long_symlink_target();
        assert_eq!(fs.read_symlink(b"/longlink").unwrap(), target);
    }

    #[test]
    fn ext4_final_symlink_path_resolution_works_and_loops_are_rejected() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.read_file(b"/link").unwrap(),
            b"hello from ext4\n".to_vec()
        );
        assert_eq!(
            fs.read_file(b"/loop"),
            Err(Ext4ImageError::UnsupportedLayout)
        );
    }

    #[test]
    fn ext4_metadata_csum_and_bigalloc_are_rejected() {
        let mut img = tiny_ext4_image();
        put_u32(
            &mut img,
            EXT4_SUPERBLOCK_OFFSET + 100,
            EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
        );
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_METADATA_CSUM
            ))
        ));
        put_u32(
            &mut img,
            EXT4_SUPERBLOCK_OFFSET + 100,
            EXT4_FEATURE_RO_COMPAT_BIGALLOC,
        );
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_BIGALLOC
            ))
        ));
    }

    #[test]
    fn ext4_small_inode_size_is_rejected() {
        let mut img = tiny_ext4_image();
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 88, 64);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::Malformed)
        ));
    }

    #[test]
    fn ext4_64bit_descriptor_size_parses_low_inode_table() {
        let mut img = tiny_ext4_image();
        let incompat = EXT4_FEATURE_INCOMPAT_FILETYPE
            | EXT4_FEATURE_INCOMPAT_EXTENTS
            | EXT4_FEATURE_INCOMPAT_64BIT;
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 96, incompat);
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 254, 64);
        assert_eq!(
            Ext4Image::mount(img.as_slice())
                .unwrap()
                .read_file(b"/hello.txt")
                .unwrap(),
            b"hello from ext4\n".to_vec()
        );
    }

    #[test]
    fn ext4_64bit_high_inode_table_out_of_image_is_rejected() {
        let mut img = tiny_ext4_image();
        let incompat = EXT4_FEATURE_INCOMPAT_FILETYPE
            | EXT4_FEATURE_INCOMPAT_EXTENTS
            | EXT4_FEATURE_INCOMPAT_64BIT;
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 96, incompat);
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 254, 64);
        put_u32(&mut img, 2 * 1024 + 8, 0);
        put_u32(&mut img, 2 * 1024 + 40, 1);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.read_dir(b"/"), Err(Ext4ImageError::Io));
    }

    #[test]
    fn ext4_bad_64bit_descriptor_size_or_table_bounds_are_rejected() {
        let mut img = tiny_ext4_image();
        let incompat = EXT4_FEATURE_INCOMPAT_FILETYPE
            | EXT4_FEATURE_INCOMPAT_EXTENTS
            | EXT4_FEATURE_INCOMPAT_64BIT;
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 96, incompat);
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 254, 24);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::Malformed)
        ));

        let mut img = tiny_ext4_image();
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 96, incompat);
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 254, 1024);
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 32, 1);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::Io)
        ));
    }

    #[test]
    fn ext4_htree_indexed_directory_lookup_uses_index_leaf_scan() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));
        assert_eq!(
            fs.read_file(b"/indexed/target.bin").unwrap(),
            b"indexed target\n".to_vec()
        );
        assert_eq!(
            fs.lookup_path(b"/indexed/missing"),
            Err(Ext4ImageError::NotFound)
        );
    }

    #[test]
    fn ext4_malformed_htree_count_or_leaf_pointer_rejected() {
        let mut img = tiny_ext4_image();
        put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 2);
        put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET, 1);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.lookup_path(b"/indexed/target.bin"),
            Err(Ext4ImageError::Malformed)
        );

        let mut img = tiny_ext4_image();
        put_u32(
            &mut img,
            35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 4,
            99_999,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.lookup_path(b"/indexed/target.bin"),
            Err(Ext4ImageError::Io)
        );
    }

    fn tiny_ext4_image() -> alloc::vec::Vec<u8> {
        let mut img = alloc::vec![0u8; 80 * 1024];
        let sb = EXT4_SUPERBLOCK_OFFSET;
        put_u32(&mut img, sb, 32); // inodes
        put_u32(&mut img, sb + 4, 80); // blocks
        put_u32(&mut img, sb + 24, 0); // 1KiB blocks
        put_u32(&mut img, sb + 32, 8192);
        put_u32(&mut img, sb + 40, 32);
        put_u16(&mut img, sb + 56, EXT4_MAGIC);
        put_u16(&mut img, sb + 88, 128);
        put_u32(&mut img, sb + 92, EXT4_FEATURE_COMPAT_DIR_INDEX);
        put_u32(
            &mut img,
            sb + 96,
            EXT4_FEATURE_INCOMPAT_FILETYPE | EXT4_FEATURE_INCOMPAT_EXTENTS,
        );
        put_u32(&mut img, 2 * 1024 + 8, 5); // inode table block
        write_inode(&mut img, 5 * 1024 + 128, 0x4000, 1024, 20);
        put_u32(&mut img, 5 * 1024 + 128 + 32, EXT4_INDEX_FL);
        write_inode(&mut img, 5 * 1024 + 11 * 128, 0x8000, 16, 21);
        write_depth1_inode(&mut img, 5 * 1024 + 12 * 128, 0x8000, 17, 22);
        write_inode_logical(&mut img, 5 * 1024 + 13 * 128, 0x8000, 2048, 1, 25);
        write_inline_symlink_inode(&mut img, 5 * 1024 + 14 * 128, b"hello.txt");
        write_indirect_inode(&mut img, 5 * 1024 + 15 * 128, 0x8000, 18, &[26], 0, 0, 0);
        write_indirect_inode(
            &mut img,
            5 * 1024 + 16 * 128,
            0x8000,
            13 * 1024,
            &[0; 12],
            31,
            0,
            0,
        );
        write_indirect_inode(
            &mut img,
            5 * 1024 + 17 * 128,
            0x8000,
            2048,
            &[0, 27],
            0,
            0,
            0,
        );
        write_indirect_inode(
            &mut img,
            5 * 1024 + 18 * 128,
            0x8000,
            270 * 1024,
            &[0; 12],
            0,
            33,
            0,
        );
        let long_target = long_symlink_target();
        write_indirect_inode(
            &mut img,
            5 * 1024 + 19 * 128,
            0xa000,
            long_target.len() as u32,
            &[34],
            0,
            0,
            0,
        );
        write_inline_symlink_inode(&mut img, 5 * 1024 + 20 * 128, b"loop");
        write_extent_inode_with_len_flags(
            &mut img,
            5 * 1024 + 21 * 128,
            0x4000,
            2048,
            0,
            2,
            35,
            EXT4_INDEX_FL,
        );
        write_inode(&mut img, 5 * 1024 + 22 * 128, 0x8000, 15, 37);
        write_dirent(&mut img[20 * 1024..20 * 1024 + 12], 2, b".", 2, 12);
        write_dirent(&mut img[20 * 1024 + 12..20 * 1024 + 24], 2, b"..", 2, 12);
        write_dirent(
            &mut img[20 * 1024 + 24..20 * 1024 + 44],
            12,
            b"hello.txt",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 44..20 * 1024 + 64],
            13,
            b"depth1.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 64..20 * 1024 + 84],
            14,
            b"hole.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 84..20 * 1024 + 104],
            15,
            b"link",
            7,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 104..20 * 1024 + 124],
            16,
            b"direct.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 124..20 * 1024 + 144],
            17,
            b"single.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 144..20 * 1024 + 164],
            18,
            b"sparsei.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 164..20 * 1024 + 184],
            19,
            b"double.bin",
            1,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 184..20 * 1024 + 204],
            20,
            b"longlink",
            7,
            20,
        );
        write_dirent(
            &mut img[20 * 1024 + 204..20 * 1024 + 224],
            21,
            b"loop",
            7,
            20,
        );
        write_dirent(&mut img[20 * 1024 + 224..21 * 1024], 22, b"indexed", 2, 800);
        write_leaf_extent_block(&mut img[22 * 1024..23 * 1024], 0, 1, 24);
        img[21 * 1024..21 * 1024 + 16].copy_from_slice(b"hello from ext4\n");
        img[24 * 1024..24 * 1024 + 17].copy_from_slice(b"depth one extent\n");
        img[25 * 1024..25 * 1024 + 16].copy_from_slice(b"after sparse gap");
        img[26 * 1024..26 * 1024 + 18].copy_from_slice(b"direct block file\n");
        img[27 * 1024..27 * 1024 + 12].copy_from_slice(b"indirect gap");
        put_u32(&mut img, 31 * 1024, 32);
        img[32 * 1024..32 * 1024 + 15].copy_from_slice(b"single indirect");
        img[34 * 1024..34 * 1024 + long_target.len()].copy_from_slice(long_target.as_slice());
        write_htree_root_block(&mut img[35 * 1024..36 * 1024], 1);
        write_dirent(&mut img[36 * 1024..37 * 1024], 23, b"target.bin", 1, 1024);
        img[37 * 1024..37 * 1024 + 15].copy_from_slice(b"indexed target\n");
        img
    }

    fn write_inode(img: &mut [u8], off: usize, mode: u16, size: u32, extent_block: u32) {
        write_inode_logical(img, off, mode, size, 0, extent_block);
    }

    fn write_inode_logical(
        img: &mut [u8],
        off: usize,
        mode: u16,
        size: u32,
        logical: u32,
        extent_block: u32,
    ) {
        put_u16(img, off, mode);
        put_u32(img, off + 4, size);
        put_u32(img, off + 32, EXT4_EXTENTS_FL);
        put_u16(img, off + 40, 0xf30a);
        put_u16(img, off + 42, 1);
        put_u16(img, off + 44, 4);
        put_u16(img, off + 46, 0);
        put_u32(img, off + 52, logical);
        put_u16(img, off + 56, 1);
        put_u16(img, off + 58, 0);
        put_u32(img, off + 60, extent_block);
    }

    fn write_extent_inode_with_len_flags(
        img: &mut [u8],
        off: usize,
        mode: u16,
        size: u32,
        logical: u32,
        len: u16,
        extent_block: u32,
        extra_flags: u32,
    ) {
        put_u16(img, off, mode);
        put_u32(img, off + 4, size);
        put_u32(img, off + 32, EXT4_EXTENTS_FL | extra_flags);
        put_u16(img, off + 40, 0xf30a);
        put_u16(img, off + 42, 1);
        put_u16(img, off + 44, 4);
        put_u16(img, off + 46, 0);
        put_u32(img, off + 52, logical);
        put_u16(img, off + 56, len);
        put_u16(img, off + 58, 0);
        put_u32(img, off + 60, extent_block);
    }

    fn write_depth1_inode(img: &mut [u8], off: usize, mode: u16, size: u32, child_block: u32) {
        put_u16(img, off, mode);
        put_u32(img, off + 4, size);
        put_u32(img, off + 32, EXT4_EXTENTS_FL);
        put_u16(img, off + 40, 0xf30a);
        put_u16(img, off + 42, 1);
        put_u16(img, off + 44, 4);
        put_u16(img, off + 46, 1);
        put_u32(img, off + 52, 0);
        put_u32(img, off + 56, child_block);
        put_u16(img, off + 60, 0);
    }

    fn write_leaf_extent_block(dst: &mut [u8], logical: u32, len: u16, start: u32) {
        put_u16(dst, 0, 0xf30a);
        put_u16(dst, 2, 1);
        put_u16(dst, 4, 84);
        put_u16(dst, 6, 0);
        put_u32(dst, 12, logical);
        put_u16(dst, 16, len);
        put_u16(dst, 18, 0);
        put_u32(dst, 20, start);
    }

    fn write_indirect_inode(
        img: &mut [u8],
        off: usize,
        mode: u16,
        size: u32,
        direct: &[u32],
        single: u32,
        double: u32,
        triple: u32,
    ) {
        put_u16(img, off, mode);
        put_u32(img, off + 4, size);
        for (idx, ptr) in direct.iter().copied().enumerate().take(12) {
            put_u32(img, off + 40 + idx * 4, ptr);
        }
        put_u32(img, off + 40 + 12 * 4, single);
        put_u32(img, off + 40 + 13 * 4, double);
        put_u32(img, off + 40 + 14 * 4, triple);
    }

    fn write_inline_symlink_inode(img: &mut [u8], off: usize, target: &[u8]) {
        put_u16(img, off, 0xa000);
        put_u32(img, off + 4, target.len() as u32);
        img[off + 40..off + 40 + target.len()].copy_from_slice(target);
    }

    fn long_symlink_target() -> alloc::vec::Vec<u8> {
        b"/this/is/a/long/external/symlink/target/that/exceeds/sixty/bytes".to_vec()
    }

    fn write_htree_root_block(dst: &mut [u8], leaf_logical: u32) {
        write_dirent(&mut dst[0..12], 22, b".", 2, 12);
        write_dirent(&mut dst[12..24], 2, b"..", 2, 12);
        put_u32(dst, EXT4_DX_ROOT_INFO_OFFSET, 0);
        dst[EXT4_DX_ROOT_INFO_OFFSET + 4] = 0;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 5] = 8;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 6] = 0;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 7] = 0;
        put_u16(dst, EXT4_DX_ROOT_ENTRIES_OFFSET, 123);
        put_u16(dst, EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 1);
        put_u32(dst, EXT4_DX_ROOT_ENTRIES_OFFSET + 4, leaf_logical);
    }

    fn write_dirent(dst: &mut [u8], inode: u32, name: &[u8], file_type: u8, rec_len: u16) {
        put_u32(dst, 0, inode);
        put_u16(dst, 4, rec_len);
        dst[6] = name.len() as u8;
        dst[7] = file_type;
        dst[8..8 + name.len()].copy_from_slice(name);
    }

    fn put_u16(dst: &mut [u8], off: usize, value: u16) {
        dst[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(dst: &mut [u8], off: usize, value: u32) {
        dst[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }
}
