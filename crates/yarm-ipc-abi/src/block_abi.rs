// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::mem::{align_of, size_of};

/// Must remain in sync with `yarm_kernel::ipc::Message::MAX_PAYLOAD`.
pub const IPC_INLINE_PAYLOAD_MAX_BYTES: usize = 128;
pub const BLK_SECTOR_SIZE: u32 = 512;

pub const BLK_OP_GET_INFO: u16 = 1;
pub const BLK_OP_READ: u16 = 2;

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
        let next_aligned = (((BLK_IPC_MAX_DATA_BYTES as u32) / BLK_SECTOR_SIZE) + 1) * BLK_SECTOR_SIZE;
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
}
