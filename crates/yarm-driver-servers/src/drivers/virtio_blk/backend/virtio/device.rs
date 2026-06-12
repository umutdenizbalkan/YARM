// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::drivers::virtio_blk::service::{BlockDeviceInfo, BlockDeviceOps};

const VIRTQ_DEPTH: usize = 16;

pub const VIRTIO_BLK_OP_READ: u16 = 1;
pub const VIRTIO_BLK_OP_WRITE: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkReqFrame {
    pub op: u16,
    pub _reserved: u16,
    pub sector: u64,
    pub len: u32,
    pub tag: u32,
}

impl VirtioBlkReqFrame {
    pub const ENCODED_LEN: usize = 20;

    pub fn decode(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(());
        }
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
        Ok(Self {
            op: u16::from_le_bytes(op),
            _reserved: u16::from_le_bytes(reserved),
            sector: u64::from_le_bytes(sector),
            len: u32::from_le_bytes(len),
            tag: u32::from_le_bytes(tag),
        })
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..2].copy_from_slice(&self.op.to_le_bytes());
        out[2..4].copy_from_slice(&self._reserved.to_le_bytes());
        out[4..12].copy_from_slice(&self.sector.to_le_bytes());
        out[12..16].copy_from_slice(&self.len.to_le_bytes());
        out[16..20].copy_from_slice(&self.tag.to_le_bytes());
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkRespFrame {
    pub status: u8,
    pub _pad: [u8; 3],
    pub done_len: u32,
    pub tag: u32,
}

