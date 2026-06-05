// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::mem::{align_of, size_of};

/// Must remain in sync with `yarm_kernel::ipc::Message::MAX_PAYLOAD`.
pub const IPC_INLINE_PAYLOAD_MAX_BYTES: usize = 128;
pub const BLK_SECTOR_SIZE: u32 = 512;

pub const BLK_OP_GET_INFO: u16 = 0x0201;
pub const BLK_OP_READ: u16 = 0x0202;
pub const BLK_OP_WRITE: u16 = 0x0203;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum BlkStatus {
    Success = 0,
    InvalidAlignment = 1,
    OversizedRequest = 2,
    DeviceUnavailable = 3,
    NotReady = 4,
    IOError = 5,
    InvalidRequest = 6,
}

/// Read reply carries status + bytes_read + inline bytes.
/// This is the largest 8-byte-aligned payload that fits into current inline IPC bytes.
pub const BLK_IPC_MAX_DATA_BYTES: usize =
    (IPC_INLINE_PAYLOAD_MAX_BYTES - size_of::<u32>() - size_of::<u32>()) & !7usize;

#[repr(C, align(8))]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkGetInfoRequest {
    pub device_id: u64,
}
impl BlkGetInfoRequest {
    pub const ENCODED_LEN: usize = 8;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.device_id.to_le_bytes()
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            device_id: u64::from_le_bytes(b.try_into().ok()?),
        })
    }
}

#[repr(C, align(8))]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkGetInfoReply {
    pub status: BlkStatus,
    pub _reserved0: u32,
    pub logical_block_size: u32,
    pub _reserved1: u32,
    pub total_blocks: u64,
    pub feature_flags: u64,
}
impl BlkGetInfoReply {
    pub const ENCODED_LEN: usize = 32;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&(self.status as u32).to_le_bytes());
        out[4..8].copy_from_slice(&self._reserved0.to_le_bytes());
        out[8..12].copy_from_slice(&self.logical_block_size.to_le_bytes());
        out[12..16].copy_from_slice(&self._reserved1.to_le_bytes());
        out[16..24].copy_from_slice(&self.total_blocks.to_le_bytes());
        out[24..32].copy_from_slice(&self.feature_flags.to_le_bytes());
        out
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        let status = decode_status(u32::from_le_bytes(b[0..4].try_into().ok()?))?;
        Some(Self {
            status,
            _reserved0: u32::from_le_bytes(b[4..8].try_into().ok()?),
            logical_block_size: u32::from_le_bytes(b[8..12].try_into().ok()?),
            _reserved1: u32::from_le_bytes(b[12..16].try_into().ok()?),
            total_blocks: u64::from_le_bytes(b[16..24].try_into().ok()?),
            feature_flags: u64::from_le_bytes(b[24..32].try_into().ok()?),
        })
    }
}

/// Inline write chunks reserve 32 bytes for metadata and use the rest of the IPC payload for data.
/// A complete logical-sector transaction is therefore split into ordered chunks.
pub const BLK_WRITE_HEADER_BYTES: usize = 32;
pub const BLK_WRITE_MAX_CHUNK_BYTES: usize = IPC_INLINE_PAYLOAD_MAX_BYTES - BLK_WRITE_HEADER_BYTES;
pub const BLK_WRITE_F_FIRST: u32 = 1 << 0;
pub const BLK_WRITE_F_LAST: u32 = 1 << 1;
pub const BLK_WRITE_F_KNOWN: u32 = BLK_WRITE_F_FIRST | BLK_WRITE_F_LAST;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkWriteRequest {
    pub request_id: u32,
    pub flags: u32,
    pub device_id: u64,
    pub lba: u64,
    pub sector_offset: u32,
    pub data_len: u32,
    pub data: [u8; BLK_WRITE_MAX_CHUNK_BYTES],
}

impl BlkWriteRequest {
    pub const ENCODED_LEN: usize = IPC_INLINE_PAYLOAD_MAX_BYTES;

    pub fn validate(&self) -> Result<(), BlkStatus> {
        if self.request_id == 0 || self.flags & !BLK_WRITE_F_KNOWN != 0 {
            return Err(BlkStatus::InvalidRequest);
        }
        let data_len = self.data_len as usize;
        if data_len == 0 || data_len > BLK_WRITE_MAX_CHUNK_BYTES {
            return Err(BlkStatus::OversizedRequest);
        }
        let end = self
            .sector_offset
            .checked_add(self.data_len)
            .ok_or(BlkStatus::InvalidRequest)?;
        if end > BLK_SECTOR_SIZE {
            return Err(BlkStatus::InvalidAlignment);
        }
        if self.flags & BLK_WRITE_F_FIRST != 0 && self.sector_offset != 0 {
            return Err(BlkStatus::InvalidRequest);
        }
        if self.flags & BLK_WRITE_F_LAST != 0 && end != BLK_SECTOR_SIZE {
            return Err(BlkStatus::InvalidRequest);
        }
        self.lba.checked_add(1).ok_or(BlkStatus::InvalidRequest)?;
        Ok(())
    }

    pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], BlkStatus> {
        self.validate()?;
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.device_id.to_le_bytes());
        out[16..24].copy_from_slice(&self.lba.to_le_bytes());
        out[24..28].copy_from_slice(&self.sector_offset.to_le_bytes());
        out[28..32].copy_from_slice(&self.data_len.to_le_bytes());
        out[32..].copy_from_slice(&self.data);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, BlkStatus> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(BlkStatus::InvalidRequest);
        }
        let mut data = [0u8; BLK_WRITE_MAX_CHUNK_BYTES];
        data.copy_from_slice(&bytes[32..]);
        let request = Self {
            request_id: u32::from_le_bytes(
                bytes[0..4]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            flags: u32::from_le_bytes(
                bytes[4..8]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            device_id: u64::from_le_bytes(
                bytes[8..16]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            lba: u64::from_le_bytes(
                bytes[16..24]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            sector_offset: u32::from_le_bytes(
                bytes[24..28]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            data_len: u32::from_le_bytes(
                bytes[28..32]
                    .try_into()
                    .map_err(|_| BlkStatus::InvalidRequest)?,
            ),
            data,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn chunk(&self) -> &[u8] {
        &self.data[..self.data_len as usize]
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkAssembledSector {
    pub request_id: u32,
    pub device_id: u64,
    pub lba: u64,
    pub data: [u8; BLK_SECTOR_SIZE as usize],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkSectorWriteAssembler {
    active: bool,
    request_id: u32,
    device_id: u64,
    lba: u64,
    next_offset: u32,
    data: [u8; BLK_SECTOR_SIZE as usize],
}

impl Default for BlkSectorWriteAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl BlkSectorWriteAssembler {
    pub const fn new() -> Self {
        Self {
            active: false,
            request_id: 0,
            device_id: 0,
            lba: 0,
            next_offset: 0,
            data: [0; BLK_SECTOR_SIZE as usize],
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.request_id = 0;
        self.device_id = 0;
        self.lba = 0;
        self.next_offset = 0;
        self.data.fill(0);
    }

    pub fn accept(
        &mut self,
        request: &BlkWriteRequest,
    ) -> Result<Option<BlkAssembledSector>, BlkStatus> {
        if let Err(status) = request.validate() {
            self.reset();
            return Err(status);
        }
        let first = request.flags & BLK_WRITE_F_FIRST != 0;
        let last = request.flags & BLK_WRITE_F_LAST != 0;
        if first {
            self.reset();
            self.active = true;
            self.request_id = request.request_id;
            self.device_id = request.device_id;
            self.lba = request.lba;
        } else if !self.active
            || self.request_id != request.request_id
            || self.device_id != request.device_id
            || self.lba != request.lba
        {
            self.reset();
            return Err(BlkStatus::InvalidRequest);
        }
        if request.sector_offset != self.next_offset {
            self.reset();
            return Err(BlkStatus::InvalidAlignment);
        }
        let end = request.sector_offset + request.data_len;
        if !last && end == BLK_SECTOR_SIZE {
            self.reset();
            return Err(BlkStatus::InvalidRequest);
        }
        self.data[request.sector_offset as usize..end as usize].copy_from_slice(request.chunk());
        self.next_offset = end;
        if !last {
            return Ok(None);
        }
        let completed = BlkAssembledSector {
            request_id: self.request_id,
            device_id: self.device_id,
            lba: self.lba,
            data: self.data,
        };
        self.reset();
        Ok(Some(completed))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkWriteReply {
    pub request_id: u32,
    pub status: BlkStatus,
    pub bytes_accepted: u32,
    pub sector_committed: u32,
    pub lba: u64,
}

impl BlkWriteReply {
    pub const ENCODED_LEN: usize = 24;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&self.request_id.to_le_bytes());
        out[4..8].copy_from_slice(&(self.status as u32).to_le_bytes());
        out[8..12].copy_from_slice(&self.bytes_accepted.to_le_bytes());
        out[12..16].copy_from_slice(&self.sector_committed.to_le_bytes());
        out[16..24].copy_from_slice(&self.lba.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::ENCODED_LEN {
            return None;
        }
        let status = decode_status(u32::from_le_bytes(bytes[4..8].try_into().ok()?))?;
        let sector_committed = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        if sector_committed > 1 {
            return None;
        }
        Some(Self {
            request_id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            status,
            bytes_accepted: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            sector_committed,
            lba: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        })
    }
}

fn decode_status(value: u32) -> Option<BlkStatus> {
    Some(match value {
        0 => BlkStatus::Success,
        1 => BlkStatus::InvalidAlignment,
        2 => BlkStatus::OversizedRequest,
        3 => BlkStatus::DeviceUnavailable,
        4 => BlkStatus::NotReady,
        5 => BlkStatus::IOError,
        6 => BlkStatus::InvalidRequest,
        _ => return None,
    })
}

#[repr(C, align(8))]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkReadRequest {
    pub device_id: u64,
    pub lba: u64,
    pub byte_len: u32,
    pub _reserved0: u32,
}

#[repr(C, align(8))]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlkReadReply {
    pub status: BlkStatus,
    pub bytes_read: u32,
    pub data: [u8; BLK_IPC_MAX_DATA_BYTES],
}

impl BlkReadRequest {
    pub fn validate(&self) -> Result<(), BlkStatus> {
        if self.byte_len == 0 {
            return Err(BlkStatus::InvalidRequest);
        }
        if !self.byte_len.is_multiple_of(BLK_SECTOR_SIZE) {
            return Err(BlkStatus::InvalidAlignment);
        }
        if self.byte_len as usize > BLK_IPC_MAX_DATA_BYTES {
            return Err(BlkStatus::OversizedRequest);
        }
        let sectors = (self.byte_len / BLK_SECTOR_SIZE) as u64;
        if self.lba.checked_add(sectors).is_none() {
            return Err(BlkStatus::InvalidRequest);
        }
        Ok(())
    }
}

const _: () = assert!(size_of::<BlkStatus>() == 4);
const _: () = assert!(align_of::<BlkGetInfoRequest>() >= 8);
const _: () = assert!(align_of::<BlkGetInfoReply>() >= 8);
const _: () = assert!(align_of::<BlkReadRequest>() >= 8);
const _: () = assert!(align_of::<BlkReadReply>() >= 8);
const _: () = assert!(size_of::<BlkGetInfoRequest>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
const _: () = assert!(size_of::<BlkGetInfoReply>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
const _: () = assert!(size_of::<BlkReadRequest>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
const _: () = assert!(size_of::<BlkReadReply>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);

const _: () = assert!(BlkWriteRequest::ENCODED_LEN <= IPC_INLINE_PAYLOAD_MAX_BYTES);
const _: () = assert!(BlkWriteReply::ENCODED_LEN <= IPC_INLINE_PAYLOAD_MAX_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_struct_sizes_fit_inline_payload_limit() {
        assert_eq!(IPC_INLINE_PAYLOAD_MAX_BYTES, 128);
        assert!(size_of::<BlkGetInfoRequest>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
        assert!(size_of::<BlkGetInfoReply>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
        assert!(size_of::<BlkReadRequest>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
        assert!(size_of::<BlkReadReply>() <= IPC_INLINE_PAYLOAD_MAX_BYTES);
    }

    #[test]
    fn block_struct_alignments_are_8_byte_stable() {
        assert!(align_of::<BlkGetInfoRequest>() >= 8);
        assert!(align_of::<BlkGetInfoReply>() >= 8);
        assert!(align_of::<BlkReadRequest>() >= 8);
        assert!(align_of::<BlkReadReply>() >= 8);
    }

    #[test]
    fn block_status_values_are_stable() {
        assert_eq!(BlkStatus::Success as u32, 0);
        assert_eq!(BlkStatus::InvalidAlignment as u32, 1);
        assert_eq!(BlkStatus::OversizedRequest as u32, 2);
        assert_eq!(BlkStatus::DeviceUnavailable as u32, 3);
        assert_eq!(BlkStatus::NotReady as u32, 4);
        assert_eq!(BlkStatus::IOError as u32, 5);
        assert_eq!(BlkStatus::InvalidRequest as u32, 6);
    }

    #[test]
    fn block_read_request_valid_case_uses_max_safe_aligned_len() {
        let req = BlkReadRequest {
            device_id: 0,
            lba: 0,
            byte_len: BLK_IPC_MAX_DATA_BYTES as u32,
            _reserved0: 0,
        };
        if BLK_IPC_MAX_DATA_BYTES >= BLK_SECTOR_SIZE as usize {
            assert_eq!(req.validate(), Ok(()));
        }
    }

    #[test]
    fn block_read_request_zero_length_rejected() {
        let req = BlkReadRequest {
            device_id: 0,
            lba: 0,
            byte_len: 0,
            _reserved0: 0,
        };
        assert_eq!(req.validate(), Err(BlkStatus::InvalidRequest));
    }

    #[test]
    fn block_read_request_unaligned_length_rejected() {
        let req = BlkReadRequest {
            device_id: 0,
            lba: 0,
            byte_len: BLK_SECTOR_SIZE - 1,
            _reserved0: 0,
        };
        assert_eq!(req.validate(), Err(BlkStatus::InvalidAlignment));
    }

    #[test]
    fn block_read_request_oversized_rejected() {
        let next_aligned =
            (((BLK_IPC_MAX_DATA_BYTES as u32) / BLK_SECTOR_SIZE) + 1) * BLK_SECTOR_SIZE;
        let req = BlkReadRequest {
            device_id: 0,
            lba: 0,
            byte_len: next_aligned,
            _reserved0: 0,
        };
        assert_eq!(req.validate(), Err(BlkStatus::OversizedRequest));
    }

    #[test]
    fn block_read_request_lba_overflow_rejected() {
        if BLK_IPC_MAX_DATA_BYTES >= BLK_SECTOR_SIZE as usize {
            let req = BlkReadRequest {
                device_id: 0,
                lba: u64::MAX,
                byte_len: BLK_SECTOR_SIZE,
                _reserved0: 0,
            };
            assert_eq!(req.validate(), Err(BlkStatus::InvalidRequest));
        }
    }

    #[test]
    fn block_sector_does_not_fit_in_current_inline_payload_budget() {
        assert!(BLK_IPC_MAX_DATA_BYTES < BLK_SECTOR_SIZE as usize);
    }

    fn write_chunk(offset: u32, len: u32, flags: u32) -> BlkWriteRequest {
        let mut data = [0u8; BLK_WRITE_MAX_CHUNK_BYTES];
        for (index, byte) in data[..len as usize].iter_mut().enumerate() {
            *byte = (offset as usize + index) as u8;
        }
        BlkWriteRequest {
            request_id: 7,
            flags,
            device_id: 3,
            lba: 9,
            sector_offset: offset,
            data_len: len,
            data,
        }
    }

    #[test]
    fn block_write_request_and_reply_roundtrip() {
        let request = write_chunk(0, BLK_WRITE_MAX_CHUNK_BYTES as u32, BLK_WRITE_F_FIRST);
        let encoded = request.encode().expect("encode write chunk");
        assert_eq!(BlkWriteRequest::decode(&encoded), Ok(request));
        let reply = BlkWriteReply {
            request_id: request.request_id,
            status: BlkStatus::Success,
            bytes_accepted: request.data_len,
            sector_committed: 0,
            lba: request.lba,
        };
        assert_eq!(BlkWriteReply::decode(&reply.encode()), Some(reply));
    }

    #[test]
    fn block_write_request_rejects_bad_payload_and_sector_bounds() {
        assert_eq!(
            BlkWriteRequest::decode(&[0u8; BlkWriteRequest::ENCODED_LEN - 1]),
            Err(BlkStatus::InvalidRequest)
        );
        assert_eq!(
            write_chunk(0, 0, BLK_WRITE_F_FIRST).validate(),
            Err(BlkStatus::OversizedRequest)
        );
        assert_eq!(
            write_chunk(500, 20, 0).validate(),
            Err(BlkStatus::InvalidAlignment)
        );
        assert_eq!(
            write_chunk(1, 1, BLK_WRITE_F_FIRST).validate(),
            Err(BlkStatus::InvalidRequest)
        );
        assert_eq!(
            write_chunk(0, 1, BLK_WRITE_F_LAST).validate(),
            Err(BlkStatus::InvalidRequest)
        );
    }

    #[test]
    fn block_write_constants_are_stable_and_fit_inline_payload() {
        assert_eq!(BLK_OP_WRITE, 0x0203);
        assert_eq!(BLK_WRITE_MAX_CHUNK_BYTES, 96);
        assert_eq!(BlkWriteRequest::ENCODED_LEN, IPC_INLINE_PAYLOAD_MAX_BYTES);
    }

    #[test]
    fn block_write_assembler_requires_ordered_complete_sector() {
        let mut assembler = BlkSectorWriteAssembler::new();
        let mut offset = 0u32;
        let mut completed = None;
        while offset < BLK_SECTOR_SIZE {
            let len = core::cmp::min(BLK_WRITE_MAX_CHUNK_BYTES as u32, BLK_SECTOR_SIZE - offset);
            let flags = (if offset == 0 { BLK_WRITE_F_FIRST } else { 0 })
                | (if offset + len == BLK_SECTOR_SIZE {
                    BLK_WRITE_F_LAST
                } else {
                    0
                });
            completed = assembler
                .accept(&write_chunk(offset, len, flags))
                .expect("accept chunk");
            offset += len;
        }
        let sector = completed.expect("complete sector");
        for (index, byte) in sector.data.iter().enumerate() {
            assert_eq!(*byte, index as u8);
        }

        let mut assembler = BlkSectorWriteAssembler::new();
        assembler
            .accept(&write_chunk(0, 96, BLK_WRITE_F_FIRST))
            .expect("first");
        assert_eq!(
            assembler.accept(&write_chunk(100, 96, 0)),
            Err(BlkStatus::InvalidAlignment)
        );
    }
}
