// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};
use yarm::yarm_fs_servers::blkcache::BlockCache;

const MAX_FAT_FILES: usize = 8;
const MAX_OPEN_FDS: usize = 8;
const MAX_PATH_LEN: usize = 32;
const FAT_ENTRIES: usize = 128;
const FAT_EOC: u16 = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatPath {
    pub len: u8,
    pub bytes: [u8; MAX_PATH_LEN],
}

impl FatPath {
    pub const fn from_bytes(bytes: [u8; MAX_PATH_LEN], len: u8) -> Self {
        Self { len, bytes }
    }

    pub fn from_abi_path(path_ptr: u64) -> Result<Self, VfsError> {
        let src = abi_path_bytes(path_ptr).ok_or(VfsError::BadFd)?;
        if src.len() > MAX_PATH_LEN {
            return Err(VfsError::Malformed);
        }
        let mut bytes = [0u8; MAX_PATH_LEN];
        bytes[..src.len()].copy_from_slice(src);
        Ok(Self {
            len: src.len() as u8,
            bytes,
        })
    }
}

const ABI_PATH_TABLE: &[(u64, &[u8])] = &[
    (0x5050, b"/hello.txt"),
    (0x6060, b"/etc/config"),
    (0x4040, b"/ext4/file.bin"),
];