impl VirtioBlkRespFrame {
    pub const ENCODED_LEN: usize = 12;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0] = self.status;
        out[1..4].copy_from_slice(&self._pad);
        out[4..8].copy_from_slice(&self.done_len.to_le_bytes());
        out[8..12].copy_from_slice(&self.tag.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(());
        }
        let mut done_len = [0u8; 4];
        done_len.copy_from_slice(&bytes[4..8]);
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&bytes[8..12]);
        Ok(Self {
            status: bytes[0],
            _pad: [bytes[1], bytes[2], bytes[3]],
            done_len: u32::from_le_bytes(done_len),
            tag: u32::from_le_bytes(tag),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtqDescRole {
    Header,
    Data,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtqDescriptor {
    pub role: VirtqDescRole,
    pub len: u32,
    pub tag: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtqChain {
    pub head: VirtqDescriptor,
    pub data: VirtqDescriptor,
    pub status: VirtqDescriptor,
    pub request: VirtioBlkReqFrame,
}

impl VirtqChain {
    pub const fn from_request(request: VirtioBlkReqFrame) -> Self {
        Self {
            head: VirtqDescriptor {
                role: VirtqDescRole::Header,
                len: VirtioBlkReqFrame::ENCODED_LEN as u32,
                tag: request.tag,
            },
            data: VirtqDescriptor {
                role: VirtqDescRole::Data,
                len: request.len,
                tag: request.tag,
            },
            status: VirtqDescriptor {
                role: VirtqDescRole::Status,
                len: VirtioBlkRespFrame::ENCODED_LEN as u32,
                tag: request.tag,
            },
            request,
        }
    }
}

pub fn build_write_chain(tag: u32, sector: u64, len: u32) -> VirtqChain {
    VirtqChain::from_request(VirtioBlkReqFrame {
        op: VIRTIO_BLK_OP_WRITE,
        _reserved: 0,
        sector,
        len,
        tag,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkRequest {
    pub sector: u64,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkDevice {
    pub sectors: u64,
    pub sector_size: u32,
    pub reads: u64,
    pub writes: u64,
}

impl Default for VirtioBlkDevice {
    fn default() -> Self {
        Self::new(4096, 512)
    }
}

impl VirtioBlkDevice {
    pub const fn new(sectors: u64, sector_size: u32) -> Self {
        Self {
            sectors,
            sector_size,
            reads: 0,
            writes: 0,
        }
    }

    pub fn read(&mut self, req: VirtioBlkRequest) -> Result<u32, ()> {
        if req.sector >= self.sectors {
            return Err(());
        }
        self.reads = self.reads.saturating_add(1);
        Ok(req.len)
    }

    pub fn write(&mut self, req: VirtioBlkRequest) -> Result<u32, ()> {
        if req.sector >= self.sectors || req.len == 0 || !req.len.is_multiple_of(self.sector_size) {
            return Err(());
        }
        let sector_count = (req.len / self.sector_size) as u64;
        req.sector
            .checked_add(sector_count)
            .filter(|end| *end <= self.sectors)
            .ok_or(())?;
        self.writes = self.writes.saturating_add(1);
        Ok(req.len)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBlkMemoryDevice<const SECTORS: usize> {
    storage: [[u8; 512]; SECTORS],
    pub device: VirtioBlkDevice,
}

impl<const SECTORS: usize> Default for VirtioBlkMemoryDevice<SECTORS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SECTORS: usize> VirtioBlkMemoryDevice<SECTORS> {
    pub const fn new() -> Self {
        Self {
            storage: [[0; 512]; SECTORS],
            device: VirtioBlkDevice::new(SECTORS as u64, 512),
        }
    }

    pub fn write_sector(&mut self, sector: u64, data: &[u8; 512]) -> Result<u32, ()> {
        self.device.write(VirtioBlkRequest { sector, len: 512 })?;
        let slot = self.storage.get_mut(sector as usize).ok_or(())?;
        *slot = *data;
        Ok(512)
    }

    pub fn read_sector(&mut self, sector: u64) -> Result<[u8; 512], ()> {
        self.device.read(VirtioBlkRequest { sector, len: 512 })?;
        self.storage.get(sector as usize).copied().ok_or(())
    }
}

impl<const SECTORS: usize> BlockDeviceOps for VirtioBlkMemoryDevice<SECTORS> {
    fn get_info(&self) -> BlockDeviceInfo {
        BlockDeviceInfo {
            logical_block_size: 512,
            total_blocks: SECTORS as u64,
            feature_flags: 0,
        }
    }

    fn read_sector(&mut self, lba: u64) -> Result<[u8; 512], ()> {
        VirtioBlkMemoryDevice::read_sector(self, lba)
    }

    fn write_sector(&mut self, lba: u64, data: &[u8; 512]) -> Result<u32, ()> {
        VirtioBlkMemoryDevice::write_sector(self, lba, data)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtQueue {
    chains: [Option<VirtqChain>; VIRTQ_DEPTH],
    used: [Option<VirtioBlkRespFrame>; VIRTQ_DEPTH],
    avail_idx: u16,
    used_idx: u16,
}

impl Default for VirtQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtQueue {
    pub const fn new() -> Self {
        Self {
            chains: [None; VIRTQ_DEPTH],
            used: [None; VIRTQ_DEPTH],
            avail_idx: 0,
            used_idx: 0,
        }
    }

    pub const fn avail_idx(&self) -> u16 {
        self.avail_idx
    }

    pub const fn used_idx(&self) -> u16 {
        self.used_idx
    }

    pub fn push_chain(&mut self, chain: VirtqChain) -> Result<(), ()> {
        let idx = (self.avail_idx as usize) % VIRTQ_DEPTH;
        if self.chains[idx].is_some() {
            return Err(());
        }
        self.chains[idx] = Some(chain);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        Ok(())
    }

    pub fn pop_next_chain(&mut self) -> Option<VirtqChain> {
        let idx = (self.used_idx as usize) % VIRTQ_DEPTH;
        self.chains[idx].take()
    }

    pub fn push_used(&mut self, resp: VirtioBlkRespFrame) {
        let idx = (self.used_idx as usize) % VIRTQ_DEPTH;
        self.used[idx] = Some(resp);
        self.used_idx = self.used_idx.wrapping_add(1);
    }

    pub fn take_last_used(&mut self) -> Option<VirtioBlkRespFrame> {
        if self.used_idx == 0 {
            return None;
        }
        let idx = ((self.used_idx - 1) as usize) % VIRTQ_DEPTH;
        self.used[idx].take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_golden_vector_stable() {
        let frame = VirtioBlkReqFrame {
            op: VIRTIO_BLK_OP_READ,
            _reserved: 0,
            sector: 7,
            len: 512,
            tag: 99,
        };
        let expected: [u8; 20] = [1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 99, 0, 0, 0];
        assert_eq!(frame.encode(), expected);
        assert_eq!(VirtioBlkReqFrame::decode(&expected).expect("decode"), frame);
    }

    #[test]
    fn response_golden_vector_stable() {
        let resp = VirtioBlkRespFrame {
            status: 0,
            _pad: [0; 3],
            done_len: 512,
            tag: 99,
        };
        let expected: [u8; 12] = [0, 0, 0, 0, 0, 2, 0, 0, 99, 0, 0, 0];
        assert_eq!(resp.encode(), expected);
        assert_eq!(VirtioBlkRespFrame::decode(&expected).expect("decode"), resp);
    }

    #[test]
    fn virtqueue_supports_three_descriptor_chain() {
        let mut q = VirtQueue::new();
        let req = VirtioBlkReqFrame {
            op: VIRTIO_BLK_OP_READ,
            _reserved: 0,
            sector: 1,
            len: 64,
            tag: 1,
        };
        q.push_chain(VirtqChain::from_request(req)).expect("push");
        let chain = q.pop_next_chain().expect("pop");
        assert_eq!(chain.head.role, VirtqDescRole::Header);
        assert_eq!(chain.data.role, VirtqDescRole::Data);
        assert_eq!(chain.status.role, VirtqDescRole::Status);
        q.push_used(VirtioBlkRespFrame {
            status: 0,
            _pad: [0; 3],
            done_len: chain.request.len,
            tag: chain.request.tag,
        });
        assert_eq!(q.take_last_used().expect("used").done_len, 64);
    }

    #[test]
    fn write_request_builder_uses_virtio_write_opcode() {
        let chain = build_write_chain(0x44, 7, 512);
        assert_eq!(chain.request.op, VIRTIO_BLK_OP_WRITE);
        assert_eq!(chain.request.sector, 7);
        assert_eq!(chain.request.len, 512);
        assert_eq!(chain.data.len, 512);
    }

    #[test]
    fn memory_device_write_then_read_and_overwrite_are_exact() {
        let mut device = VirtioBlkMemoryDevice::<4>::new();
        let first = [0x5a; 512];
        let second = core::array::from_fn(|index| index as u8);
        assert_eq!(device.write_sector(2, &first), Ok(512));
        assert_eq!(device.read_sector(2), Ok(first));
        assert_eq!(device.write_sector(2, &second), Ok(512));
        assert_eq!(device.read_sector(2), Ok(second));
        assert_eq!(device.write_sector(4, &first), Err(()));
    }
}
