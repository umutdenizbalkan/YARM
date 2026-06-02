// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::super::common::vfs_ipc::{VfsBackend, VfsError};

const SECTOR_512: usize = 512;
const MAX_OPEN_FDS: usize = 32;
const MAX_PATH_COMPONENTS: usize = 32;
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LFN: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

pub const FAT_HELLO_PATH: &[u8] = b"/hello.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatLayout {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_entry_count: u16,
    pub total_sectors: u32,
    pub sectors_per_fat: u32,
    pub fat_start_lba: u32,
    pub root_dir_start_lba: u32,
    pub root_dir_sectors: u32,
    pub data_start_lba: u32,
    pub root_cluster: u32,
    pub cluster_count: u32,
    pub fat_type: FatType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatError {
    Io,
    Malformed,
    Unsupported,
    InvalidPath,
    NotFound,
    IsDirectory,
    NotDirectory,
    BadCluster,
    ClusterLoop,
}

impl From<FatError> for VfsError {
    fn from(value: FatError) -> Self {
        match value {
            FatError::Malformed | FatError::BadCluster | FatError::ClusterLoop => {
                VfsError::Malformed
            }
            FatError::Unsupported | FatError::Io => VfsError::Unsupported,
            FatError::InvalidPath
            | FatError::NotFound
            | FatError::IsDirectory
            | FatError::NotDirectory => VfsError::InvalidPath,
        }
    }
}