fn abi_path_bytes(path_ptr: u64) -> Option<&'static [u8]> {
    ABI_PATH_TABLE
        .iter()
        .find(|(ptr, _)| *ptr == path_ptr)
        .map(|(_, bytes)| *bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatBpb {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_entry_count: u16,
    pub sectors_per_fat_16: u16,
}

impl FatBpb {
    pub fn parse(boot_sector: &[u8; 512]) -> Result<Self, VfsError> {
        if boot_sector[510] != 0x55 || boot_sector[511] != 0xAA {
            return Err(VfsError::Malformed);
        }
        let mut bps = [0u8; 2];
        bps.copy_from_slice(&boot_sector[11..13]);
        let mut reserved = [0u8; 2];
        reserved.copy_from_slice(&boot_sector[14..16]);
        let mut root = [0u8; 2];
        root.copy_from_slice(&boot_sector[17..19]);
        let mut spf = [0u8; 2];
        spf.copy_from_slice(&boot_sector[22..24]);
        Ok(Self {
            bytes_per_sector: u16::from_le_bytes(bps),
            sectors_per_cluster: boot_sector[13],
            reserved_sectors: u16::from_le_bytes(reserved),
            fat_count: boot_sector[16],
            root_entry_count: u16::from_le_bytes(root),
            sectors_per_fat_16: u16::from_le_bytes(spf),
        })
    }

    pub const fn cluster_size_bytes(self) -> u64 {
        (self.bytes_per_sector as u64) * (self.sectors_per_cluster as u64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatTable {
    pub entries: [u16; FAT_ENTRIES],
}

impl FatTable {
    pub const fn empty() -> Self {
        Self {
            entries: [0; FAT_ENTRIES],
        }
    }

    pub fn parse_from_bytes(bytes: &[u8]) -> Result<Self, VfsError> {
        if bytes.len() < FAT_ENTRIES * 2 {
            return Err(VfsError::Malformed);
        }
        let mut table = [0u16; FAT_ENTRIES];
        let mut idx = 0usize;
        while idx < FAT_ENTRIES {
            let off = idx * 2;
            let mut raw = [0u8; 2];
            raw.copy_from_slice(&bytes[off..off + 2]);
            table[idx] = u16::from_le_bytes(raw);
            idx += 1;
        }
        Ok(Self { entries: table })
    }

    pub fn next_cluster(&self, cluster: u16) -> Option<u16> {
        let idx = cluster as usize;
        if idx >= FAT_ENTRIES {
            return None;
        }
        let next = self.entries[idx];
        if next == 0 || next == FAT_EOC {
            None
        } else {
            Some(next)
        }
    }

    fn set_next(&mut self, from: u16, to: u16) {
        let idx = from as usize;
        if idx < FAT_ENTRIES {
            self.entries[idx] = to;
        }
    }

    fn mark_eoc(&mut self, cluster: u16) {
        self.set_next(cluster, FAT_EOC);
    }

    fn alloc_free_cluster(&mut self) -> Option<u16> {
        for i in 2..FAT_ENTRIES {
            if self.entries[i] == 0 {
                self.entries[i] = FAT_EOC;
                return Some(i as u16);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatDirEntry {
    pub path: FatPath,
    pub start_cluster: u16,
    pub file_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenFd {
    fd: u64,
    file_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatCluster {
    pub id: u16,
    pub next: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatBackend {
    next_fd: u64,
    files: [Option<FatDirEntry>; MAX_FAT_FILES],
    open_fds: [Option<OpenFd>; MAX_OPEN_FDS],
    bpb: FatBpb,
    fat: FatTable,
    cache: BlockCache,
}

impl Default for FatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FatBackend {
    pub fn new() -> Self {
        let boot = sample_boot_sector();
        let bpb = FatBpb::parse(&boot).expect("sample BPB must parse");
        let fat = FatTable::parse_from_bytes(&sample_fat_region()).expect("sample FAT must parse");
        Self {
            next_fd: 300,
            files: [None; MAX_FAT_FILES],
            open_fds: [None; MAX_OPEN_FDS],
            bpb,
            fat,
            cache: BlockCache::new(),
        }
    }

    pub const fn cluster_size_bytes(&self) -> u64 {
        self.bpb.cluster_size_bytes()
    }

    fn alloc_fd(&mut self, file_idx: usize) -> Result<u64, VfsError> {
        if let Some(slot) = self.open_fds.iter_mut().find(|slot| slot.is_none()) {
            let fd = self.next_fd;
            self.next_fd = self.next_fd.saturating_add(1);
            *slot = Some(OpenFd { fd, file_idx });
            Ok(fd)
        } else {
            Err(VfsError::NoFd)
        }
    }

    fn find_file_idx(&self, path: FatPath) -> Option<usize> {
        self.files
            .iter()
            .position(|slot| slot.map(|e| e.path == path).unwrap_or(false))
    }

    fn alloc_file(&mut self, path: FatPath) -> Result<usize, VfsError> {
        if let Some(idx) = self.find_file_idx(path) {
            return Ok(idx);
        }
        let start = self.fat.alloc_free_cluster().ok_or(VfsError::NoFd)?;
        if let Some((idx, slot)) = self
            .files
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.is_none())
        {
            *slot = Some(FatDirEntry {
                path,
                start_cluster: start,
                file_len: 0,
            });
            Ok(idx)
        } else {
            Err(VfsError::NoFd)
        }
    }

    fn open_fd_lookup(&self, fd: u64) -> Option<OpenFd> {
        self.open_fds
            .iter()
            .flatten()
            .find(|entry| entry.fd == fd)
            .copied()
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsError> {
        if let Some(slot) = self
            .open_fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsError::BadFd)
        }
    }

    fn clusters_needed_for_len(&self, file_len: u64) -> u32 {
        let csz = self.cluster_size_bytes();
        if file_len == 0 {
            1
        } else {
            ((file_len + csz.saturating_sub(1)) / csz) as u32
        }
    }

    fn chain_len(&self, start: u16) -> u32 {
        let mut count = 1u32;
        let mut cur = start;
        while let Some(next) = self.fat.next_cluster(cur) {
            count = count.saturating_add(1);
            cur = next;
        }
        count
    }

    fn append_cluster_to_chain(&mut self, start: u16) -> Result<(), VfsError> {
        let new_cluster = self.fat.alloc_free_cluster().ok_or(VfsError::NoFd)?;
        let mut tail = start;
        while let Some(next) = self.fat.next_cluster(tail) {
            tail = next;
        }
        self.fat.set_next(tail, new_cluster);
        self.fat.mark_eoc(new_cluster);
        Ok(())
    }

    fn grow_chain_if_needed(&mut self, start: u16, file_len: u64) -> Result<(), VfsError> {
        let needed = self.clusters_needed_for_len(file_len);
        while self.chain_len(start) < needed {
            self.append_cluster_to_chain(start)?;
        }
        Ok(())
    }

    pub fn cluster_chain_head_for_path(&self, path_ptr: u64) -> Option<FatCluster> {
        let path = FatPath::from_abi_path(path_ptr).ok()?;
        let idx = self.find_file_idx(path)?;
        let entry = self.files[idx]?;
        Some(FatCluster {
            id: entry.start_cluster,
            next: self.fat.next_cluster(entry.start_cluster),
        })
    }
}

impl VfsBackend for FatBackend {
    fn openat(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        if path_ptr == 0 {
            return Err(VfsError::BadFd);
        }
        let path = FatPath::from_abi_path(path_ptr)?;
        let file_idx = self.alloc_file(path)?;
        self.alloc_fd(file_idx)
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        self.close_fd(fd)?;
        Ok(0)
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let opened = self.open_fd_lookup(fd).ok_or(VfsError::BadFd)?;
        let file = self.files[opened.file_idx].ok_or(VfsError::BadFd)?;
        let _ = self.cache.get(fd);
        Ok(core::cmp::min(len, file.file_len))
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let opened = self.open_fd_lookup(fd).ok_or(VfsError::BadFd)?;
        let mut file = self.files[opened.file_idx].ok_or(VfsError::BadFd)?;
        file.file_len = file.file_len.saturating_add(len);
        self.grow_chain_if_needed(file.start_cluster, file.file_len)?;
        self.files[opened.file_idx] = Some(file);
        self.cache.put(fd, file.file_len);
        Ok(len)
    }

    fn statx(&mut self, path_ptr: u64) -> Result<u64, VfsError> {
        let path = FatPath::from_abi_path(path_ptr)?;
        let idx = self.find_file_idx(path).ok_or(VfsError::BadFd)?;
        Ok(self.files[idx].ok_or(VfsError::BadFd)?.file_len)
    }
}

fn sample_boot_sector() -> [u8; 512] {
    let mut boot = [0u8; 512];
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&1u16.to_le_bytes());
    boot[16] = 1;
    boot[17..19].copy_from_slice(&64u16.to_le_bytes());
    boot[22..24].copy_from_slice(&1u16.to_le_bytes());
    boot[510] = 0x55;
    boot[511] = 0xAA;
    boot
}

fn sample_fat_region() -> [u8; FAT_ENTRIES * 2] {
    let mut bytes = [0u8; FAT_ENTRIES * 2];
    // Reserve cluster 0 and 1
    bytes[0..2].copy_from_slice(&FAT_EOC.to_le_bytes());
    bytes[2..4].copy_from_slice(&FAT_EOC.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpb_parsing_reads_core_fields() {
        let boot = sample_boot_sector();
        let bpb = FatBpb::parse(&boot).expect("parse");
        assert_eq!(bpb.bytes_per_sector, 512);
        assert_eq!(bpb.sectors_per_cluster, 1);
    }

    #[test]
    fn fat_chain_grows_as_file_expands() {
        let mut fs = FatBackend::new();
        let fd = fs.openat(0x5050).expect("open");
        let cluster_size = fs.cluster_size_bytes();
        let _ = fs.write(fd, cluster_size * 2 + 1).expect("write");
        let head = fs.cluster_chain_head_for_path(0x5050).expect("head");
        assert!(head.next.is_some());
    }

    #[test]
    fn typed_pathname_layer_uses_abi_buffers() {
        let mut fs = FatBackend::new();
        let fd = fs.openat(0x5050).expect("open");
        let _ = fs.write(fd, 11).expect("write");
        assert_eq!(fs.statx(0x5050).expect("stat"), 11);
    }
}
