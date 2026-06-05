// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::fs::{FdRecord, ServiceFsBackend, MAX_SERVICE_FDS, MAX_SERVICE_INODES};
use super::super::common::vfs_ipc::{VfsBackend, VfsError};

use super::dir::find_inode_index;
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
            cache: BlockCache::new(),
        };
        backend.seed_path(EXT4_DEMO_PATH_PTR, EXT4_DEMO_PATH);
        backend.seed_path(EXT4_SERVICE_PATH_PTR, EXT4_SERVICE_PATH);
        backend.seed_path(EXT4_OVERSIZE_PATH_PTR, EXT4_OVERSIZE_PATH);
        backend
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

    fn write(&mut self, fd: u64, _len: u64) -> Result<u64, VfsError> {
        self.inode_for_fd(fd).ok_or(VfsError::BadFd)?;
        Err(VfsError::Unsupported)
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
const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
const EXT4_FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;
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
    | EXT4_FEATURE_INCOMPAT_FLEX_BG
    | EXT4_FEATURE_INCOMPAT_CSUM_SEED;
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
const EXT4_DX_NODE_ENTRIES_OFFSET: usize = 8;
const EXT4_DX_MAX_INDIRECT_LEVELS: u8 = 2;
const EXT4_SUPERBLOCK_CHECKSUM_OFFSET: usize = 1020;
const EXT4_INODE_CHECKSUM_LO_OFFSET: usize = 124;
const EXT4_INODE_EXTRA_ISIZE_OFFSET: usize = 128;
const EXT4_INODE_CHECKSUM_HI_OFFSET: usize = 130;
const EXT4_DIR_TAIL_SIZE: usize = 12;
const EXT4_DX_TAIL_SIZE: usize = 8;
const EXT4_CHECKSUM_TYPE_CRC32C: u8 = 1;

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
    ChecksumMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Superblock {
    pub inodes_count: u32,
    pub blocks_count: u64,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub inode_size: u16,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub uuid: [u8; 16],
    pub checksum_seed: u32,
    pub metadata_csum: bool,
    pub hash_seed: [u32; 4],
    pub default_hash_version: u8,
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
        let metadata_csum_seed = (feature_incompat & EXT4_FEATURE_INCOMPAT_CSUM_SEED) != 0;
        if (feature_incompat & EXT4_FEATURE_INCOMPAT_META_BG) != 0 {
            // meta_bg relocates descriptor blocks outside the contiguous primary table.
            // Reject it before generic feature masking so the failure is stable and explicit.
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_INCOMPAT_META_BG,
            ));
        }
        let unsupported = feature_incompat & !EXT4_SUPPORTED_INCOMPAT;
        if unsupported != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(unsupported));
        }
        if (feature_incompat & EXT4_FEATURE_INCOMPAT_INLINE_DATA) != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_INCOMPAT_INLINE_DATA,
            ));
        }
        let metadata_csum = (feature_ro_compat & EXT4_FEATURE_RO_COMPAT_METADATA_CSUM) != 0;
        if metadata_csum_seed && !metadata_csum {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_INCOMPAT_CSUM_SEED,
            ));
        }
        let supported_ro_for_parse =
            EXT4_SUPPORTED_RO_COMPAT | EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
        let unsupported_ro = feature_ro_compat & !supported_ro_for_parse;
        if unsupported_ro != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(unsupported_ro));
        }
        if (feature_ro_compat & EXT4_FEATURE_RO_COMPAT_BIGALLOC) != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_BIGALLOC,
            ));
        }
        let blocks_lo = le_u32(sb, 4)? as u64;
        let blocks_hi = if (feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT) != 0 {
            le_u32(sb, 336).unwrap_or(0) as u64
        } else {
            0
        };
        let inode_size = le_u16(sb, 88).unwrap_or(128);
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(sb.get(104..120).ok_or(Ext4ImageError::Io)?);
        if metadata_csum {
            if sb.get(373).copied() != Some(EXT4_CHECKSUM_TYPE_CRC32C) {
                return Err(Ext4ImageError::UnsupportedFeature(
                    EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
                ));
            }
            validate_superblock_checksum(sb)?;
        }
        let checksum_seed = if metadata_csum_seed {
            le_u32(sb, 0x270)?
        } else {
            crc32c_update(!0, &uuid)
        };
        Ok(Self {
            inodes_count: le_u32(sb, 0)?,
            blocks_count: blocks_lo | (blocks_hi << 32),
            first_data_block: le_u32(sb, 20)?,
            log_block_size: le_u32(sb, 24)?,
            blocks_per_group: le_u32(sb, 32)?,
            inodes_per_group: le_u32(sb, 40)?,
            inode_size: if inode_size == 0 { 128 } else { inode_size },
            feature_compat,
            feature_incompat,
            feature_ro_compat,
            uuid,
            checksum_seed,
            metadata_csum,
            hash_seed: [
                le_u32(sb, 236)?,
                le_u32(sb, 240)?,
                le_u32(sb, 244)?,
                le_u32(sb, 248)?,
            ],
            default_hash_version: *sb.get(252).ok_or(Ext4ImageError::Io)?,
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
    len: u32,
    start: u64,
    initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedInode {
    number: u32,
    generation: u32,
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
            || !sb.inode_size.is_power_of_two()
            || u64::from(sb.inode_size) > sb.block_size()
            || (sb.block_size() == 1024 && sb.first_data_block != 1)
            || (sb.block_size() > 1024 && sb.first_data_block != 0)
            || u64::from(sb.first_data_block) >= sb.blocks_count
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
        let data_blocks = self
            .sb
            .blocks_count
            .checked_sub(u64::from(self.sb.first_data_block))
            .ok_or(Ext4ImageError::Malformed)?;
        let groups = data_blocks
            .checked_add(u64::from(self.sb.blocks_per_group) - 1)
            .ok_or(Ext4ImageError::UnsupportedLayout)?
            / u64::from(self.sb.blocks_per_group);
        let groups = u32::try_from(groups).map_err(|_| Ext4ImageError::UnsupportedLayout)?;
        let inode_capacity = u64::from(groups)
            .checked_mul(u64::from(self.sb.inodes_per_group))
            .ok_or(Ext4ImageError::UnsupportedLayout)?;
        if groups == 0 || u64::from(self.sb.inodes_count) > inode_capacity {
            return Err(Ext4ImageError::Malformed);
        }
        Ok(groups)
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
        if self.sb.metadata_csum {
            for group in 0..self.group_count()? {
                self.group_desc(group)?;
            }
        }
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
            .checked_add(
                (group as usize)
                    .checked_mul(self.desc_size as usize)
                    .ok_or(Ext4ImageError::Io)?,
            )
            .ok_or(Ext4ImageError::Io)?;
        let end = start
            .checked_add(self.desc_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        let descriptor = self.image.get(start..end).ok_or(Ext4ImageError::Io)?;
        if self.sb.metadata_csum {
            validate_group_descriptor_checksum(self.sb.checksum_seed, group, descriptor)?;
        }
        Ok(descriptor)
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
        let table_bytes = u64::from(self.sb.inodes_per_group)
            .checked_mul(u64::from(self.sb.inode_size))
            .ok_or(Ext4ImageError::Io)?;
        let table_blocks = table_bytes.div_ceil(self.sb.block_size());
        if block
            .checked_add(table_blocks)
            .map(|end| end > self.sb.blocks_count)
            .unwrap_or(true)
        {
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
        if self.sb.metadata_csum {
            validate_inode_checksum(self.sb.checksum_seed, inode, raw)?;
        }
        let size_lo = le_u32(raw, 4)? as u64;
        let size_hi = le_u32(raw, 108).unwrap_or(0) as u64;
        let mut block = [0u8; 60];
        block.copy_from_slice(raw.get(40..100).ok_or(Ext4ImageError::Malformed)?);
        Ok(ParsedInode {
            number: inode,
            generation: le_u32(raw, 100)?,
            mode: le_u16(raw, 0)?,
            size: size_lo | (size_hi << 32),
            flags: le_u32(raw, 32)?,
            block,
        })
    }

    fn extents(&self, inode: &ParsedInode) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        if (inode.flags & EXT4_EXTENTS_FL) != 0 || inode.block[0..2] == 0xf30au16.to_le_bytes() {
            self.parse_extent_tree(inode, &inode.block, 0, false)
        } else if self.sb.metadata_csum {
            // Legacy indirect metadata blocks have no metadata_csum checksum format.
            // Keep accepted metadata_csum mounts limited to extent-backed objects.
            Err(Ext4ImageError::UnsupportedLayout)
        } else {
            self.indirect_extents(inode)
        }
    }

    fn indirect_extents(
        &self,
        inode: &ParsedInode,
    ) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        let block_size = self.sb.block_size() as usize;
        let ptrs_per_block = block_size / 4;
        if ptrs_per_block == 0 {
            return Err(Ext4ImageError::Malformed);
        }
        let blocks_needed = usize::try_from(inode.size.div_ceil(self.sb.block_size()))
            .map_err(|_| Ext4ImageError::UnsupportedLayout)?;
        let double_capacity = ptrs_per_block
            .checked_mul(ptrs_per_block)
            .ok_or(Ext4ImageError::UnsupportedLayout)?;
        let supported_blocks = EXT4_NDIR_BLOCKS
            .checked_add(ptrs_per_block)
            .and_then(|value| value.checked_add(double_capacity))
            .ok_or(Ext4ImageError::UnsupportedLayout)?;
        if blocks_needed > supported_blocks {
            return Err(Ext4ImageError::UnsupportedLayout);
        }

        let mut out = alloc::vec::Vec::new();
        for logical in 0..core::cmp::min(blocks_needed, EXT4_NDIR_BLOCKS) {
            self.push_indirect_extent(&mut out, logical, le_u32(&inode.block, logical * 4)?)?;
        }

        if blocks_needed > EXT4_NDIR_BLOCKS {
            let single = le_u32(&inode.block, EXT4_NDIR_BLOCKS * 4)?;
            let single_count = core::cmp::min(blocks_needed - EXT4_NDIR_BLOCKS, ptrs_per_block);
            if single != 0 {
                let raw = self.block_bytes(u64::from(single))?;
                for idx in 0..single_count {
                    self.push_indirect_extent(
                        &mut out,
                        EXT4_NDIR_BLOCKS + idx,
                        le_u32(raw, idx * 4)?,
                    )?;
                }
            }
        }

        let double_start = EXT4_NDIR_BLOCKS + ptrs_per_block;
        if blocks_needed > double_start {
            let double = le_u32(&inode.block, (EXT4_NDIR_BLOCKS + 1) * 4)?;
            if double != 0 {
                let outer = self.block_bytes(u64::from(double))?;
                let double_count = blocks_needed - double_start;
                let outer_count = double_count.div_ceil(ptrs_per_block);
                for outer_idx in 0..outer_count {
                    let inner_ptr = le_u32(outer, outer_idx * 4)?;
                    if inner_ptr == 0 {
                        continue;
                    }
                    let inner = self.block_bytes(u64::from(inner_ptr))?;
                    let inner_count =
                        core::cmp::min(ptrs_per_block, double_count - outer_idx * ptrs_per_block);
                    for inner_idx in 0..inner_count {
                        let logical = double_start + outer_idx * ptrs_per_block + inner_idx;
                        self.push_indirect_extent(
                            &mut out,
                            logical,
                            le_u32(inner, inner_idx * 4)?,
                        )?;
                    }
                }
            }
        }
        Ok(out)
    }

    fn push_indirect_extent(
        &self,
        out: &mut alloc::vec::Vec<Extent>,
        logical: usize,
        pointer: u32,
    ) -> Result<(), Ext4ImageError> {
        if pointer == 0 {
            return Ok(());
        }
        if u64::from(pointer) >= self.sb.blocks_count {
            return Err(Ext4ImageError::Io);
        }
        out.push(Extent {
            logical: u32::try_from(logical).map_err(|_| Ext4ImageError::UnsupportedLayout)?,
            len: 1,
            start: u64::from(pointer),
            initialized: true,
        });
        Ok(())
    }

    fn parse_extent_tree(
        &self,
        inode: &ParsedInode,
        raw: &[u8],
        recursion_depth: u16,
        external: bool,
    ) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        let header_depth = extent_header_depth(raw)?;
        if external && self.sb.metadata_csum {
            validate_extent_block_checksum(
                self.sb.checksum_seed,
                inode.number,
                inode.generation,
                raw,
            )?;
        }
        if header_depth > EXT4_MAX_EXTENT_DEPTH || recursion_depth > EXT4_MAX_EXTENT_DEPTH {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        if header_depth == 0 {
            return parse_extent_leaf(raw);
        }
        let entries = le_u16(raw, 2)? as usize;
        let max_entries = le_u16(raw, 4)? as usize;
        let capacity = raw.len().saturating_sub(12) / 12;
        if entries > max_entries || max_entries > capacity {
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
            out.extend(self.parse_extent_tree(inode, child, recursion_depth + 1, true)?);
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
            let extents = self.extents(&inode)?;
            let output_len =
                usize::try_from(inode.size).map_err(|_| Ext4ImageError::UnsupportedLayout)?;
            let mut out = alloc::vec![0u8; output_len];
            for ex in extents {
                if ex
                    .start
                    .checked_add(u64::from(ex.len))
                    .map(|end| end > self.sb.blocks_count)
                    .unwrap_or(true)
                {
                    return Err(Ext4ImageError::Io);
                }
                if !ex.initialized {
                    continue;
                }
                let src = self.block_offset(ex.start)?;
                let dst = (ex.logical as usize)
                    .checked_mul(self.sb.block_size() as usize)
                    .ok_or(Ext4ImageError::UnsupportedLayout)?;
                if dst >= out.len() {
                    continue;
                }
                let len = core::cmp::min(
                    usize::try_from(ex.len)
                        .map_err(|_| Ext4ImageError::UnsupportedLayout)?
                        .checked_mul(self.sb.block_size() as usize)
                        .ok_or(Ext4ImageError::UnsupportedLayout)?,
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
        let indexed = (meta.flags & EXT4_INDEX_FL) != 0
            && (self.sb.feature_compat & EXT4_FEATURE_COMPAT_DIR_INDEX) != 0;
        let bytes = self.read_inode_bytes(inode)?;
        if indexed {
            return self.read_htree_dir(&meta, &bytes);
        }
        if self.sb.metadata_csum {
            self.validate_linear_directory_blocks(&meta, &bytes)?;
        }
        parse_dir_entries(bytes.as_slice(), self.sb.block_size() as usize)
    }

    fn read_htree_dir(
        &self,
        dir_inode: &ParsedInode,
        bytes: &[u8],
    ) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
        let block_size = self.sb.block_size() as usize;
        let root = bytes.get(..block_size).ok_or(Ext4ImageError::Malformed)?;
        let mut out = parse_htree_root_dot_entries(root, block_size)?;
        for logical in self.htree_leaf_logicals(dir_inode, bytes)? {
            let leaf = directory_logical_block(bytes, block_size, logical)?;
            if self.sb.metadata_csum {
                validate_directory_block_checksum(
                    self.sb.checksum_seed,
                    dir_inode.number,
                    dir_inode.generation,
                    leaf,
                )?;
            }
            for entry in parse_dir_entries(leaf, block_size)? {
                if !out.iter().any(|existing| {
                    existing.inode == entry.inode && existing.name() == entry.name()
                }) {
                    out.push(entry);
                }
            }
        }
        Ok(out)
    }

    fn htree_leaf_logicals(
        &self,
        dir_inode: &ParsedInode,
        bytes: &[u8],
    ) -> Result<alloc::vec::Vec<u32>, Ext4ImageError> {
        let block_size = self.sb.block_size() as usize;
        let root = bytes.get(..block_size).ok_or(Ext4ImageError::Malformed)?;
        if block_size < EXT4_DX_ROOT_ENTRIES_OFFSET + 8
            || le_u32(root, EXT4_DX_ROOT_INFO_OFFSET)? != 0
        {
            return Err(Ext4ImageError::Malformed);
        }
        let info_len = *root
            .get(EXT4_DX_ROOT_INFO_OFFSET + 5)
            .ok_or(Ext4ImageError::Malformed)? as usize;
        let indirect_levels = *root
            .get(EXT4_DX_ROOT_INFO_OFFSET + 6)
            .ok_or(Ext4ImageError::Malformed)?;
        if info_len != 8 || indirect_levels > EXT4_DX_MAX_INDIRECT_LEVELS {
            return Err(Ext4ImageError::UnsupportedLayout);
        }
        if self.sb.metadata_csum {
            validate_dx_block_checksum(
                self.sb.checksum_seed,
                dir_inode.number,
                dir_inode.generation,
                root,
                EXT4_DX_ROOT_ENTRIES_OFFSET,
            )?;
        }
        let root_entries = parse_dx_entries(
            root,
            EXT4_DX_ROOT_ENTRIES_OFFSET,
            block_size,
            self.sb.metadata_csum,
        )?;
        let mut candidates = alloc::vec::Vec::new();
        for (_, logical) in root_entries {
            push_unique_logical(&mut candidates, logical);
        }
        for _ in 0..indirect_levels {
            let mut children = alloc::vec::Vec::new();
            for logical in candidates {
                let node = directory_logical_block(bytes, block_size, logical)?;
                validate_dx_node_header(node, block_size)?;
                if self.sb.metadata_csum {
                    validate_dx_block_checksum(
                        self.sb.checksum_seed,
                        dir_inode.number,
                        dir_inode.generation,
                        node,
                        EXT4_DX_NODE_ENTRIES_OFFSET,
                    )?;
                }
                for (_, child) in parse_dx_entries(
                    node,
                    EXT4_DX_NODE_ENTRIES_OFFSET,
                    block_size,
                    self.sb.metadata_csum,
                )? {
                    push_unique_logical(&mut children, child);
                }
            }
            candidates = children;
        }
        for logical in &candidates {
            directory_logical_block(bytes, block_size, *logical)?;
        }
        Ok(candidates)
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
        let bytes = self.read_inode_bytes(dir_inode)?;
        if self.sb.metadata_csum {
            self.validate_linear_directory_blocks(&inode, &bytes)?;
        }
        parse_dir_entries(bytes.as_slice(), self.sb.block_size() as usize)?
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
        let root = bytes.get(..block_size).ok_or(Ext4ImageError::Malformed)?;
        if block_size < EXT4_DX_ROOT_ENTRIES_OFFSET + 8
            || le_u32(root, EXT4_DX_ROOT_INFO_OFFSET)? != 0
        {
            return Err(Ext4ImageError::Malformed);
        }
        let hash_version = *root
            .get(EXT4_DX_ROOT_INFO_OFFSET + 4)
            .ok_or(Ext4ImageError::Malformed)?;
        let info_len = *root
            .get(EXT4_DX_ROOT_INFO_OFFSET + 5)
            .ok_or(Ext4ImageError::Malformed)? as usize;
        let indirect_levels = *root
            .get(EXT4_DX_ROOT_INFO_OFFSET + 6)
            .ok_or(Ext4ImageError::Malformed)?;
        if info_len != 8 || indirect_levels > EXT4_DX_MAX_INDIRECT_LEVELS {
            return Err(Ext4ImageError::UnsupportedLayout);
        }

        if self.sb.metadata_csum {
            validate_dx_block_checksum(
                self.sb.checksum_seed,
                dir_inode.number,
                dir_inode.generation,
                root,
                EXT4_DX_ROOT_ENTRIES_OFFSET,
            )?;
        }
        let hash = ext4_dir_hash(hash_version, name, self.sb.hash_seed);
        let root_entries = parse_dx_entries(
            root,
            EXT4_DX_ROOT_ENTRIES_OFFSET,
            block_size,
            self.sb.metadata_csum,
        )?;
        let mut candidates = alloc::vec::Vec::new();
        for idx in dx_candidates(root_entries.as_slice(), hash) {
            candidates.push(root_entries[idx].1);
        }

        for _ in 0..indirect_levels {
            let mut children = alloc::vec::Vec::new();
            for logical in candidates {
                let node = directory_logical_block(&bytes, block_size, logical)?;
                validate_dx_node_header(node, block_size)?;
                if self.sb.metadata_csum {
                    validate_dx_block_checksum(
                        self.sb.checksum_seed,
                        dir_inode.number,
                        dir_inode.generation,
                        node,
                        EXT4_DX_NODE_ENTRIES_OFFSET,
                    )?;
                }
                let entries = parse_dx_entries(
                    node,
                    EXT4_DX_NODE_ENTRIES_OFFSET,
                    block_size,
                    self.sb.metadata_csum,
                )?;
                for idx in dx_candidates(entries.as_slice(), hash) {
                    children.push(entries[idx].1);
                }
            }
            candidates = children;
        }

        for logical in candidates {
            if let Some(inode) = self.scan_htree_leaf(dir_inode, &bytes, logical, name)? {
                return Ok(Some(inode));
            }
        }
        Ok(None)
    }

    fn scan_htree_leaf(
        &self,
        dir_inode: &ParsedInode,
        directory: &[u8],
        logical: u32,
        name: &[u8],
    ) -> Result<Option<u32>, Ext4ImageError> {
        let leaf = directory_logical_block(directory, self.sb.block_size() as usize, logical)?;
        if self.sb.metadata_csum {
            validate_directory_block_checksum(
                self.sb.checksum_seed,
                dir_inode.number,
                dir_inode.generation,
                leaf,
            )?;
        }
        for entry in parse_dir_entries(leaf, self.sb.block_size() as usize)? {
            if entry.name() == name {
                return Ok(Some(entry.inode));
            }
        }
        Ok(None)
    }

    fn validate_linear_directory_blocks(
        &self,
        inode: &ParsedInode,
        bytes: &[u8],
    ) -> Result<(), Ext4ImageError> {
        let block_size = self.sb.block_size() as usize;
        for block in bytes.chunks(block_size) {
            if block.len() != block_size {
                return Err(Ext4ImageError::Malformed);
            }
            validate_directory_block_checksum(
                self.sb.checksum_seed,
                inode.number,
                inode.generation,
                block,
            )?;
        }
        Ok(())
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

fn parse_htree_root_dot_entries(
    root: &[u8],
    block_size: usize,
) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
    if root.len() < block_size || EXT4_DX_ROOT_INFO_OFFSET > block_size {
        return Err(Ext4ImageError::Malformed);
    }
    let mut out = alloc::vec::Vec::new();
    for (off, expected_name) in [(0usize, b".".as_slice()), (12usize, b"..".as_slice())] {
        let inode = le_u32(root, off)?;
        let rec_len = le_u16(root, off + 4)? as usize;
        let name_len = *root.get(off + 6).ok_or(Ext4ImageError::Malformed)? as usize;
        if inode == 0
            || rec_len < 8 + name_len
            || off
                .checked_add(rec_len)
                .map(|end| end > block_size)
                .unwrap_or(true)
            || root.get(off + 8..off + 8 + name_len) != Some(expected_name)
        {
            return Err(Ext4ImageError::Malformed);
        }
        let mut name = [0u8; 255];
        name[..name_len].copy_from_slice(expected_name);
        out.push(Ext4DirEntry {
            inode,
            file_type: Ext4FileType::Directory,
            name_len: name_len as u8,
            name,
        });
    }
    Ok(out)
}

fn push_unique_logical(blocks: &mut alloc::vec::Vec<u32>, logical: u32) {
    if !blocks.contains(&logical) {
        blocks.push(logical);
    }
}

fn parse_dx_entries(
    block: &[u8],
    entries_offset: usize,
    block_size: usize,
    metadata_csum: bool,
) -> Result<alloc::vec::Vec<(u32, u32)>, Ext4ImageError> {
    let limit = le_u16(block, entries_offset)? as usize;
    let count = le_u16(block, entries_offset + 2)? as usize;
    let tail_size = if metadata_csum { EXT4_DX_TAIL_SIZE } else { 0 };
    let capacity = block_size
        .checked_sub(entries_offset)
        .and_then(|bytes| bytes.checked_sub(tail_size))
        .ok_or(Ext4ImageError::Malformed)?
        / 8;
    if limit == 0 || count == 0 || count > limit || limit > capacity {
        return Err(Ext4ImageError::Malformed);
    }
    let end = entries_offset
        .checked_add(count.checked_mul(8).ok_or(Ext4ImageError::Io)?)
        .ok_or(Ext4ImageError::Io)?;
    if end > block.len() || end > block_size {
        return Err(Ext4ImageError::Malformed);
    }

    let mut entries = alloc::vec::Vec::with_capacity(count);
    let first_block = le_u32(block, entries_offset + 4)?;
    entries.push((0, first_block));
    let mut last_hash = 0u32;
    for idx in 1..count {
        let off = entries_offset + idx * 8;
        let hash = le_u32(block, off)?;
        if (hash & !1) < (last_hash & !1) {
            return Err(Ext4ImageError::Malformed);
        }
        entries.push((hash, le_u32(block, off + 4)?));
        last_hash = hash;
    }
    Ok(entries)
}

fn dx_candidates(entries: &[(u32, u32)], hash: Option<u32>) -> alloc::vec::Vec<usize> {
    let Some(hash) = hash else {
        return (0..entries.len()).collect();
    };
    let target = hash & !1;
    let mut selected = 0usize;
    for idx in 1..entries.len() {
        // Compare the raw stored hash. A continuation entry uses target|1 and
        // must remain after the primary target range rather than replacing it.
        if entries[idx].0 > target {
            break;
        }
        selected = idx;
    }
    let mut out = alloc::vec![selected];
    let mut idx = selected + 1;
    while idx < entries.len() && (entries[idx].0 & 1) != 0 {
        out.push(idx);
        idx += 1;
    }
    out
}

fn directory_logical_block<'a>(
    directory: &'a [u8],
    block_size: usize,
    logical: u32,
) -> Result<&'a [u8], Ext4ImageError> {
    if logical == 0 {
        return Err(Ext4ImageError::Malformed);
    }
    let start = usize::try_from(logical)
        .map_err(|_| Ext4ImageError::Io)?
        .checked_mul(block_size)
        .ok_or(Ext4ImageError::Io)?;
    let end = start.checked_add(block_size).ok_or(Ext4ImageError::Io)?;
    directory.get(start..end).ok_or(Ext4ImageError::Io)
}

fn validate_dx_node_header(block: &[u8], block_size: usize) -> Result<(), Ext4ImageError> {
    if le_u32(block, 0)? != 0
        || le_u16(block, 4)? as usize != block_size
        || block.get(6).copied() != Some(0)
        || block.get(7).copied() != Some(0)
    {
        return Err(Ext4ImageError::Malformed);
    }
    Ok(())
}

fn ext4_dir_hash(version: u8, name: &[u8], seed: [u32; 4]) -> Option<u32> {
    let mut state = if seed.iter().any(|word| *word != 0) {
        seed
    } else {
        [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476]
    };
    let hash = match version {
        0 => legacy_dir_hash(name, true),
        1 | 4 => {
            let signed = version == 1;
            let mut offset = 0usize;
            while offset < name.len() {
                let input = str2hashbuf(&name[offset..], name.len() - offset, 8, signed);
                half_md4_transform(&mut state, &input);
                offset += 32;
            }
            state[1]
        }
        2 | 5 => {
            let signed = version == 2;
            let mut offset = 0usize;
            while offset < name.len() {
                let input = str2hashbuf(&name[offset..], name.len() - offset, 4, signed);
                tea_transform(&mut state, &input);
                offset += 16;
            }
            state[0]
        }
        3 => legacy_dir_hash(name, false),
        // SipHash requires the encrypted-directory key and therefore cannot be
        // reproduced from the on-disk hash seed alone. Unknown versions retain
        // the validated exhaustive-leaf fallback.
        6 | _ => return None,
    };
    let hash = hash & !1;
    Some(if hash == 0xffff_fffe {
        0xffff_fffc
    } else {
        hash
    })
}

fn legacy_dir_hash(name: &[u8], signed: bool) -> u32 {
    let mut hash0 = 0x12a3_fe2du32;
    let mut hash1 = 0x37ab_e8f9u32;
    for byte in name {
        let value = hash_byte(*byte, signed);
        let mut hash = hash1.wrapping_add(hash0 ^ value.wrapping_mul(7_152_373));
        if hash & 0x8000_0000 != 0 {
            hash = hash.wrapping_sub(0x7fff_ffff);
        }
        hash1 = hash0;
        hash0 = hash;
    }
    hash0 << 1
}

fn hash_byte(byte: u8, signed: bool) -> u32 {
    if signed {
        i32::from(byte as i8) as u32
    } else {
        u32::from(byte)
    }
}

fn str2hashbuf(message: &[u8], remaining_len: usize, words: usize, signed: bool) -> [u32; 8] {
    let len = remaining_len;
    let mut output = [0u32; 8];
    let mut pad = len as u32 | ((len as u32) << 8);
    pad |= pad << 16;
    output[..words].fill(pad);

    let mut value = pad;
    let capped = core::cmp::min(len, words * 4);
    let mut word = 0usize;
    for (idx, byte) in message
        .get(..capped)
        .unwrap_or(message)
        .iter()
        .copied()
        .enumerate()
    {
        value = hash_byte(byte, signed).wrapping_add(value << 8);
        if idx % 4 == 3 {
            output[word] = value;
            word += 1;
            value = pad;
        }
    }
    if capped % 4 != 0 && word < words {
        output[word] = value;
    }
    output
}

fn tea_transform(state: &mut [u32; 4], input: &[u32; 8]) {
    let mut sum = 0u32;
    let mut b0 = state[0];
    let mut b1 = state[1];
    for _ in 0..16 {
        sum = sum.wrapping_add(0x9e37_79b9);
        b0 = b0.wrapping_add(
            ((b1 << 4).wrapping_add(input[0]))
                ^ b1.wrapping_add(sum)
                ^ ((b1 >> 5).wrapping_add(input[1])),
        );
        b1 = b1.wrapping_add(
            ((b0 << 4).wrapping_add(input[2]))
                ^ b0.wrapping_add(sum)
                ^ ((b0 >> 5).wrapping_add(input[3])),
        );
    }
    state[0] = state[0].wrapping_add(b0);
    state[1] = state[1].wrapping_add(b1);
}

fn half_md4_transform(state: &mut [u32; 4], input: &[u32; 8]) {
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];

    macro_rules! round {
        ($func:expr, $target:ident, $x:ident, $y:ident, $z:ident, $word:expr, $shift:expr) => {{
            $target = $target
                .wrapping_add($func($x, $y, $z))
                .wrapping_add($word)
                .rotate_left($shift);
        }};
    }
    let f = |x: u32, y: u32, z: u32| z ^ (x & (y ^ z));
    let g = |x: u32, y: u32, z: u32| (x & y).wrapping_add((x ^ y) & z);
    let h = |x: u32, y: u32, z: u32| x ^ y ^ z;

    round!(f, a, b, c, d, input[0], 3);
    round!(f, d, a, b, c, input[1], 7);
    round!(f, c, d, a, b, input[2], 11);
    round!(f, b, c, d, a, input[3], 19);
    round!(f, a, b, c, d, input[4], 3);
    round!(f, d, a, b, c, input[5], 7);
    round!(f, c, d, a, b, input[6], 11);
    round!(f, b, c, d, a, input[7], 19);

    const K2: u32 = 0x5a82_7999;
    round!(g, a, b, c, d, input[1].wrapping_add(K2), 3);
    round!(g, d, a, b, c, input[3].wrapping_add(K2), 5);
    round!(g, c, d, a, b, input[5].wrapping_add(K2), 9);
    round!(g, b, c, d, a, input[7].wrapping_add(K2), 13);
    round!(g, a, b, c, d, input[0].wrapping_add(K2), 3);
    round!(g, d, a, b, c, input[2].wrapping_add(K2), 5);
    round!(g, c, d, a, b, input[4].wrapping_add(K2), 9);
    round!(g, b, c, d, a, input[6].wrapping_add(K2), 13);

    const K3: u32 = 0x6ed9_eba1;
    round!(h, a, b, c, d, input[3].wrapping_add(K3), 3);
    round!(h, d, a, b, c, input[7].wrapping_add(K3), 9);
    round!(h, c, d, a, b, input[2].wrapping_add(K3), 11);
    round!(h, b, c, d, a, input[6].wrapping_add(K3), 15);
    round!(h, a, b, c, d, input[1].wrapping_add(K3), 3);
    round!(h, d, a, b, c, input[5].wrapping_add(K3), 9);
    round!(h, c, d, a, b, input[0].wrapping_add(K3), 11);
    round!(h, b, c, d, a, input[4].wrapping_add(K3), 15);

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
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
    let capacity = raw.len().saturating_sub(12) / 12;
    if entries > max_entries || max_entries > capacity {
        return Err(Ext4ImageError::Malformed);
    }
    let mut out = alloc::vec::Vec::new();
    let mut previous_end = None;
    for idx in 0..entries {
        let off = 12 + idx * 12;
        let logical = le_u32(raw, off)?;
        let encoded_len = le_u16(raw, off + 4)?;
        if encoded_len == 0 {
            return Err(Ext4ImageError::Malformed);
        }
        let (len, initialized) = if encoded_len <= 0x8000 {
            (u32::from(encoded_len), true)
        } else {
            (u32::from(encoded_len - 0x8000), false)
        };
        let end = logical.checked_add(len).ok_or(Ext4ImageError::Malformed)?;
        if previous_end.map(|prior| logical < prior).unwrap_or(false) {
            return Err(Ext4ImageError::Malformed);
        }
        previous_end = Some(end);
        let start_hi = le_u16(raw, off + 6)? as u64;
        let start_lo = le_u32(raw, off + 8)? as u64;
        out.push(Extent {
            logical,
            len,
            start: (start_hi << 32) | start_lo,
            initialized,
        });
    }
    Ok(out)
}

fn parse_dir_entries(
    bytes: &[u8],
    block_size: usize,
) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
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
        if block_size == 0
            || rec_len < 8
            || rec_len % 4 != 0
            || (off % block_size)
                .checked_add(rec_len)
                .map(|end| end > block_size)
                .unwrap_or(true)
            || off
                .checked_add(rec_len)
                .map(|end| end > bytes.len())
                .unwrap_or(true)
            || name_len as usize > rec_len - 8
        {
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
    if off != bytes.len() {
        return Err(Ext4ImageError::Malformed);
    }
    Ok(out)
}

fn crc32c_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

#[cfg(test)]
fn crc32c(bytes: &[u8]) -> u32 {
    !crc32c_update(!0, bytes)
}

fn validate_superblock_checksum(sb: &[u8]) -> Result<(), Ext4ImageError> {
    let stored = le_u32(sb, EXT4_SUPERBLOCK_CHECKSUM_OFFSET)?;
    let calculated = crc32c_update(
        !0,
        sb.get(..EXT4_SUPERBLOCK_CHECKSUM_OFFSET)
            .ok_or(Ext4ImageError::Io)?,
    );
    checksum_matches(stored, calculated)
}

fn validate_group_descriptor_checksum(
    seed: u32,
    group: u32,
    descriptor: &[u8],
) -> Result<(), Ext4ImageError> {
    let stored = le_u16(descriptor, 30)?;
    let mut calculated = crc32c_update(seed, &group.to_le_bytes());
    calculated = crc32c_update(
        calculated,
        descriptor.get(..30).ok_or(Ext4ImageError::Malformed)?,
    );
    calculated = crc32c_update(calculated, &[0, 0]);
    calculated = crc32c_update(
        calculated,
        descriptor.get(32..).ok_or(Ext4ImageError::Malformed)?,
    );
    checksum_matches(u32::from(stored), calculated & 0xffff)
}

fn inode_has_high_checksum(raw: &[u8]) -> Result<bool, Ext4ImageError> {
    if raw.len() <= EXT4_INODE_EXTRA_ISIZE_OFFSET {
        return Ok(false);
    }
    let extra_isize = le_u16(raw, EXT4_INODE_EXTRA_ISIZE_OFFSET)? as usize;
    Ok(extra_isize >= 4 && raw.len() >= EXT4_INODE_CHECKSUM_HI_OFFSET + 2)
}

fn validate_inode_checksum(seed: u32, inode: u32, raw: &[u8]) -> Result<(), Ext4ImageError> {
    if raw.len() < 128 {
        return Err(Ext4ImageError::Malformed);
    }
    let generation = le_u32(raw, 100)?;
    let stored_lo = le_u16(raw, EXT4_INODE_CHECKSUM_LO_OFFSET)?;
    let has_high = inode_has_high_checksum(raw)?;
    let stored_hi = if has_high {
        le_u16(raw, EXT4_INODE_CHECKSUM_HI_OFFSET)?
    } else {
        0
    };
    let mut calculated = metadata_checksum_prefix(seed, inode, generation);
    calculated = crc32c_update(
        calculated,
        raw.get(..EXT4_INODE_CHECKSUM_LO_OFFSET)
            .ok_or(Ext4ImageError::Malformed)?,
    );
    calculated = crc32c_update(calculated, &[0, 0]);
    if has_high {
        calculated = crc32c_update(
            calculated,
            raw.get(EXT4_INODE_CHECKSUM_LO_OFFSET + 2..EXT4_INODE_CHECKSUM_HI_OFFSET)
                .ok_or(Ext4ImageError::Malformed)?,
        );
        calculated = crc32c_update(calculated, &[0, 0]);
        calculated = crc32c_update(
            calculated,
            raw.get(EXT4_INODE_CHECKSUM_HI_OFFSET + 2..)
                .ok_or(Ext4ImageError::Malformed)?,
        );
    } else {
        calculated = crc32c_update(
            calculated,
            raw.get(EXT4_INODE_CHECKSUM_LO_OFFSET + 2..)
                .ok_or(Ext4ImageError::Malformed)?,
        );
    }
    let stored = u32::from(stored_lo) | (u32::from(stored_hi) << 16);
    if has_high {
        checksum_matches(stored, calculated)
    } else {
        checksum_matches(stored, calculated & 0xffff)
    }
}

fn validate_directory_block_checksum(
    seed: u32,
    inode: u32,
    generation: u32,
    block: &[u8],
) -> Result<(), Ext4ImageError> {
    if block.len() < EXT4_DIR_TAIL_SIZE {
        return Err(Ext4ImageError::Malformed);
    }
    let tail_offset = block.len() - EXT4_DIR_TAIL_SIZE;
    if le_u32(block, tail_offset)? != 0
        || le_u16(block, tail_offset + 4)? as usize != EXT4_DIR_TAIL_SIZE
        || block.get(tail_offset + 6).copied() != Some(0)
        || block.get(tail_offset + 7).copied() != Some(0xde)
    {
        return Err(Ext4ImageError::Malformed);
    }
    let stored = le_u32(block, tail_offset + 8)?;
    let mut calculated = metadata_checksum_prefix(seed, inode, generation);
    calculated = crc32c_update(
        calculated,
        block.get(..tail_offset).ok_or(Ext4ImageError::Malformed)?,
    );
    checksum_matches(stored, calculated)
}

fn validate_dx_block_checksum(
    seed: u32,
    inode: u32,
    generation: u32,
    block: &[u8],
    entries_offset: usize,
) -> Result<(), Ext4ImageError> {
    if block.len() < EXT4_DX_TAIL_SIZE {
        return Err(Ext4ImageError::Malformed);
    }
    let count = le_u16(block, entries_offset + 2)? as usize;
    let used_end = entries_offset
        .checked_add(count.checked_mul(8).ok_or(Ext4ImageError::Malformed)?)
        .ok_or(Ext4ImageError::Malformed)?;
    let tail_offset = block.len() - EXT4_DX_TAIL_SIZE;
    if used_end > tail_offset || le_u32(block, tail_offset)? != 0 {
        return Err(Ext4ImageError::Malformed);
    }
    let stored = le_u32(block, tail_offset + 4)?;
    let mut calculated = metadata_checksum_prefix(seed, inode, generation);
    calculated = crc32c_update(
        calculated,
        block.get(..used_end).ok_or(Ext4ImageError::Malformed)?,
    );
    calculated = crc32c_update(calculated, &[0; EXT4_DX_TAIL_SIZE]);
    checksum_matches(stored, calculated)
}

fn validate_extent_block_checksum(
    seed: u32,
    inode: u32,
    generation: u32,
    block: &[u8],
) -> Result<(), Ext4ImageError> {
    let max_entries = le_u16(block, 4)? as usize;
    let tail_offset = 12usize
        .checked_add(
            max_entries
                .checked_mul(12)
                .ok_or(Ext4ImageError::Malformed)?,
        )
        .ok_or(Ext4ImageError::Malformed)?;
    let stored = le_u32(block, tail_offset)?;
    let mut calculated = metadata_checksum_prefix(seed, inode, generation);
    calculated = crc32c_update(
        calculated,
        block.get(..tail_offset).ok_or(Ext4ImageError::Malformed)?,
    );
    checksum_matches(stored, calculated)
}

fn metadata_checksum_prefix(seed: u32, inode: u32, generation: u32) -> u32 {
    let checksum = crc32c_update(seed, &inode.to_le_bytes());
    crc32c_update(checksum, &generation.to_le_bytes())
}

fn checksum_matches(stored: u32, calculated: u32) -> Result<(), Ext4ImageError> {
    if stored == calculated {
        Ok(())
    } else {
        Err(Ext4ImageError::ChecksumMismatch)
    }
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
    fn ext4_meta_bg_is_rejected_explicitly() {
        let mut img = mkfs_style_ext4_image();
        let incompat = le_u32(&img, EXT4_SUPERBLOCK_OFFSET + 96).unwrap();
        put_u32(
            &mut img,
            EXT4_SUPERBLOCK_OFFSET + 96,
            incompat | EXT4_FEATURE_INCOMPAT_META_BG,
        );
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_INCOMPAT_META_BG
            ))
        ));
    }

    #[test]
    fn ext4_read_side_support_matrix_is_frozen() {
        let checksummed = metadata_csum_ext4_image();
        let fs = Ext4Image::mount(checksummed.as_slice())
            .expect("mount UUID-seeded 64bit/flex_bg metadata_csum fixture");
        assert_eq!(
            fs.read_file(b"/indexed/nested.bin").unwrap(),
            b"nested indexed payload\n"
        );
        assert_eq!(
            fs.read_file(b"/double.bin"),
            Err(Ext4ImageError::UnsupportedLayout)
        );

        let stored_seed = metadata_csum_seed_ext4_image(0x2468_ace0);
        let fs = Ext4Image::mount(stored_seed.as_slice())
            .expect("mount stored-seed metadata_csum fixture");
        assert_eq!(fs.lookup_path(b"/indexed/other.bin"), Ok(20));

        let legacy = tiny_ext4_image();
        let fs = Ext4Image::mount(legacy.as_slice()).expect("mount non-checksummed legacy fixture");
        assert!(fs.read_file(b"/double.bin").is_ok());

        let mut triple = tiny_ext4_image();
        put_u32(
            &mut triple,
            5 * 1024 + 18 * 128 + 4,
            ((12 + 256 + 65_536 + 1) * 1024) as u32,
        );
        let fs = Ext4Image::mount(triple.as_slice()).expect("mount triple-indirect fixture");
        assert_eq!(
            fs.read_file(b"/double.bin"),
            Err(Ext4ImageError::UnsupportedLayout)
        );

        for (offset, feature) in [
            (100, EXT4_FEATURE_RO_COMPAT_BIGALLOC),
            (96, EXT4_FEATURE_INCOMPAT_INLINE_DATA),
            (96, EXT4_FEATURE_INCOMPAT_META_BG),
        ] {
            let mut image = mkfs_style_ext4_image();
            let current = le_u32(&image, EXT4_SUPERBLOCK_OFFSET + offset).unwrap();
            put_u32(
                &mut image,
                EXT4_SUPERBLOCK_OFFSET + offset,
                current | feature,
            );
            assert!(matches!(
                Ext4Image::mount(image.as_slice()),
                Err(Ext4ImageError::UnsupportedFeature(mask)) if mask == feature
            ));
        }
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
    fn ext4_double_indirect_file_read_and_invalid_pointer_handling_work() {
        let img = tiny_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        let bytes = fs.read_file(b"/double.bin").expect("double indirect read");
        assert_eq!(&bytes[268 * 1024..268 * 1024 + 16], b"double indirect!");
        assert_eq!(&bytes[269 * 1024..269 * 1024 + 16], b"second dbl block");

        let mut img = tiny_ext4_image();
        put_u32(&mut img, 38 * 1024, 99_999);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.read_file(b"/double.bin"), Err(Ext4ImageError::Io));
    }

    #[test]
    fn ext4_triple_indirect_range_remains_unsupported() {
        let mut img = tiny_ext4_image();
        put_u32(
            &mut img,
            5 * 1024 + 18 * 128 + 4,
            ((12 + 256 + 65_536 + 1) * 1024) as u32,
        );
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
    fn crc32c_vectors_and_incremental_updates_match() {
        assert_eq!(crc32c(b""), 0);
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
        let state = crc32c_update(!0, b"1234");
        assert_eq!(!crc32c_update(state, b"56789"), crc32c(b"123456789"));
    }

    #[test]
    fn ext4_metadata_csum_profile_validates_every_trusted_metadata_type() {
        let img = metadata_csum_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount metadata_csum fixture");
        assert!(fs.superblock().metadata_csum);
        assert_eq!(
            fs.read_file(b"/indexed/nested.bin").unwrap(),
            b"nested indexed payload\n".to_vec()
        );
        assert_eq!(
            fs.read_file(b"/depth.bin").unwrap(),
            b"depth-one data\n".to_vec()
        );
        assert_eq!(
            fs.read_file(b"/external-link").unwrap(),
            b"nested indexed payload\n".to_vec()
        );
        assert_eq!(
            fs.read_file(b"/double.bin"),
            Err(Ext4ImageError::UnsupportedLayout)
        );
    }

    #[test]
    fn ext4_metadata_csum_corruption_is_rejected_at_each_read_point() {
        let mut img = metadata_csum_ext4_image();
        img[EXT4_SUPERBLOCK_OFFSET + 16] ^= 1;
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::ChecksumMismatch)
        ));

        let mut img = metadata_csum_ext4_image();
        img[REALISTIC_BLOCK_SIZE + 12] ^= 1;
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::ChecksumMismatch)
        ));

        let mut img = metadata_csum_ext4_image();
        img[realistic_inode_offset(20) + 8] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before inode read");
        assert_eq!(
            fs.read_file(b"/indexed/nested.bin"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = metadata_csum_ext4_image();
        img[16 * REALISTIC_BLOCK_SIZE + 8] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before directory read");
        assert_eq!(fs.read_dir(b"/"), Err(Ext4ImageError::ChecksumMismatch));

        let mut img = metadata_csum_ext4_image();
        img[20 * REALISTIC_BLOCK_SIZE + EXT4_DX_ROOT_INFO_OFFSET + 4] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before dx read");
        assert_eq!(
            fs.lookup_path(b"/indexed/nested.bin"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = metadata_csum_ext4_image();
        img[21 * REALISTIC_BLOCK_SIZE + EXT4_DX_NODE_ENTRIES_OFFSET + 4] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before dx-node read");
        assert_eq!(
            fs.lookup_path(b"/indexed/nested.bin"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = metadata_csum_ext4_image();
        img[23 * REALISTIC_BLOCK_SIZE + 8] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before leaf read");
        assert_eq!(
            fs.lookup_path(b"/indexed/nested.bin"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = metadata_csum_ext4_image();
        img[31 * REALISTIC_BLOCK_SIZE + 8] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before extent read");
        assert_eq!(
            fs.read_file(b"/depth.bin"),
            Err(Ext4ImageError::ChecksumMismatch)
        );
    }

    #[test]
    fn ext4_metadata_csum_requires_crc32c_checksum_type() {
        let mut img = metadata_csum_ext4_image();
        img[EXT4_SUPERBLOCK_OFFSET + 373] = 0xff;
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(
                EXT4_FEATURE_RO_COMPAT_METADATA_CSUM
            ))
        ));
    }

    #[test]
    fn ext4_metadata_csum_seed_selects_stored_seed() {
        let stored_seed = 0x1357_9bdf;
        let img = metadata_csum_seed_ext4_image(stored_seed);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount metadata_csum_seed fixture");
        assert_eq!(fs.superblock().checksum_seed, stored_seed);
        assert_eq!(
            fs.read_file(b"/indexed/nested.bin").unwrap(),
            b"nested indexed payload\n".to_vec()
        );

        let mut wrong = img;
        put_u32(
            &mut wrong,
            EXT4_SUPERBLOCK_OFFSET + 0x270,
            stored_seed ^ 0x0102_0304,
        );
        refresh_superblock_checksum(&mut wrong);
        assert!(matches!(
            Ext4Image::mount(wrong.as_slice()),
            Err(Ext4ImageError::ChecksumMismatch)
        ));
    }

    #[test]
    fn ext4_metadata_csum_seed_without_metadata_csum_is_rejected() {
        let mut img = mkfs_style_ext4_image();
        let incompat = le_u32(&img, EXT4_SUPERBLOCK_OFFSET + 96).unwrap();
        put_u32(
            &mut img,
            EXT4_SUPERBLOCK_OFFSET + 96,
            incompat | EXT4_FEATURE_INCOMPAT_CSUM_SEED,
        );
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 0x270, 0x1234_5678);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(mask))
                if mask == EXT4_FEATURE_INCOMPAT_CSUM_SEED
        ));
    }

    #[test]
    fn ext4_indexed_directory_enumeration_is_validated_and_unique() {
        for img in [mkfs_style_ext4_image(), metadata_csum_ext4_image()] {
            let fs = Ext4Image::mount(img.as_slice()).expect("mount indexed fixture");
            let entries = fs
                .read_dir(b"/indexed")
                .expect("enumerate indexed directory");
            let names: alloc::vec::Vec<&[u8]> = entries.iter().map(Ext4DirEntry::name).collect();
            assert_eq!(names, [b".".as_slice(), b"..", b"nested.bin", b"other.bin"]);
            for (idx, entry) in entries.iter().enumerate() {
                assert!(!entries[..idx]
                    .iter()
                    .any(|prior| prior.inode == entry.inode && prior.name() == entry.name()));
            }
            assert_eq!(fs.lookup_path(b"/indexed/nested.bin"), Ok(20));
            assert_eq!(
                fs.lookup_path(b"/indexed/missing.bin"),
                Err(Ext4ImageError::NotFound)
            );
        }
    }

    #[test]
    fn ext4_indexed_enumeration_rejects_checksum_and_layout_corruption() {
        let mut img = metadata_csum_ext4_image();
        img[21 * REALISTIC_BLOCK_SIZE + EXT4_DX_NODE_ENTRIES_OFFSET + 4] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before dx corruption read");
        assert_eq!(
            fs.read_dir(b"/indexed"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = metadata_csum_ext4_image();
        img[24 * REALISTIC_BLOCK_SIZE + 8] ^= 1;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount before leaf corruption read");
        assert_eq!(
            fs.read_dir(b"/indexed"),
            Err(Ext4ImageError::ChecksumMismatch)
        );

        let mut img = mkfs_style_ext4_image();
        put_u16(
            &mut img,
            22 * REALISTIC_BLOCK_SIZE + EXT4_DX_NODE_ENTRIES_OFFSET + 2,
            u16::MAX,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("mount malformed dx fixture");
        assert_eq!(fs.read_dir(b"/indexed"), Err(Ext4ImageError::Malformed));

        let mut img = mkfs_style_ext4_image();
        put_u32(
            &mut img,
            22 * REALISTIC_BLOCK_SIZE + EXT4_DX_NODE_ENTRIES_OFFSET + 4,
            99_999,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("mount bad leaf fixture");
        assert_eq!(fs.read_dir(b"/indexed"), Err(Ext4ImageError::Io));
    }

    #[test]
    fn ext4_bigalloc_is_rejected() {
        let mut img = tiny_ext4_image();
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
    fn ext4_64bit_flex_bg_profile_reads_sparse_extent_and_external_symlink() {
        let mut img = tiny_ext4_image();
        let incompat = EXT4_FEATURE_INCOMPAT_FILETYPE
            | EXT4_FEATURE_INCOMPAT_EXTENTS
            | EXT4_FEATURE_INCOMPAT_64BIT
            | EXT4_FEATURE_INCOMPAT_FLEX_BG;
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 96, incompat);
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 254, 64);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            &fs.read_file(b"/hole.bin").unwrap()[1024..1024 + 16],
            b"after sparse gap"
        );
        assert_eq!(
            fs.read_symlink(b"/longlink").unwrap(),
            long_symlink_target()
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
    fn ext4_native_htree_hashes_match_e2fsprogs_vectors() {
        let zero_seed = [0u32; 4];
        assert_eq!(
            ext4_dir_hash(0, b"target.bin", zero_seed),
            Some(0x318e_a834)
        );
        assert_eq!(
            ext4_dir_hash(1, b"target.bin", zero_seed),
            Some(0x811f_3776)
        );
        assert_eq!(
            ext4_dir_hash(2, b"target.bin", zero_seed),
            Some(0xc8ef_3d6c)
        );
        assert_eq!(
            ext4_dir_hash(3, b"target.bin", zero_seed),
            Some(0x318e_a834)
        );
        assert_eq!(
            ext4_dir_hash(4, b"target.bin", zero_seed),
            Some(0x811f_3776)
        );
        assert_eq!(
            ext4_dir_hash(5, b"target.bin", zero_seed),
            Some(0xc8ef_3d6c)
        );
        assert_eq!(ext4_dir_hash(6, b"target.bin", zero_seed), None);
        assert_eq!(ext4_dir_hash(0xff, b"target.bin", zero_seed), None);

        let seed = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
        assert_eq!(ext4_dir_hash(1, b"target.bin", seed), Some(0x811f_3776));
        assert_eq!(ext4_dir_hash(2, b"target.bin", seed), Some(0xc8ef_3d6c));

        let long = b"abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        assert_eq!(ext4_dir_hash(1, long, zero_seed), Some(0x9f6d_c676));
        assert_eq!(ext4_dir_hash(2, long, zero_seed), Some(0xca7d_fe38));
        assert_eq!(ext4_dir_hash(4, long, zero_seed), Some(0x9f6d_c676));
        assert_eq!(ext4_dir_hash(5, long, zero_seed), Some(0xca7d_fe38));
    }

    #[test]
    fn ext4_native_hash_versions_route_to_selected_leaf() {
        for version in 0..=5 {
            let mut img = tiny_ext4_image();
            put_u32(&mut img, 5 * 1024 + 21 * 128 + 4, 3 * 1024);
            put_u16(&mut img, 5 * 1024 + 21 * 128 + 56, 3);
            img[35 * 1024 + EXT4_DX_ROOT_INFO_OFFSET + 4] = version;
            let hash = ext4_dir_hash(version, b"target.bin", [0; 4]).unwrap();
            put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 2);
            put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 8, hash);
            put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 12, 2);
            img[36 * 1024..37 * 1024].fill(0);
            put_u16(&mut img, 36 * 1024 + 4, 1024);
            write_dirent(&mut img[37 * 1024..38 * 1024], 23, b"target.bin", 1, 1024);
            let fs = Ext4Image::mount(img.as_slice()).expect("mount routed htree fixture");
            assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));
            assert_eq!(
                fs.lookup_path(b"/indexed/missing.bin"),
                Err(Ext4ImageError::NotFound)
            );
        }
    }

    #[test]
    fn ext4_htree_collision_continuation_scans_adjacent_leaf() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 21 * 128 + 4, 3 * 1024);
        put_u16(&mut img, 5 * 1024 + 21 * 128 + 56, 3);
        let hash = ext4_dir_hash(0, b"target.bin", [0; 4]).unwrap();
        put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 2);
        put_u32(
            &mut img,
            35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 8,
            hash | 1,
        );
        put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 12, 2);
        img[36 * 1024..37 * 1024].fill(0);
        put_u16(&mut img, 36 * 1024 + 4, 1024);
        write_dirent(&mut img[37 * 1024..38 * 1024], 23, b"target.bin", 1, 1024);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount collision fixture");
        assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));
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
    fn ext4_htree_legacy_hash_selects_leaf_and_siphash_falls_back() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 21 * 128 + 4, 3 * 1024);
        put_u16(&mut img, 5 * 1024 + 21 * 128 + 56, 3);
        let hash = legacy_dir_hash(b"target.bin", true) & !1;
        put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 2);
        put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 8, hash);
        put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 12, 2);
        img[36 * 1024..37 * 1024].fill(0);
        put_u16(&mut img, 36 * 1024 + 4, 1024);
        write_dirent(&mut img[37 * 1024..38 * 1024], 23, b"target.bin", 1, 1024);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));

        img[35 * 1024 + EXT4_DX_ROOT_INFO_OFFSET + 4] = 6;
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));
    }

    #[test]
    fn ext4_htree_hash_order_violation_is_rejected() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 21 * 128 + 4, 3 * 1024);
        put_u16(&mut img, 5 * 1024 + 21 * 128 + 56, 3);
        put_u16(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 3);
        put_u32(
            &mut img,
            35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 8,
            0x8000_0000,
        );
        put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 12, 2);
        put_u32(
            &mut img,
            35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 16,
            0x4000_0000,
        );
        put_u32(&mut img, 35 * 1024 + EXT4_DX_ROOT_ENTRIES_OFFSET + 20, 1);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount hash-order fixture");
        assert_eq!(
            fs.lookup_path(b"/indexed/target.bin"),
            Err(Ext4ImageError::Malformed)
        );
    }

    #[test]
    fn ext4_htree_dx_node_traversal_and_malformed_count_handling_work() {
        let mut img = tiny_ext4_image();
        put_u32(&mut img, 5 * 1024 + 21 * 128 + 4, 4 * 1024);
        put_u16(&mut img, 5 * 1024 + 21 * 128 + 56, 4);
        img[35 * 1024 + EXT4_DX_ROOT_INFO_OFFSET + 6] = 1;
        write_dx_node(&mut img[36 * 1024..37 * 1024], 3);
        write_dirent(&mut img[38 * 1024..39 * 1024], 23, b"target.bin", 1, 1024);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(fs.lookup_path(b"/indexed/target.bin"), Ok(23));

        put_u16(&mut img, 36 * 1024 + EXT4_DX_NODE_ENTRIES_OFFSET, 1);
        put_u16(&mut img, 36 * 1024 + EXT4_DX_NODE_ENTRIES_OFFSET + 2, 2);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount");
        assert_eq!(
            fs.lookup_path(b"/indexed/target.bin"),
            Err(Ext4ImageError::Malformed)
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

    const REALISTIC_BLOCK_SIZE: usize = 4096;
    const REALISTIC_BLOCKS: usize = 96;

    #[test]
    fn ext4_mkfs_style_64bit_flex_bg_profile_reads_nested_data() {
        let img = mkfs_style_ext4_image();
        let fs = Ext4Image::mount(img.as_slice()).expect("mount realistic fixture");
        assert_eq!(fs.superblock().block_size(), REALISTIC_BLOCK_SIZE as u64);
        assert_eq!(fs.superblock().first_data_block, 0);

        let root = fs.read_dir(b"/").expect("root directory");
        assert!(root.iter().any(|entry| entry.name() == b"indexed"));
        assert_eq!(fs.lookup_path(b"/indexed/nested.bin"), Ok(20));
        assert_eq!(
            fs.read_file(b"/indexed/nested.bin").unwrap(),
            b"nested indexed payload\n".to_vec()
        );
        assert_eq!(
            fs.lookup_path(b"/indexed/missing.bin"),
            Err(Ext4ImageError::NotFound)
        );

        let sparse = fs.read_file(b"/sparse.bin").expect("sparse extent");
        assert_eq!(
            &sparse[..2 * REALISTIC_BLOCK_SIZE],
            alloc::vec![0; 2 * REALISTIC_BLOCK_SIZE]
        );
        assert_eq!(
            &sparse[2 * REALISTIC_BLOCK_SIZE..2 * REALISTIC_BLOCK_SIZE + 12],
            b"sparse tail\n"
        );
        assert_eq!(
            fs.read_file(b"/depth.bin").unwrap(),
            b"depth-one data\n".to_vec()
        );
        assert_eq!(
            fs.read_symlink(b"/external-link").unwrap(),
            realistic_external_symlink_target().to_vec()
        );
        assert_eq!(
            fs.read_file(b"/external-link").unwrap(),
            b"nested indexed payload\n".to_vec()
        );

        let double = fs.read_file(b"/double.bin").expect("double indirect");
        let double_logical = EXT4_NDIR_BLOCKS + REALISTIC_BLOCK_SIZE / 4;
        assert!(double[..double_logical * REALISTIC_BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(
            &double
                [double_logical * REALISTIC_BLOCK_SIZE..double_logical * REALISTIC_BLOCK_SIZE + 16],
            b"double-indirect!"
        );
    }

    #[test]
    fn ext4_mkfs_style_profile_rejects_gated_features_stably() {
        for feature in [
            EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
            EXT4_FEATURE_RO_COMPAT_BIGALLOC,
        ] {
            let mut img = mkfs_style_ext4_image();
            let current = le_u32(&img, EXT4_SUPERBLOCK_OFFSET + 100).unwrap();
            put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 100, current | feature);
            assert!(matches!(
                Ext4Image::mount(img.as_slice()),
                Err(Ext4ImageError::UnsupportedFeature(mask)) if mask == feature
            ));
        }

        let mut img = mkfs_style_ext4_image();
        let current = le_u32(&img, EXT4_SUPERBLOCK_OFFSET + 96).unwrap();
        put_u32(
            &mut img,
            EXT4_SUPERBLOCK_OFFSET + 96,
            current | EXT4_FEATURE_INCOMPAT_INLINE_DATA,
        );
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::UnsupportedFeature(mask))
                if mask == EXT4_FEATURE_INCOMPAT_INLINE_DATA
        ));
    }

    #[test]
    fn ext4_mkfs_style_malformed_layouts_are_rejected() {
        let mut img = mkfs_style_ext4_image();
        put_u32(&mut img, EXT4_SUPERBLOCK_OFFSET + 20, 1);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::Malformed)
        ));

        let mut img = mkfs_style_ext4_image();
        put_u16(&mut img, EXT4_SUPERBLOCK_OFFSET + 88, 192);
        assert!(matches!(
            Ext4Image::mount(img.as_slice()),
            Err(Ext4ImageError::Malformed)
        ));

        let mut img = mkfs_style_ext4_image();
        // Force the second group's flex_bg inode table beyond the image.
        put_u32(
            &mut img,
            REALISTIC_BLOCK_SIZE + 64 + 8,
            REALISTIC_BLOCKS as u32,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("descriptor table still mounts");
        assert_eq!(fs.lookup_path(b"/double.bin"), Err(Ext4ImageError::Io));

        let mut img = mkfs_style_ext4_image();
        // A directory record may never cross a filesystem block boundary.
        put_u16(
            &mut img,
            16 * REALISTIC_BLOCK_SIZE + 4,
            (REALISTIC_BLOCK_SIZE - 4) as u16,
        );
        let fs = Ext4Image::mount(img.as_slice()).expect("mount malformed directory fixture");
        assert_eq!(fs.read_dir(b"/"), Err(Ext4ImageError::Malformed));
    }

    #[test]
    fn ext4_extent_unwritten_and_overlap_semantics_are_hardened() {
        let mut img = mkfs_style_ext4_image();
        let sparse_inode = realistic_inode_offset(13);
        // An unwritten extent must remain a zero-filled hole even if its physical
        // block contains non-zero bytes.
        put_u16(&mut img, sparse_inode + 56, 0x8001);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount unwritten extent fixture");
        assert!(fs
            .read_file(b"/sparse.bin")
            .unwrap()
            .iter()
            .all(|byte| *byte == 0));

        let mut img = mkfs_style_ext4_image();
        let sparse_inode = realistic_inode_offset(13);
        put_u16(&mut img, sparse_inode + 42, 2);
        put_u32(&mut img, sparse_inode + 64, 2);
        put_u16(&mut img, sparse_inode + 68, 1);
        put_u16(&mut img, sparse_inode + 70, 0);
        put_u32(&mut img, sparse_inode + 72, 31);
        let fs = Ext4Image::mount(img.as_slice()).expect("mount overlapping extent fixture");
        assert_eq!(fs.read_file(b"/sparse.bin"), Err(Ext4ImageError::Malformed));
    }

    fn realistic_external_symlink_target() -> &'static [u8] {
        b"/////////////////////////////////////////////////////////////////indexed/nested.bin"
    }

    fn metadata_csum_ext4_image() -> alloc::vec::Vec<u8> {
        let mut img = mkfs_style_ext4_image();
        let sb = EXT4_SUPERBLOCK_OFFSET;
        let ro = le_u32(&img, sb + 100).unwrap() | EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
        put_u32(&mut img, sb + 100, ro);
        img[sb + 104..sb + 120].copy_from_slice(b"YARM-ext4-csum!!");
        img[sb + 373] = EXT4_CHECKSUM_TYPE_CRC32C;
        let seed = crc32c_update(!0, &img[sb + 104..sb + 120]);

        // Linear directory leaves reserve the final 12-byte checksum dirent.
        put_u16(
            &mut img,
            16 * REALISTIC_BLOCK_SIZE + 132 + 4,
            (REALISTIC_BLOCK_SIZE - 132 - EXT4_DIR_TAIL_SIZE) as u16,
        );
        set_directory_tail(&mut img, 16, 2, seed);
        for block in 23..27 {
            put_u16(
                &mut img,
                block * REALISTIC_BLOCK_SIZE + 4,
                (REALISTIC_BLOCK_SIZE - EXT4_DIR_TAIL_SIZE) as u16,
            );
            set_directory_tail(&mut img, block, 12, seed);
        }

        set_dx_tail(&mut img, 20, 12, EXT4_DX_ROOT_ENTRIES_OFFSET, seed);
        set_dx_tail(&mut img, 21, 12, EXT4_DX_NODE_ENTRIES_OFFSET, seed);
        set_dx_tail(&mut img, 22, 12, EXT4_DX_NODE_ENTRIES_OFFSET, seed);
        set_extent_tail(&mut img, 31, 14, seed);

        for inode in [2, 12, 13, 14, 18, 19, 20] {
            set_inode_checksum(&mut img, inode, seed);
        }
        for group in 0..2 {
            set_group_descriptor_checksum(&mut img, group, seed);
        }
        put_u32(&mut img, sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = crc32c_update(!0, &img[sb..sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET]);
        put_u32(&mut img, sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        img
    }

    fn metadata_csum_seed_ext4_image(seed: u32) -> alloc::vec::Vec<u8> {
        let mut img = metadata_csum_ext4_image();
        let sb = EXT4_SUPERBLOCK_OFFSET;
        let incompat = le_u32(&img, sb + 96).unwrap() | EXT4_FEATURE_INCOMPAT_CSUM_SEED;
        put_u32(&mut img, sb + 96, incompat);
        put_u32(&mut img, sb + 0x270, seed);
        resign_metadata_csum_image(&mut img, seed);
        img
    }

    fn resign_metadata_csum_image(img: &mut [u8], seed: u32) {
        set_directory_tail(img, 16, 2, seed);
        for block in 23..27 {
            set_directory_tail(img, block, 12, seed);
        }
        set_dx_tail(img, 20, 12, EXT4_DX_ROOT_ENTRIES_OFFSET, seed);
        set_dx_tail(img, 21, 12, EXT4_DX_NODE_ENTRIES_OFFSET, seed);
        set_dx_tail(img, 22, 12, EXT4_DX_NODE_ENTRIES_OFFSET, seed);
        set_extent_tail(img, 31, 14, seed);
        for inode in [2, 12, 13, 14, 18, 19, 20] {
            set_inode_checksum(img, inode, seed);
        }
        for group in 0..2 {
            set_group_descriptor_checksum(img, group, seed);
        }
        refresh_superblock_checksum(img);
    }

    fn refresh_superblock_checksum(img: &mut [u8]) {
        let sb = EXT4_SUPERBLOCK_OFFSET;
        put_u32(img, sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = crc32c_update(!0, &img[sb..sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET]);
        put_u32(img, sb + EXT4_SUPERBLOCK_CHECKSUM_OFFSET, checksum);
    }

    fn set_group_descriptor_checksum(img: &mut [u8], group: u32, seed: u32) {
        let off = REALISTIC_BLOCK_SIZE + group as usize * 64;
        put_u16(img, off + 30, 0);
        let descriptor = &img[off..off + 64];
        let mut checksum = crc32c_update(seed, &group.to_le_bytes());
        checksum = crc32c_update(checksum, &descriptor[..30]);
        checksum = crc32c_update(checksum, &[0, 0]);
        checksum = crc32c_update(checksum, &descriptor[32..]);
        put_u16(img, off + 30, checksum as u16);
    }

    fn set_inode_checksum(img: &mut [u8], inode: u32, seed: u32) {
        let off = realistic_inode_offset(inode);
        put_u16(img, off + EXT4_INODE_EXTRA_ISIZE_OFFSET, 32);
        put_u16(img, off + EXT4_INODE_CHECKSUM_LO_OFFSET, 0);
        put_u16(img, off + EXT4_INODE_CHECKSUM_HI_OFFSET, 0);
        let raw = &img[off..off + 256];
        let generation = le_u32(raw, 100).unwrap();
        let mut checksum = metadata_checksum_prefix(seed, inode, generation);
        checksum = crc32c_update(checksum, &raw[..EXT4_INODE_CHECKSUM_LO_OFFSET]);
        checksum = crc32c_update(checksum, &[0, 0]);
        checksum = crc32c_update(
            checksum,
            &raw[EXT4_INODE_CHECKSUM_LO_OFFSET + 2..EXT4_INODE_CHECKSUM_HI_OFFSET],
        );
        checksum = crc32c_update(checksum, &[0, 0]);
        checksum = crc32c_update(checksum, &raw[EXT4_INODE_CHECKSUM_HI_OFFSET + 2..]);
        put_u16(img, off + EXT4_INODE_CHECKSUM_LO_OFFSET, checksum as u16);
        put_u16(
            img,
            off + EXT4_INODE_CHECKSUM_HI_OFFSET,
            (checksum >> 16) as u16,
        );
    }

    fn set_directory_tail(img: &mut [u8], block: usize, inode: u32, seed: u32) {
        let off = block * REALISTIC_BLOCK_SIZE;
        let tail = off + REALISTIC_BLOCK_SIZE - EXT4_DIR_TAIL_SIZE;
        put_u32(img, tail, 0);
        put_u16(img, tail + 4, EXT4_DIR_TAIL_SIZE as u16);
        img[tail + 6] = 0;
        img[tail + 7] = 0xde;
        put_u32(img, tail + 8, 0);
        let generation = 0;
        let mut checksum = metadata_checksum_prefix(seed, inode, generation);
        checksum = crc32c_update(checksum, &img[off..tail]);
        put_u32(img, tail + 8, checksum);
    }

    fn set_dx_tail(img: &mut [u8], block: usize, inode: u32, entries_offset: usize, seed: u32) {
        let off = block * REALISTIC_BLOCK_SIZE;
        put_u16(
            img,
            off + entries_offset,
            ((REALISTIC_BLOCK_SIZE - entries_offset - EXT4_DX_TAIL_SIZE) / 8) as u16,
        );
        let count = le_u16(img, off + entries_offset + 2).unwrap() as usize;
        let used_end = off + entries_offset + count * 8;
        let tail = off + REALISTIC_BLOCK_SIZE - EXT4_DX_TAIL_SIZE;
        put_u32(img, tail, 0);
        put_u32(img, tail + 4, 0);
        let mut checksum = metadata_checksum_prefix(seed, inode, 0);
        checksum = crc32c_update(checksum, &img[off..used_end]);
        checksum = crc32c_update(checksum, &[0; EXT4_DX_TAIL_SIZE]);
        put_u32(img, tail + 4, checksum);
    }

    fn set_extent_tail(img: &mut [u8], block: usize, inode: u32, seed: u32) {
        let off = block * REALISTIC_BLOCK_SIZE;
        let max = le_u16(img, off + 4).unwrap() as usize;
        let tail = off + 12 + max * 12;
        put_u32(img, tail, 0);
        let mut checksum = metadata_checksum_prefix(seed, inode, 0);
        checksum = crc32c_update(checksum, &img[off..tail]);
        put_u32(img, tail, checksum);
    }

    fn mkfs_style_ext4_image() -> alloc::vec::Vec<u8> {
        let mut img = alloc::vec![0u8; REALISTIC_BLOCKS * REALISTIC_BLOCK_SIZE];
        let sb = EXT4_SUPERBLOCK_OFFSET;
        put_u32(&mut img, sb, 32); // two groups of sixteen inodes
        put_u32(&mut img, sb + 4, REALISTIC_BLOCKS as u32);
        put_u32(&mut img, sb + 20, 0); // first data block for 4KiB layout
        put_u32(&mut img, sb + 24, 2); // 4KiB blocks
        put_u32(&mut img, sb + 32, 48); // two block groups
        put_u32(&mut img, sb + 40, 16);
        put_u16(&mut img, sb + 56, EXT4_MAGIC);
        put_u16(&mut img, sb + 88, 256);
        put_u32(&mut img, sb + 92, EXT4_FEATURE_COMPAT_DIR_INDEX);
        put_u32(
            &mut img,
            sb + 96,
            EXT4_FEATURE_INCOMPAT_FILETYPE
                | EXT4_FEATURE_INCOMPAT_EXTENTS
                | EXT4_FEATURE_INCOMPAT_64BIT
                | EXT4_FEATURE_INCOMPAT_FLEX_BG,
        );
        put_u32(
            &mut img,
            sb + 100,
            EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
                | EXT4_FEATURE_RO_COMPAT_LARGE_FILE
                | EXT4_FEATURE_RO_COMPAT_HUGE_FILE
                | EXT4_FEATURE_RO_COMPAT_DIR_NLINK
                | EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE,
        );
        put_u16(&mut img, sb + 254, 64);
        put_u32(&mut img, sb + 336, 0);
        img[sb + 252] = 1; // half-MD4: validated exhaustive htree fallback

        // Two 64-byte descriptors. Group 1's inode table is deliberately in
        // group 0 to mimic flex_bg placement through absolute descriptor fields.
        put_u32(&mut img, REALISTIC_BLOCK_SIZE + 8, 6);
        put_u32(&mut img, REALISTIC_BLOCK_SIZE + 64 + 8, 7);

        write_inode(
            &mut img,
            realistic_inode_offset(2),
            0x4000,
            REALISTIC_BLOCK_SIZE as u32,
            16,
        );
        write_extent_inode_with_len_flags(
            &mut img,
            realistic_inode_offset(12),
            0x4000,
            (7 * REALISTIC_BLOCK_SIZE) as u32,
            0,
            7,
            20,
            EXT4_INDEX_FL,
        );
        write_inode_logical(
            &mut img,
            realistic_inode_offset(13),
            0x8000,
            (3 * REALISTIC_BLOCK_SIZE) as u32,
            2,
            30,
        );
        write_depth1_inode(&mut img, realistic_inode_offset(14), 0x8000, 15, 31);
        let double_blocks = EXT4_NDIR_BLOCKS + REALISTIC_BLOCK_SIZE / 4 + 1;
        write_indirect_inode(
            &mut img,
            realistic_inode_offset(18),
            0x8000,
            (double_blocks * REALISTIC_BLOCK_SIZE) as u32,
            &[0; 12],
            0,
            33,
            0,
        );
        let target = realistic_external_symlink_target();
        write_inode(
            &mut img,
            realistic_inode_offset(19),
            0xa000,
            target.len() as u32,
            36,
        );
        write_inode(&mut img, realistic_inode_offset(20), 0x8000, 23, 37);

        let root = realistic_block_mut(&mut img, 16);
        write_dirent(&mut root[0..12], 2, b".", 2, 12);
        write_dirent(&mut root[12..24], 2, b"..", 2, 12);
        write_dirent(&mut root[24..44], 12, b"indexed", 2, 20);
        write_dirent(&mut root[44..64], 13, b"sparse.bin", 1, 20);
        write_dirent(&mut root[64..84], 14, b"depth.bin", 1, 20);
        write_dirent(&mut root[84..104], 18, b"double.bin", 1, 20);
        write_dirent(&mut root[104..132], 19, b"external-link", 7, 28);
        write_dirent(
            &mut root[132..],
            20,
            b"target.bin",
            1,
            (REALISTIC_BLOCK_SIZE - 132) as u16,
        );

        write_realistic_htree_root(realistic_block_mut(&mut img, 20), 1, 2);
        write_realistic_dx_node(realistic_block_mut(&mut img, 21), &[(0, 2)]);
        write_realistic_dx_node(
            realistic_block_mut(&mut img, 22),
            &[(0, 3), (0x7000_0000, 4)],
        );
        write_dirent(
            realistic_block_mut(&mut img, 23),
            20,
            b"nested.bin",
            1,
            REALISTIC_BLOCK_SIZE as u16,
        );
        write_dirent(
            realistic_block_mut(&mut img, 24),
            20,
            b"other.bin",
            1,
            REALISTIC_BLOCK_SIZE as u16,
        );
        // Remaining logical htree leaf blocks are valid empty directory blocks.
        for block in 25..27 {
            write_dirent(
                realistic_block_mut(&mut img, block),
                0,
                b"",
                0,
                REALISTIC_BLOCK_SIZE as u16,
            );
        }

        img[30 * REALISTIC_BLOCK_SIZE..30 * REALISTIC_BLOCK_SIZE + 12]
            .copy_from_slice(b"sparse tail\n");
        write_realistic_leaf_extent(realistic_block_mut(&mut img, 31), 0, 1, 32);
        img[32 * REALISTIC_BLOCK_SIZE..32 * REALISTIC_BLOCK_SIZE + 15]
            .copy_from_slice(b"depth-one data\n");
        put_u32(&mut img, 33 * REALISTIC_BLOCK_SIZE, 34);
        put_u32(&mut img, 34 * REALISTIC_BLOCK_SIZE, 35);
        img[35 * REALISTIC_BLOCK_SIZE..35 * REALISTIC_BLOCK_SIZE + 16]
            .copy_from_slice(b"double-indirect!");
        img[36 * REALISTIC_BLOCK_SIZE..36 * REALISTIC_BLOCK_SIZE + target.len()]
            .copy_from_slice(target);
        img[37 * REALISTIC_BLOCK_SIZE..37 * REALISTIC_BLOCK_SIZE + 23]
            .copy_from_slice(b"nested indexed payload\n");
        img
    }

    fn realistic_inode_offset(inode: u32) -> usize {
        let group = (inode - 1) / 16;
        let index = (inode - 1) % 16;
        let table_block = if group == 0 { 6 } else { 7 };
        table_block * REALISTIC_BLOCK_SIZE + index as usize * 256
    }

    fn realistic_block_mut(img: &mut [u8], block: usize) -> &mut [u8] {
        &mut img[block * REALISTIC_BLOCK_SIZE..(block + 1) * REALISTIC_BLOCK_SIZE]
    }

    fn write_realistic_htree_root(dst: &mut [u8], first_child: u32, levels: u8) {
        write_dirent(&mut dst[0..12], 12, b".", 2, 12);
        write_dirent(
            &mut dst[12..],
            2,
            b"..",
            2,
            (REALISTIC_BLOCK_SIZE - 12) as u16,
        );
        put_u32(dst, EXT4_DX_ROOT_INFO_OFFSET, 0);
        dst[EXT4_DX_ROOT_INFO_OFFSET + 4] = 1;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 5] = 8;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 6] = levels;
        dst[EXT4_DX_ROOT_INFO_OFFSET + 7] = 0;
        put_u16(
            dst,
            EXT4_DX_ROOT_ENTRIES_OFFSET,
            ((REALISTIC_BLOCK_SIZE - EXT4_DX_ROOT_ENTRIES_OFFSET) / 8) as u16,
        );
        put_u16(dst, EXT4_DX_ROOT_ENTRIES_OFFSET + 2, 1);
        put_u32(dst, EXT4_DX_ROOT_ENTRIES_OFFSET + 4, first_child);
    }

    fn write_realistic_dx_node(dst: &mut [u8], entries: &[(u32, u32)]) {
        put_u32(dst, 0, 0);
        put_u16(dst, 4, REALISTIC_BLOCK_SIZE as u16);
        dst[6] = 0;
        dst[7] = 0;
        put_u16(
            dst,
            EXT4_DX_NODE_ENTRIES_OFFSET,
            ((REALISTIC_BLOCK_SIZE - EXT4_DX_NODE_ENTRIES_OFFSET) / 8) as u16,
        );
        put_u16(dst, EXT4_DX_NODE_ENTRIES_OFFSET + 2, entries.len() as u16);
        for (idx, (hash, block)) in entries.iter().copied().enumerate() {
            let off = EXT4_DX_NODE_ENTRIES_OFFSET + idx * 8;
            if idx != 0 {
                put_u32(dst, off, hash);
            }
            put_u32(dst, off + 4, block);
        }
    }

    fn write_realistic_leaf_extent(dst: &mut [u8], logical: u32, len: u16, start: u32) {
        put_u16(dst, 0, 0xf30a);
        put_u16(dst, 2, 1);
        put_u16(dst, 4, ((REALISTIC_BLOCK_SIZE - 12) / 12) as u16);
        put_u16(dst, 6, 0);
        put_u32(dst, 12, logical);
        put_u16(dst, 16, len);
        put_u16(dst, 18, 0);
        put_u32(dst, 20, start);
    }

    fn tiny_ext4_image() -> alloc::vec::Vec<u8> {
        let mut img = alloc::vec![0u8; 80 * 1024];
        let sb = EXT4_SUPERBLOCK_OFFSET;
        put_u32(&mut img, sb, 32); // inodes
        put_u32(&mut img, sb + 4, 80); // blocks
        put_u32(&mut img, sb + 20, 1); // first data block for 1KiB layout
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
        put_u32(&mut img, 33 * 1024, 38);
        put_u32(&mut img, 38 * 1024, 39);
        put_u32(&mut img, 38 * 1024 + 4, 40);
        img[39 * 1024..39 * 1024 + 16].copy_from_slice(b"double indirect!");
        img[40 * 1024..40 * 1024 + 16].copy_from_slice(b"second dbl block");
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

    fn write_dx_node(dst: &mut [u8], leaf_logical: u32) {
        put_u32(dst, 0, 0);
        put_u16(dst, 4, dst.len() as u16);
        dst[6] = 0;
        dst[7] = 0;
        put_u16(dst, EXT4_DX_NODE_ENTRIES_OFFSET, 127);
        put_u16(dst, EXT4_DX_NODE_ENTRIES_OFFSET + 2, 1);
        put_u32(dst, EXT4_DX_NODE_ENTRIES_OFFSET + 4, leaf_logical);
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