pub trait BlockDevice {
    fn len(&self) -> u64;
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<(), FatError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemBlockDevice {
    bytes: Vec<u8>,
}

impl MemBlockDevice {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl BlockDevice for MemBlockDevice {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<(), FatError> {
        let start = usize::try_from(offset).map_err(|_| FatError::Io)?;
        let end = start.checked_add(out.len()).ok_or(FatError::Io)?;
        let src = self.bytes.get(start..end).ok_or(FatError::Io)?;
        out.copy_from_slice(src);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcBlockDevice {
    pub device_id: u64,
    pub send_cap: u32,
    pub reply_recv_cap: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatBlockDevice {
    Mem(MemBlockDevice),
    Ipc(IpcBlockDevice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatBackendKind {
    MemoryImage,
    IpcBlock,
}

impl FatBlockDevice {
    pub const fn kind(&self) -> FatBackendKind {
        match self {
            Self::Mem(_) => FatBackendKind::MemoryImage,
            Self::Ipc(_) => FatBackendKind::IpcBlock,
        }
    }
}

impl BlockDevice for FatBlockDevice {
    fn len(&self) -> u64 {
        match self {
            Self::Mem(device) => device.len(),
            Self::Ipc(device) => device.len(),
        }
    }

    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<(), FatError> {
        match self {
            Self::Mem(device) => device.read_exact_at(offset, out),
            Self::Ipc(device) => device.read_exact_at(offset, out),
        }
    }
}

impl BlockDevice for IpcBlockDevice {
    fn len(&self) -> u64 {
        u64::MAX
    }

    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> Result<(), FatError> {
        use yarm_ipc_abi::block_abi::{
            BLK_IPC_MAX_DATA_BYTES, BLK_OP_READ, BLK_SECTOR_SIZE, BlkReadRequest, BlkStatus,
        };
        use yarm_user_rt::ipc::Message;
        if offset % u64::from(BLK_SECTOR_SIZE) != 0 || out.len() % BLK_SECTOR_SIZE as usize != 0 {
            return Err(FatError::Unsupported);
        }
        let mut done = 0usize;
        while done < out.len() {
            let chunk = core::cmp::min(out.len() - done, BLK_IPC_MAX_DATA_BYTES);
            let req = BlkReadRequest {
                device_id: self.device_id,
                lba: (offset + done as u64) / u64::from(BLK_SECTOR_SIZE),
                byte_len: chunk as u32,
                _reserved0: 0,
            };
            req.validate().map_err(|_| FatError::Unsupported)?;
            let mut payload = [0u8; 24];
            payload[0..8].copy_from_slice(&req.device_id.to_le_bytes());
            payload[8..16].copy_from_slice(&req.lba.to_le_bytes());
            payload[16..20].copy_from_slice(&req.byte_len.to_le_bytes());
            payload[20..24].copy_from_slice(&req._reserved0.to_le_bytes());
            let msg = Message::with_header(0, BLK_OP_READ, 0, None, &payload)
                .map_err(|_| FatError::Io)?;
            unsafe { yarm_user_rt::syscall::ipc_call(self.send_cap, self.reply_recv_cap, &msg) }
                .map_err(|_| FatError::Io)?;
            let reply =
                unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(self.reply_recv_cap, 0) }
                    .map_err(|_| FatError::Io)?
                    .ok_or(FatError::Io)?;
            let bytes = reply.as_slice();
            if bytes.len() < 8 {
                return Err(FatError::Io);
            }
            let status = u32::from_le_bytes(bytes[0..4].try_into().map_err(|_| FatError::Io)?);
            let bytes_read =
                u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| FatError::Io)?) as usize;
            if status != BlkStatus::Success as u32 || bytes_read != chunk || bytes.len() < 8 + chunk
            {
                return Err(FatError::Io);
            }
            out[done..done + chunk].copy_from_slice(&bytes[8..8 + chunk]);
            done += chunk;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    pub name: String,
    pub short_name: String,
    pub start_cluster: u32,
    pub size: u32,
    pub attr: u8,
}

impl DirEntryInfo {
    pub const fn is_dir(&self) -> bool {
        (self.attr & ATTR_DIRECTORY) != 0
    }
    pub const fn is_file(&self) -> bool {
        !self.is_dir()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenFd {
    fd: u64,
    entry: usize,
    offset: u64,
}

#[derive(Debug, Clone)]
pub struct FatFs<D: BlockDevice> {
    device: D,
    layout: FatLayout,
}

impl<D: BlockDevice> FatFs<D> {
    pub fn mount(device: D) -> Result<Self, FatError> {
        let mut boot = [0u8; SECTOR_512];
        device.read_exact_at(0, &mut boot)?;
        let layout = FatLayout::parse(&boot)?;
        let min_len = u64::from(layout.total_sectors) * u64::from(layout.bytes_per_sector);
        if device.len() < min_len {
            return Err(FatError::Malformed);
        }
        Ok(Self { device, layout })
    }

    pub const fn layout(&self) -> FatLayout {
        self.layout
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<(), FatError> {
        self.device.read_exact_at(offset, out)
    }

    fn sector_offset(&self, lba: u32) -> u64 {
        u64::from(lba) * u64::from(self.layout.bytes_per_sector)
    }

    fn cluster_lba(&self, cluster: u32) -> Result<u32, FatError> {
        if cluster < 2 || cluster - 2 >= self.layout.cluster_count {
            return Err(FatError::BadCluster);
        }
        self.layout
            .data_start_lba
            .checked_add((cluster - 2) * u32::from(self.layout.sectors_per_cluster))
            .ok_or(FatError::Malformed)
    }

    fn cluster_size(&self) -> usize {
        self.layout.bytes_per_sector as usize * self.layout.sectors_per_cluster as usize
    }

    fn read_cluster(&self, cluster: u32, out: &mut Vec<u8>) -> Result<(), FatError> {
        out.resize(self.cluster_size(), 0);
        self.read_at(
            self.sector_offset(self.cluster_lba(cluster)?),
            out.as_mut_slice(),
        )
    }

    pub fn fat_entry(&self, cluster: u32) -> Result<u32, FatError> {
        if cluster >= self.layout.cluster_count + 2 {
            return Err(FatError::BadCluster);
        }
        let fat_byte = match self.layout.fat_type {
            FatType::Fat12 => u64::from(cluster) + u64::from(cluster / 2),
            FatType::Fat16 => u64::from(cluster) * 2,
            FatType::Fat32 => u64::from(cluster) * 4,
        };
        let off = self
            .sector_offset(self.layout.fat_start_lba)
            .checked_add(fat_byte)
            .ok_or(FatError::Malformed)?;
        let mut raw = [0u8; 4];
        let needed = match self.layout.fat_type {
            FatType::Fat12 => 2,
            FatType::Fat16 => 2,
            FatType::Fat32 => 4,
        };
        self.read_at(off, &mut raw[..needed])?;
        let v = match self.layout.fat_type {
            FatType::Fat12 => {
                let word = u16::from_le_bytes([raw[0], raw[1]]) as u32;
                if cluster & 1 == 0 {
                    word & 0x0fff
                } else {
                    word >> 4
                }
            }
            FatType::Fat16 => u16::from_le_bytes([raw[0], raw[1]]) as u32,
            FatType::Fat32 => u32::from_le_bytes(raw) & 0x0fff_ffff,
        };
        Ok(v)
    }

    fn is_eoc(&self, v: u32) -> bool {
        match self.layout.fat_type {
            FatType::Fat12 => v >= 0x0ff8,
            FatType::Fat16 => v >= 0xfff8,
            FatType::Fat32 => v >= 0x0fff_fff8,
        }
    }

    fn is_bad(&self, v: u32) -> bool {
        match self.layout.fat_type {
            FatType::Fat12 => v == 0x0ff7,
            FatType::Fat16 => v == 0xfff7,
            FatType::Fat32 => v == 0x0fff_fff7,
        }
    }

    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, FatError> {
        let v = self.fat_entry(cluster)?;
        if self.is_eoc(v) {
            return Ok(None);
        }
        if v == 0 || self.is_bad(v) || v < 2 || v - 2 >= self.layout.cluster_count {
            return Err(FatError::BadCluster);
        }
        Ok(Some(v))
    }

    fn read_chain(&self, start: u32, max_bytes: Option<u64>) -> Result<Vec<u8>, FatError> {
        if start == 0 {
            return Ok(Vec::new());
        }
        let mut data = Vec::new();
        let mut cur = start;
        let limit = self.layout.cluster_count.saturating_add(1);
        let mut visited = Vec::new();
        for _ in 0..limit {
            if visited.contains(&cur) {
                return Err(FatError::ClusterLoop);
            }
            visited.push(cur);
            let mut cluster = Vec::new();
            self.read_cluster(cur, &mut cluster)?;
            if let Some(max) = max_bytes {
                let remain = max.saturating_sub(data.len() as u64) as usize;
                data.extend_from_slice(&cluster[..core::cmp::min(remain, cluster.len())]);
                if data.len() as u64 >= max {
                    break;
                }
            } else {
                data.extend_from_slice(&cluster);
            }
            match self.next_cluster(cur)? {
                Some(n) => cur = n,
                None => break,
            }
        }
        if visited.len() as u32 > limit {
            return Err(FatError::ClusterLoop);
        }
        Ok(data)
    }

    fn read_root_dir(&self) -> Result<Vec<u8>, FatError> {
        match self.layout.fat_type {
            FatType::Fat12 | FatType::Fat16 => {
                let len =
                    self.layout.root_dir_sectors as usize * self.layout.bytes_per_sector as usize;
                let mut out = vec![0u8; len];
                self.read_at(self.sector_offset(self.layout.root_dir_start_lba), &mut out)?;
                Ok(out)
            }
            FatType::Fat32 => self.read_chain(self.layout.root_cluster, None),
        }
    }

    fn parse_dir_entries(&self, bytes: &[u8]) -> Vec<DirEntryInfo> {
        let mut entries = Vec::new();
        let mut lfn_parts: Vec<(u8, String)> = Vec::new();
        let mut lfn_checksum: Option<u8> = None;
        for raw in bytes.chunks_exact(32) {
            if raw[0] == 0x00 {
                break;
            }
            if raw[0] == 0xe5 {
                lfn_parts.clear();
                lfn_checksum = None;
                continue;
            }
            let attr = raw[11];
            if attr == ATTR_LFN {
                let seq = raw[0] & 0x1f;
                let checksum = raw[13];
                if lfn_checksum.map(|c| c != checksum).unwrap_or(false) {
                    lfn_parts.clear();
                }
                lfn_checksum = Some(checksum);
                lfn_parts.push((seq, decode_lfn_part(raw)));
                continue;
            }
            if (attr & ATTR_VOLUME_ID) != 0 {
                lfn_parts.clear();
                lfn_checksum = None;
                continue;
            }
            let short = decode_short_name(raw);
            if short.is_empty() {
                lfn_parts.clear();
                lfn_checksum = None;
                continue;
            }
            let mut name = short.clone();
            if let Some(sum) = lfn_checksum {
                if lfn_checksum_valid(raw, sum) {
                    lfn_parts.sort_by_key(|(seq, _)| *seq);
                    let mut full = String::new();
                    for (_, part) in lfn_parts.iter() {
                        full.push_str(part);
                    }
                    if !full.is_empty() {
                        name = full;
                    }
                }
            }
            lfn_parts.clear();
            lfn_checksum = None;
            let lo = u16::from_le_bytes([raw[26], raw[27]]) as u32;
            let hi = u16::from_le_bytes([raw[20], raw[21]]) as u32;
            entries.push(DirEntryInfo {
                name,
                short_name: short,
                start_cluster: (hi << 16) | lo,
                size: u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]),
                attr,
            });
        }
        entries
    }

    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntryInfo>, FatError> {
        let entry = self.lookup(path)?;
        if !entry.is_dir() {
            return Err(FatError::NotDirectory);
        }
        let bytes = if entry.start_cluster == 0 {
            self.read_root_dir()?
        } else {
            self.read_chain(entry.start_cluster, None)?
        };
        Ok(self.parse_dir_entries(&bytes))
    }

    pub fn lookup(&self, path: &[u8]) -> Result<DirEntryInfo, FatError> {
        let comps = normalized_components(path)?;
        if comps.is_empty() {
            return Ok(DirEntryInfo {
                name: String::from("/"),
                short_name: String::from("/"),
                start_cluster: if self.layout.fat_type == FatType::Fat32 {
                    self.layout.root_cluster
                } else {
                    0
                },
                size: 0,
                attr: ATTR_DIRECTORY,
            });
        }
        let mut current_dir = self.read_root_dir()?;
        for (idx, comp) in comps.iter().enumerate() {
            let found = self
                .parse_dir_entries(&current_dir)
                .into_iter()
                .find(|e| name_eq(&e.name, comp) || name_eq(&e.short_name, comp));
            let entry = found.ok_or(FatError::NotFound)?;
            if idx == comps.len() - 1 {
                return Ok(entry);
            }
            if !entry.is_dir() {
                return Err(FatError::NotDirectory);
            }
            current_dir = self.read_chain(entry.start_cluster, None)?;
        }
        Err(FatError::NotFound)
    }

    pub fn read_file_at(
        &self,
        entry: &DirEntryInfo,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FatError> {
        if entry.is_dir() {
            return Err(FatError::IsDirectory);
        }
        if offset >= u64::from(entry.size) {
            return Ok(0);
        }
        let to_read = core::cmp::min(out.len() as u64, u64::from(entry.size) - offset) as usize;
        let data = self.read_chain(entry.start_cluster, Some(offset + to_read as u64))?;
        let start = offset as usize;
        out[..to_read].copy_from_slice(&data[start..start + to_read]);
        Ok(to_read)
    }
}

impl FatLayout {
    pub fn parse(boot: &[u8; SECTOR_512]) -> Result<Self, FatError> {
        if boot[510] != 0x55 || boot[511] != 0xaa {
            return Err(FatError::Malformed);
        }
        let bps = u16::from_le_bytes([boot[11], boot[12]]);
        if !matches!(bps, 512 | 1024 | 2048 | 4096) {
            return Err(FatError::Unsupported);
        }
        let spc = boot[13];
        if spc == 0 || !spc.is_power_of_two() {
            return Err(FatError::Malformed);
        }
        let reserved = u16::from_le_bytes([boot[14], boot[15]]);
        if reserved == 0 {
            return Err(FatError::Malformed);
        }
        let fats = boot[16];
        if fats == 0 {
            return Err(FatError::Malformed);
        }
        let root_entries = u16::from_le_bytes([boot[17], boot[18]]);
        let total16 = u16::from_le_bytes([boot[19], boot[20]]) as u32;
        let spf16 = u16::from_le_bytes([boot[22], boot[23]]) as u32;
        let total32 = u32::from_le_bytes([boot[32], boot[33], boot[34], boot[35]]);
        let spf32 = u32::from_le_bytes([boot[36], boot[37], boot[38], boot[39]]);
        let total = if total16 != 0 { total16 } else { total32 };
        if total == 0 {
            return Err(FatError::Malformed);
        }
        let spf = if spf16 != 0 { spf16 } else { spf32 };
        if spf == 0 {
            return Err(FatError::Malformed);
        }
        let root_dir_sectors = (u32::from(root_entries) * 32).div_ceil(u32::from(bps));
        let fat_start = u32::from(reserved);
        let root_dir_start = fat_start
            .checked_add(
                u32::from(fats)
                    .checked_mul(spf)
                    .ok_or(FatError::Malformed)?,
            )
            .ok_or(FatError::Malformed)?;
        let data_start = root_dir_start
            .checked_add(root_dir_sectors)
            .ok_or(FatError::Malformed)?;
        if data_start >= total {
            return Err(FatError::Malformed);
        }
        let data_sectors = total - data_start;
        let clusters = data_sectors / u32::from(spc);
        let fat_type = if clusters < 4085 {
            FatType::Fat12
        } else if clusters < 65525 {
            FatType::Fat16
        } else {
            FatType::Fat32
        };
        let root_cluster = if fat_type == FatType::Fat32 {
            u32::from_le_bytes([boot[44], boot[45], boot[46], boot[47]])
        } else {
            0
        };
        match fat_type {
            FatType::Fat32 => {
                if root_entries != 0 || root_cluster < 2 {
                    return Err(FatError::Malformed);
                }
            }
            FatType::Fat12 | FatType::Fat16 => {
                if root_entries == 0 {
                    return Err(FatError::Malformed);
                }
            }
        }
        Ok(Self {
            bytes_per_sector: bps,
            sectors_per_cluster: spc,
            reserved_sectors: reserved,
            fat_count: fats,
            root_entry_count: root_entries,
            total_sectors: total,
            sectors_per_fat: spf,
            fat_start_lba: fat_start,
            root_dir_start_lba: root_dir_start,
            root_dir_sectors,
            data_start_lba: data_start,
            root_cluster,
            cluster_count: clusters,
            fat_type,
        })
    }
}

#[derive(Debug, Clone)]
pub struct FatBackend {
    fs: FatFs<FatBlockDevice>,
    entries: Vec<DirEntryInfo>,
    open_fds: [Option<OpenFd>; MAX_OPEN_FDS],
    next_fd: u64,
}

impl Default for FatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FatBackend {
    pub fn new() -> Self {
        Self::from_image(build_sample_fat12_image()).expect("built-in FAT image mounts")
    }
    pub fn from_image(bytes: Vec<u8>) -> Result<Self, FatError> {
        Ok(Self {
            fs: FatFs::mount(FatBlockDevice::Mem(MemBlockDevice::new(bytes)))?,
            entries: Vec::new(),
            open_fds: [None; MAX_OPEN_FDS],
            next_fd: 300,
        })
    }
    pub fn from_ipc_block(
        device_id: u64,
        send_cap: u32,
        reply_recv_cap: u32,
    ) -> Result<Self, FatError> {
        Ok(Self {
            fs: FatFs::mount(FatBlockDevice::Ipc(IpcBlockDevice {
                device_id,
                send_cap,
                reply_recv_cap,
            }))?,
            entries: Vec::new(),
            open_fds: [None; MAX_OPEN_FDS],
            next_fd: 300,
        })
    }
    pub const fn layout(&self) -> FatLayout {
        self.fs.layout()
    }
    pub const fn backend_kind(&self) -> FatBackendKind {
        self.fs.device.kind()
    }
    pub fn lookup_entry(&self, path: &[u8]) -> Result<DirEntryInfo, FatError> {
        self.fs.lookup(path)
    }
    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntryInfo>, FatError> {
        self.fs.list_dir(path)
    }
    pub fn write_path(&mut self, _path: &[u8], _data: &[u8]) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
    pub fn mkdir_path(&mut self, _path: &[u8]) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
    pub fn unlink_path(&mut self, _path: &[u8]) -> Result<(), VfsError> {
        Err(VfsError::Unsupported)
    }
}

impl VfsBackend for FatBackend {
    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        let entry = self.fs.lookup(path).map_err(VfsError::from)?;
        if entry.is_dir() {
            return Err(VfsError::InvalidPath);
        }
        let fd = self.next_fd;
        let idx = self.entries.len();
        self.entries.push(entry);
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.open_fds.iter_mut().find(|s| s.is_none()) {
            *slot = Some(OpenFd {
                fd,
                entry: idx,
                offset: 0,
            });
            Ok(fd)
        } else {
            Err(VfsError::NoFd)
        }
    }
    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        if let Some(slot) = self
            .open_fds
            .iter_mut()
            .find(|s| s.map(|o| o.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(0)
        } else {
            Err(VfsError::BadFd)
        }
    }
    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        let mut buf = vec![0u8; core::cmp::min(len, 4096) as usize];
        let (n, _) = self.read_into(fd, len, &mut buf)?;
        Ok(n)
    }
    fn read_into(&mut self, fd: u64, len: u64, out: &mut [u8]) -> Result<(u64, usize), VfsError> {
        let slot_idx = self
            .open_fds
            .iter()
            .position(|s| s.map(|o| o.fd == fd).unwrap_or(false))
            .ok_or(VfsError::BadFd)?;
        let open = self.open_fds[slot_idx].ok_or(VfsError::BadFd)?;
        let entry = self.entries.get(open.entry).ok_or(VfsError::BadFd)?.clone();
        let max = core::cmp::min(len as usize, out.len());
        let n = self
            .fs
            .read_file_at(&entry, open.offset, &mut out[..max])
            .map_err(VfsError::from)?;
        if let Some(slot) = self.open_fds.get_mut(slot_idx).and_then(Option::as_mut) {
            slot.offset = slot.offset.saturating_add(n as u64);
        }
        Ok((n as u64, n))
    }
    fn write(&mut self, _fd: u64, _len: u64) -> Result<u64, VfsError> {
        Err(VfsError::Unsupported)
    }
    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        Ok(u64::from(
            self.fs.lookup(path).map_err(VfsError::from)?.size,
        ))
    }
}

fn normalized_components(path: &[u8]) -> Result<Vec<String>, FatError> {
    if path.is_empty() || path[0] != b'/' {
        return Err(FatError::InvalidPath);
    }
    let mut comps = Vec::new();
    for raw in path.split(|b| *b == b'/') {
        if raw.is_empty() || raw == b"." {
            continue;
        }
        if raw == b".." {
            comps.pop();
            continue;
        }
        if comps.len() >= MAX_PATH_COMPONENTS {
            return Err(FatError::InvalidPath);
        }
        comps.push(String::from_utf8_lossy(raw).into_owned());
    }
    Ok(comps)
}

fn name_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn decode_short_name(raw: &[u8]) -> String {
    let base = trim_fat_spaces(&raw[0..8]);
    let ext = trim_fat_spaces(&raw[8..11]);
    let mut s = String::from_utf8_lossy(base).into_owned();
    if !ext.is_empty() {
        s.push('.');
        s.push_str(&String::from_utf8_lossy(ext));
    }
    s
}

fn trim_fat_spaces(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }
    &bytes[..end]
}

fn decode_lfn_part(raw: &[u8]) -> String {
    let mut out = String::new();
    for off in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
        let c = u16::from_le_bytes([raw[off], raw[off + 1]]);
        if c == 0x0000 || c == 0xffff {
            break;
        }
        match char::from_u32(c as u32) {
            Some(ch) if !ch.is_control() => out.push(ch),
            _ => out.push('\u{fffd}'),
        }
    }
    out
}

fn lfn_checksum_valid(short_entry: &[u8], expected: u8) -> bool {
    let mut sum = 0u8;
    for b in &short_entry[0..11] {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(*b);
    }
    sum == expected
}

fn build_sample_fat12_image() -> Vec<u8> {
    let mut img = vec![0u8; 8 * SECTOR_512];
    format_boot(&mut img, FatType::Fat12, 8, 1, 1, 16, 1, 0);
    set_fat12(&mut img, 2, 0x0fff);
    write_dir_entry(
        &mut img[2 * SECTOR_512..2 * SECTOR_512 + 32],
        b"HELLO   TXT",
        ATTR_ARCHIVE,
        2,
        13,
    );
    img[3 * SECTOR_512..3 * SECTOR_512 + 13].copy_from_slice(b"Hello, FAT!\n\0");
    img
}

const ATTR_ARCHIVE: u8 = 0x20;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::common::vfs_ipc::VfsBackend;

