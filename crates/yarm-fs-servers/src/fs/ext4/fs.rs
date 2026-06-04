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
const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
const EXT4_SUPPORTED_INCOMPAT: u32 = EXT4_FEATURE_INCOMPAT_FILETYPE
    | EXT4_FEATURE_INCOMPAT_EXTENTS
    | EXT4_FEATURE_INCOMPAT_64BIT
    | EXT4_FEATURE_INCOMPAT_FLEX_BG;

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
        let feature_incompat = le_u32(sb, 96)?;
        let unsupported = feature_incompat & !EXT4_SUPPORTED_INCOMPAT;
        if unsupported != 0 {
            return Err(Ext4ImageError::UnsupportedFeature(unsupported));
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
            feature_incompat,
            feature_ro_compat: le_u32(sb, 100)?,
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
        if sb.block_size() < 1024 || sb.blocks_per_group == 0 || sb.inodes_per_group == 0 {
            return Err(Ext4ImageError::Malformed);
        }
        let desc_size = if (sb.feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT) != 0 {
            le_u16(
                image
                    .get(EXT4_SUPERBLOCK_OFFSET..EXT4_SUPERBLOCK_OFFSET + 1024)
                    .ok_or(Ext4ImageError::Io)?,
                254,
            )
            .unwrap_or(64)
            .max(32)
        } else {
            32
        };
        Ok(Self {
            image,
            sb,
            desc_size,
        })
    }

    pub const fn superblock(&self) -> Ext4Superblock {
        self.sb
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
        Ok(lo | (hi << 32))
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
        let raw = self
            .image
            .get(off..off + self.sb.inode_size as usize)
            .ok_or(Ext4ImageError::Io)?;
        let size_lo = le_u32(raw, 4)? as u64;
        let size_hi = le_u32(raw, 108).unwrap_or(0) as u64;
        let mut block = [0u8; 60];
        block.copy_from_slice(raw.get(40..100).ok_or(Ext4ImageError::Malformed)?);
        Ok(ParsedInode {
            mode: le_u16(raw, 0)?,
            size: size_lo | (size_hi << 32),
            block,
        })
    }

    fn extents(&self, inode: &ParsedInode) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
        parse_extent_node(&inode.block, 0)
    }

    fn read_inode_bytes(&self, inode: u32) -> Result<alloc::vec::Vec<u8>, Ext4ImageError> {
        let inode = self.inode(inode)?;
        if inode.file_type() == Ext4FileType::Directory
            || inode.file_type() == Ext4FileType::Regular
        {
            let mut out = alloc::vec![0u8; inode.size as usize];
            for ex in self.extents(&inode)? {
                let src = self.block_offset(ex.start)?;
                let dst = ex.logical as usize * self.sb.block_size() as usize;
                let len = core::cmp::min(
                    ex.len as usize * self.sb.block_size() as usize,
                    out.len().saturating_sub(dst),
                );
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
        let inode = self.lookup_path(path)?;
        let meta = self.inode(inode)?;
        if meta.file_type() != Ext4FileType::Regular {
            return Err(Ext4ImageError::IsDirectory);
        }
        self.read_inode_bytes(inode)
    }

    pub fn read_dir(&self, path: &[u8]) -> Result<alloc::vec::Vec<Ext4DirEntry>, Ext4ImageError> {
        let inode = self.lookup_path(path)?;
        let meta = self.inode(inode)?;
        if meta.file_type() != Ext4FileType::Directory {
            return Err(Ext4ImageError::NotDirectory);
        }
        parse_dir_entries(self.read_inode_bytes(inode)?.as_slice())
    }

    pub fn lookup_path(&self, path: &[u8]) -> Result<u32, Ext4ImageError> {
        if path == b"/" || path.is_empty() {
            return Ok(2);
        }
        let mut current = 2u32;
        for comp in path.split(|b| *b == b'/').filter(|c| !c.is_empty()) {
            let inode = self.inode(current)?;
            if inode.file_type() != Ext4FileType::Directory {
                return Err(Ext4ImageError::NotDirectory);
            }
            let entries = parse_dir_entries(self.read_inode_bytes(current)?.as_slice())?;
            current = entries
                .iter()
                .find(|e| e.name() == comp)
                .map(|e| e.inode)
                .ok_or(Ext4ImageError::NotFound)?;
        }
        Ok(current)
    }
}

fn parse_extent_node(raw: &[u8], depth: u16) -> Result<alloc::vec::Vec<Extent>, Ext4ImageError> {
    if le_u16(raw, 0)? != 0xf30a {
        return Err(Ext4ImageError::UnsupportedLayout);
    }
    let entries = le_u16(raw, 2)? as usize;
    let actual_depth = le_u16(raw, 6)?;
    if actual_depth != depth || depth != 0 {
        return Err(Ext4ImageError::UnsupportedLayout);
    }
    let mut out = alloc::vec::Vec::new();
    for idx in 0..entries {
        let off = 12 + idx * 12;
        let logical = le_u32(raw, off)?;
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

    fn tiny_ext4_image() -> alloc::vec::Vec<u8> {
        let mut img = alloc::vec![0u8; 32 * 1024];
        let sb = EXT4_SUPERBLOCK_OFFSET;
        put_u32(&mut img, sb, 16); // inodes
        put_u32(&mut img, sb + 4, 32); // blocks
        put_u32(&mut img, sb + 24, 0); // 1KiB blocks
        put_u32(&mut img, sb + 32, 8192);
        put_u32(&mut img, sb + 40, 16);
        put_u16(&mut img, sb + 56, EXT4_MAGIC);
        put_u16(&mut img, sb + 88, 128);
        put_u32(
            &mut img,
            sb + 96,
            EXT4_FEATURE_INCOMPAT_FILETYPE | EXT4_FEATURE_INCOMPAT_EXTENTS,
        );
        put_u32(&mut img, 2 * 1024 + 8, 5); // inode table block
        write_inode(&mut img, 5 * 1024 + 128, 0x4000, 1024, 20);
        write_inode(&mut img, 5 * 1024 + 11 * 128, 0x8000, 16, 21);
        write_dirent(&mut img[20 * 1024..20 * 1024 + 12], 2, b".", 2, 12);
        write_dirent(&mut img[20 * 1024 + 12..20 * 1024 + 24], 2, b"..", 2, 12);
        write_dirent(
            &mut img[20 * 1024 + 24..21 * 1024],
            12,
            b"hello.txt",
            1,
            1000,
        );
        img[21 * 1024..21 * 1024 + 16].copy_from_slice(b"hello from ext4\n");
        img
    }

    fn write_inode(img: &mut [u8], off: usize, mode: u16, size: u32, extent_block: u32) {
        put_u16(img, off, mode);
        put_u32(img, off + 4, size);
        put_u16(img, off + 40, 0xf30a);
        put_u16(img, off + 42, 1);
        put_u16(img, off + 44, 4);
        put_u16(img, off + 46, 0);
        put_u32(img, off + 52, 0);
        put_u16(img, off + 56, 1);
        put_u16(img, off + 58, 0);
        put_u32(img, off + 60, extent_block);
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