    #[test]
    fn fat12_bpb_parse_succeeds() {
        assert_eq!(
            FatFs::mount(MemBlockDevice::new(image(FatType::Fat12)))
                .unwrap()
                .layout()
                .fat_type,
            FatType::Fat12
        );
    }
    #[test]
    fn fat16_bpb_parse_succeeds() {
        assert_eq!(
            FatFs::mount(MemBlockDevice::new(image(FatType::Fat16)))
                .unwrap()
                .layout()
                .fat_type,
            FatType::Fat16
        );
    }
    #[test]
    fn fat32_bpb_parse_succeeds() {
        assert_eq!(
            FatFs::mount(MemBlockDevice::new(image(FatType::Fat32)))
                .unwrap()
                .layout()
                .fat_type,
            FatType::Fat32
        );
    }
    #[test]
    fn invalid_bpb_rejected() {
        let mut img = image(FatType::Fat12);
        img[11] = 1;
        assert!(FatFs::mount(MemBlockDevice::new(img)).is_err());
    }
    #[test]
    fn root_file_lookup_short_83() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        assert_eq!(fs.lookup(b"/hello.txt").unwrap().size, 600);
    }
    #[test]
    fn root_file_read_single_cluster() {
        let mut b = FatBackend::from_image(image(FatType::Fat12)).unwrap();
        let fd = b.openat_path(b"/one.txt").unwrap();
        let mut out = [0u8; 8];
        assert_eq!(b.read_into(fd, 8, &mut out).unwrap(), (4, 4));
        assert_eq!(b.read_into(fd, 8, &mut out).unwrap(), (0, 0));
        assert_eq!(&out[..4], b"ONE\n");
    }
    #[test]
    fn file_read_across_two_clusters() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        let e = fs.lookup(b"/hello.txt").unwrap();
        let mut out = vec![0u8; 600];
        assert_eq!(fs.read_file_at(&e, 0, &mut out).unwrap(), 600);
        assert_eq!(out[0], b'A');
        assert_eq!(out[511], b'A');
        assert_eq!(out[512], b'B');
    }
    #[test]
    fn eof_clamped_read() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        let e = fs.lookup(b"/hello.txt").unwrap();
        let mut out = [0u8; 32];
        assert_eq!(fs.read_file_at(&e, 590, &mut out).unwrap(), 10);
        assert_eq!(fs.read_file_at(&e, 600, &mut out).unwrap(), 0);
    }
    #[test]
    fn subdirectory_lookup() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        assert_eq!(fs.lookup(b"/sub/inner.txt").unwrap().size, 5);
    }
    #[test]
    fn fat32_root_directory_cluster_chain_lookup() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat32))).unwrap();
        assert_eq!(fs.lookup(b"/next.txt").unwrap().size, 4);
    }
    #[test]
    fn lfn_lookup() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        assert_eq!(fs.lookup(b"/LongName.txt").unwrap().size, 7);
    }
    #[test]
    fn malformed_cluster_chain_loop_rejected() {
        let mut img = image(FatType::Fat12);
        set_fat12(&mut img, 2, 3);
        set_fat12(&mut img, 3, 2);
        img[2 * SECTOR_512 + 28..2 * SECTOR_512 + 32].copy_from_slice(&1200u32.to_le_bytes());
        let fs = FatFs::mount(MemBlockDevice::new(img)).unwrap();
        let e = fs.lookup(b"/hello.txt").unwrap();
        let mut out = vec![0u8; 1200];
        assert_eq!(fs.read_file_at(&e, 0, &mut out), Err(FatError::ClusterLoop));
    }
    #[test]
    fn bad_cluster_rejected() {
        let mut img = image(FatType::Fat12);
        set_fat12(&mut img, 2, 0x0ff7);
        let fs = FatFs::mount(MemBlockDevice::new(img)).unwrap();
        let e = fs.lookup(b"/hello.txt").unwrap();
        let mut out = vec![0u8; 600];
        assert_eq!(fs.read_file_at(&e, 0, &mut out), Err(FatError::BadCluster));
    }
    #[test]
    fn deleted_directory_entry_ignored() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        assert_eq!(fs.lookup(b"/deleted.txt"), Err(FatError::NotFound));
    }
    #[test]
    fn directory_stat_vs_file_stat() {
        let fs = FatFs::mount(MemBlockDevice::new(image(FatType::Fat12))).unwrap();
        assert!(fs.lookup(b"/sub").unwrap().is_dir());
        assert!(fs.lookup(b"/hello.txt").unwrap().is_file());
    }
    #[test]
    fn unsupported_write_mkdir_unlink_return_readonly_error() {
        let mut b = FatBackend::from_image(image(FatType::Fat12)).unwrap();
        let fd = b.openat_path(b"/hello.txt").unwrap();
        assert_eq!(b.write(fd, 1), Err(VfsError::Unsupported));
        assert_eq!(b.mkdir_path(b"/x"), Err(VfsError::Unsupported));
        assert_eq!(b.unlink_path(b"/hello.txt"), Err(VfsError::Unsupported));
    }

    fn image(kind: FatType) -> Vec<u8> {
        match kind {
            FatType::Fat12 => fat12_image(),
            FatType::Fat16 => fat16_image(),
            FatType::Fat32 => fat32_image(),
        }
    }

    fn fat12_image() -> Vec<u8> {
        let mut img = vec![0u8; 20 * SECTOR_512];
        format_boot(&mut img, FatType::Fat12, 20, 1, 1, 16, 1, 0);
        set_fat12(&mut img, 2, 3);
        set_fat12(&mut img, 3, 0x0fff);
        set_fat12(&mut img, 4, 0x0fff);
        set_fat12(&mut img, 5, 0x0fff);
        set_fat12(&mut img, 6, 0x0fff);
        let root = 2 * SECTOR_512;
        write_dir_entry(
            &mut img[root..root + 32],
            b"HELLO   TXT",
            ATTR_ARCHIVE,
            2,
            600,
        );
        write_dir_entry(
            &mut img[root + 32..root + 64],
            b"ONE     TXT",
            ATTR_ARCHIVE,
            4,
            4,
        );
        write_dir_entry(
            &mut img[root + 64..root + 96],
            b"SUB        ",
            ATTR_DIRECTORY,
            5,
            0,
        );
        write_lfn_pair(
            &mut img[root + 96..root + 160],
            "LongName.txt",
            b"LONGFI~1TXT",
            6,
            7,
        );
        img[root + 160] = 0xe5;
        write_dir_entry(
            &mut img[root + 160..root + 192],
            b"DELETE  TXT",
            ATTR_ARCHIVE,
            7,
            1,
        );
        let data = 3 * SECTOR_512;
        img[data..data + 512].fill(b'A');
        img[data + 512..data + 1024].fill(b'B');
        img[data + 2 * 512..data + 2 * 512 + 4].copy_from_slice(b"ONE\n");
        write_dir_entry(
            &mut img[data + 3 * 512..data + 3 * 512 + 32],
            b"INNER   TXT",
            ATTR_ARCHIVE,
            6,
            5,
        );
        img[data + 4 * 512..data + 4 * 512 + 7].copy_from_slice(b"LFN1234");
        img
    }
    fn fat16_image() -> Vec<u8> {
        let total = 4200usize;
        let mut img = vec![0u8; total * SECTOR_512];
        format_boot(&mut img, FatType::Fat16, total as u32, 1, 1, 512, 16, 0);
        set_fat16(&mut img, 2, 0xffff);
        img
    }
    fn fat32_image() -> Vec<u8> {
        let total = 70000usize;
        let mut img = vec![0u8; total * SECTOR_512];
        format_boot(&mut img, FatType::Fat32, total as u32, 1, 32, 0, 128, 2);
        set_fat32(&mut img, 2, 3);
        set_fat32(&mut img, 3, 0x0fff_ffff);
        set_fat32(&mut img, 4, 0x0fff_ffff);
        let data_start = 32 + 128;
        write_dir_entry(
            &mut img[data_start * 512..data_start * 512 + 32],
            b"ROOT    TXT",
            ATTR_ARCHIVE,
            4,
            4,
        );
        img[data_start * 512 + 32..(data_start + 1) * 512].fill(0xe5);
        write_dir_entry(
            &mut img[(data_start + 1) * 512..(data_start + 1) * 512 + 32],
            b"NEXT    TXT",
            ATTR_ARCHIVE,
            4,
            4,
        );
        img
    }
}

#[cfg(test)]
fn write_lfn_pair(dst: &mut [u8], long: &str, short: &[u8; 11], cluster: u32, size: u32) {
    let sum = {
        let mut s = 0u8;
        for b in short {
            s = ((s & 1) << 7).wrapping_add(s >> 1).wrapping_add(*b);
        }
        s
    };
    dst[..64].fill(0);
    dst[0] = 0x41;
    dst[11] = ATTR_LFN;
    dst[13] = sum;
    let utf: Vec<u16> = long.encode_utf16().collect();
    for (i, off) in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
        .iter()
        .copied()
        .enumerate()
    {
        let c = utf.get(i).copied().unwrap_or(0xffff);
        dst[off..off + 2].copy_from_slice(&c.to_le_bytes());
    }
    write_dir_entry(&mut dst[32..64], short, ATTR_ARCHIVE, cluster, size);
}

fn write_dir_entry(dst: &mut [u8], name: &[u8; 11], attr: u8, cluster: u32, size: u32) {
    dst[..32].fill(0);
    dst[0..11].copy_from_slice(name);
    dst[11] = attr;
    dst[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    dst[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    dst[28..32].copy_from_slice(&size.to_le_bytes());
}

fn format_boot(
    img: &mut [u8],
    kind: FatType,
    total: u32,
    spc: u8,
    reserved: u16,
    root_entries: u16,
    spf: u32,
    root_cluster: u32,
) {
    img[0] = 0xeb;
    img[1] = 0x3c;
    img[2] = 0x90;
    img[3..11].copy_from_slice(b"YARMFAT ");
    img[11..13].copy_from_slice(&(SECTOR_512 as u16).to_le_bytes());
    img[13] = spc;
    img[14..16].copy_from_slice(&reserved.to_le_bytes());
    img[16] = 1;
    img[17..19].copy_from_slice(&root_entries.to_le_bytes());
    if total <= 0xffff {
        img[19..21].copy_from_slice(&(total as u16).to_le_bytes());
    } else {
        img[32..36].copy_from_slice(&total.to_le_bytes());
    }
    img[21] = 0xf8;
    if kind == FatType::Fat32 {
        img[36..40].copy_from_slice(&spf.to_le_bytes());
        img[44..48].copy_from_slice(&root_cluster.to_le_bytes());
    } else {
        img[22..24].copy_from_slice(&(spf as u16).to_le_bytes());
    }
    img[510] = 0x55;
    img[511] = 0xaa;
}

fn set_fat12(img: &mut [u8], cluster: u32, value: u32) {
    let off = SECTOR_512 + cluster as usize + cluster as usize / 2;
    let mut word = u16::from_le_bytes([img[off], img[off + 1]]);
    if cluster & 1 == 0 {
        word = (word & 0xf000) | (value as u16 & 0x0fff);
    } else {
        word = (word & 0x000f) | ((value as u16 & 0x0fff) << 4);
    }
    img[off..off + 2].copy_from_slice(&word.to_le_bytes());
}
#[cfg(test)]
fn set_fat16(img: &mut [u8], cluster: u32, value: u32) {
    let off = SECTOR_512 + cluster as usize * 2;
    img[off..off + 2].copy_from_slice(&(value as u16).to_le_bytes());
}
#[cfg(test)]
fn set_fat32(img: &mut [u8], cluster: u32, value: u32) {
    let off = 32 * SECTOR_512 + cluster as usize * 4;
    img[off..off + 4].copy_from_slice(&(value & 0x0fff_ffff).to_le_bytes());
}
