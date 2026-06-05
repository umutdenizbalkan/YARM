// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::block_abi::{BlkWriteReply, BlkWriteRequest};

/// FS-12 inline write protocol used with `BLKCACHE_OP_WRITE_BLOCK`.
/// The backend id is carried in `BlkWriteRequest::device_id`.
pub type BlkCacheWriteBlockRequest = BlkWriteRequest;
pub type BlkCacheWriteBlockReply = BlkWriteReply;

pub const BLKCACHE_OP_REGISTER_BACKEND: u16 = 1;
pub const BLKCACHE_OP_REGISTER_BUFFER: u16 = 2;
pub const BLKCACHE_OP_UNREGISTER_BUFFER: u16 = 3;
pub const BLKCACHE_OP_READ_BLOCK: u16 = 4;
pub const BLKCACHE_OP_WRITE_BLOCK: u16 = 5;
pub const BLKCACHE_OP_FLUSH: u16 = 6;
pub const BLKCACHE_OP_INVALIDATE: u16 = 7;
pub const BLKCACHE_OP_GET_STATS: u16 = 8;
pub const BLKCACHE_OP_CANCEL: u16 = 9;

pub const BLKCACHE_STATUS_OK: u32 = 0;
pub const BLKCACHE_STATUS_ERR_UNSUPPORTED: u32 = 1;
pub const BLKCACHE_STATUS_ERR_BAD_REQUEST: u32 = 2;
pub const BLKCACHE_STATUS_ERR_UNKNOWN_BACKEND: u32 = 3;
pub const BLKCACHE_STATUS_ERR_UNKNOWN_BUFFER: u32 = 4;
pub const BLKCACHE_STATUS_ERR_OUT_OF_RANGE: u32 = 5;
pub const BLKCACHE_STATUS_ERR_PERMISSION: u32 = 6;
pub const BLKCACHE_STATUS_ERR_BUSY: u32 = 7;
pub const BLKCACHE_STATUS_ERR_IO: u32 = 8;
pub const BLKCACHE_STATUS_ERR_NO_MEMORY: u32 = 9;
pub const BLKCACHE_STATUS_ERR_CANCELLED: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterBackendArgs {
    pub backend_id: u64,
    pub backend_send_cap: u64,
    pub block_size: u32,
    pub flags: u32,
    pub block_count: u64,
}
impl RegisterBackendArgs {
    pub const ENCODED_LEN: usize = 32;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.backend_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.backend_send_cap.to_le_bytes());
        o[16..20].copy_from_slice(&self.block_size.to_le_bytes());
        o[20..24].copy_from_slice(&self.flags.to_le_bytes());
        o[24..32].copy_from_slice(&self.block_count.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            backend_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            backend_send_cap: u64::from_le_bytes(b[8..16].try_into().ok()?),
            block_size: u32::from_le_bytes(b[16..20].try_into().ok()?),
            flags: u32::from_le_bytes(b[20..24].try_into().ok()?),
            block_count: u64::from_le_bytes(b[24..32].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterBufferArgs {
    pub buffer_id: u64,
    pub mem_cap: u64,
    pub offset: u64,
    pub len: u64,
    pub flags: u32,
}
impl RegisterBufferArgs {
    pub const ENCODED_LEN: usize = 36;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.buffer_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.mem_cap.to_le_bytes());
        o[16..24].copy_from_slice(&self.offset.to_le_bytes());
        o[24..32].copy_from_slice(&self.len.to_le_bytes());
        o[32..36].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            buffer_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            mem_cap: u64::from_le_bytes(b[8..16].try_into().ok()?),
            offset: u64::from_le_bytes(b[16..24].try_into().ok()?),
            len: u64::from_le_bytes(b[24..32].try_into().ok()?),
            flags: u32::from_le_bytes(b[32..36].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnregisterBufferArgs {
    pub buffer_id: u64,
}
impl UnregisterBufferArgs {
    pub const ENCODED_LEN: usize = 8;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.buffer_id.to_le_bytes()
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != 8 {
            return None;
        }
        Some(Self {
            buffer_id: u64::from_le_bytes(b.try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockIoRequest {
    pub request_id: u64,
    pub backend_id: u64,
    pub block_index: u64,
    pub block_count: u32,
    pub buffer_id: u64,
    pub buffer_offset: u64,
    pub flags: u32,
}
impl BlockIoRequest {
    pub const ENCODED_LEN: usize = 48;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.backend_id.to_le_bytes());
        o[16..24].copy_from_slice(&self.block_index.to_le_bytes());
        o[24..28].copy_from_slice(&self.block_count.to_le_bytes());
        o[28..36].copy_from_slice(&self.buffer_id.to_le_bytes());
        o[36..44].copy_from_slice(&self.buffer_offset.to_le_bytes());
        o[44..48].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            request_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            backend_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
            block_index: u64::from_le_bytes(b[16..24].try_into().ok()?),
            block_count: u32::from_le_bytes(b[24..28].try_into().ok()?),
            buffer_id: u64::from_le_bytes(b[28..36].try_into().ok()?),
            buffer_offset: u64::from_le_bytes(b[36..44].try_into().ok()?),
            flags: u32::from_le_bytes(b[44..48].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    pub request_id: u64,
    pub backend_id: u64,
    pub block_index: u64,
    pub block_count: u32,
    pub flags: u32,
}
impl RangeRequest {
    pub const ENCODED_LEN: usize = 32;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.backend_id.to_le_bytes());
        o[16..24].copy_from_slice(&self.block_index.to_le_bytes());
        o[24..28].copy_from_slice(&self.block_count.to_le_bytes());
        o[28..32].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            request_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            backend_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
            block_index: u64::from_le_bytes(b[16..24].try_into().ok()?),
            block_count: u32::from_le_bytes(b[24..28].try_into().ok()?),
            flags: u32::from_le_bytes(b[28..32].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetStatsRequest {
    pub request_id: u64,
    pub backend_id: u64,
    pub flags: u32,
}
impl GetStatsRequest {
    pub const ENCODED_LEN: usize = 20;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.backend_id.to_le_bytes());
        o[16..20].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            request_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            backend_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
            flags: u32::from_le_bytes(b[16..20].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelRequest {
    pub request_id: u64,
    pub target_request_id: u64,
}
impl CancelRequest {
    pub const ENCODED_LEN: usize = 16;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        o[8..16].copy_from_slice(&self.target_request_id.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            request_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            target_request_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkCacheResponse {
    pub request_id: u64,
    pub status: u32,
    pub bytes_moved: u64,
    pub flags: u32,
}
impl BlkCacheResponse {
    pub const ENCODED_LEN: usize = 24;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.request_id.to_le_bytes());
        o[8..12].copy_from_slice(&self.status.to_le_bytes());
        o[12..20].copy_from_slice(&self.bytes_moved.to_le_bytes());
        o[20..24].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            request_id: u64::from_le_bytes(b[0..8].try_into().ok()?),
            status: u32::from_le_bytes(b[8..12].try_into().ok()?),
            bytes_moved: u64::from_le_bytes(b[12..20].try_into().ok()?),
            flags: u32::from_le_bytes(b[20..24].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_opcode_values() {
        assert_eq!(BLKCACHE_OP_REGISTER_BACKEND, 1);
        assert_eq!(BLKCACHE_OP_CANCEL, 9);
    }
    #[test]
    fn frozen_status_values() {
        assert_eq!(BLKCACHE_STATUS_OK, 0);
        assert_eq!(BLKCACHE_STATUS_ERR_CANCELLED, 10);
    }
    #[test]
    fn distinct_nonzero_errors() {
        let e = [
            BLKCACHE_STATUS_ERR_UNSUPPORTED,
            BLKCACHE_STATUS_ERR_BAD_REQUEST,
            BLKCACHE_STATUS_ERR_UNKNOWN_BACKEND,
            BLKCACHE_STATUS_ERR_UNKNOWN_BUFFER,
            BLKCACHE_STATUS_ERR_OUT_OF_RANGE,
            BLKCACHE_STATUS_ERR_PERMISSION,
            BLKCACHE_STATUS_ERR_BUSY,
            BLKCACHE_STATUS_ERR_IO,
            BLKCACHE_STATUS_ERR_NO_MEMORY,
            BLKCACHE_STATUS_ERR_CANCELLED,
        ];
        for i in 0..e.len() {
            assert_ne!(e[i], 0);
            for j in i + 1..e.len() {
                assert_ne!(e[i], e[j]);
            }
        }
    }
    #[test]
    fn golden_roundtrips() {
        let rb = RegisterBackendArgs {
            backend_id: 1,
            backend_send_cap: 2,
            block_size: 4096,
            flags: 3,
            block_count: 4,
        };
        assert_eq!(RegisterBackendArgs::decode(&rb.encode()), Some(rb));
        let rbuf = RegisterBufferArgs {
            buffer_id: 7,
            mem_cap: 8,
            offset: 9,
            len: 10,
            flags: 11,
        };
        assert_eq!(RegisterBufferArgs::decode(&rbuf.encode()), Some(rbuf));
        let ub = UnregisterBufferArgs { buffer_id: 12 };
        assert_eq!(UnregisterBufferArgs::decode(&ub.encode()), Some(ub));
        let bio = BlockIoRequest {
            request_id: 13,
            backend_id: 14,
            block_index: 15,
            block_count: 16,
            buffer_id: 17,
            buffer_offset: 18,
            flags: 19,
        };
        assert_eq!(BlockIoRequest::decode(&bio.encode()), Some(bio));
        let rr = RangeRequest {
            request_id: 20,
            backend_id: 21,
            block_index: 22,
            block_count: 23,
            flags: 24,
        };
        assert_eq!(RangeRequest::decode(&rr.encode()), Some(rr));
        let gs = GetStatsRequest {
            request_id: 25,
            backend_id: 26,
            flags: 27,
        };
        assert_eq!(GetStatsRequest::decode(&gs.encode()), Some(gs));
        let c = CancelRequest {
            request_id: 28,
            target_request_id: 29,
        };
        assert_eq!(CancelRequest::decode(&c.encode()), Some(c));
        let resp = BlkCacheResponse {
            request_id: 30,
            status: BLKCACHE_STATUS_ERR_UNSUPPORTED,
            bytes_moved: 0,
            flags: 31,
        };
        assert_eq!(BlkCacheResponse::decode(&resp.encode()), Some(resp));
    }
    #[test]
    fn malformed_rejected() {
        assert!(RegisterBackendArgs::decode(&[0; 31]).is_none());
        assert!(RegisterBufferArgs::decode(&[0; 35]).is_none());
        assert!(UnregisterBufferArgs::decode(&[0; 7]).is_none());
        assert!(BlockIoRequest::decode(&[0; 47]).is_none());
        assert!(RangeRequest::decode(&[0; 31]).is_none());
        assert!(GetStatsRequest::decode(&[0; 19]).is_none());
        assert!(CancelRequest::decode(&[0; 15]).is_none());
        assert!(BlkCacheResponse::decode(&[0; 23]).is_none());
    }
    #[test]
    fn representative_ops_roundtrip() {
        let _read = BlockIoRequest {
            request_id: 1,
            backend_id: 2,
            block_index: 3,
            block_count: 4,
            buffer_id: 5,
            buffer_offset: 6,
            flags: 0,
        };
        let _write = BlockIoRequest {
            request_id: 7,
            backend_id: 8,
            block_index: 9,
            block_count: 10,
            buffer_id: 11,
            buffer_offset: 12,
            flags: 1,
        };
        let _flush = RangeRequest {
            request_id: 13,
            backend_id: 14,
            block_index: 15,
            block_count: 16,
            flags: 2,
        };
        let resp = BlkCacheResponse {
            request_id: 17,
            status: BLKCACHE_STATUS_ERR_UNSUPPORTED,
            bytes_moved: 0,
            flags: 0,
        };
        assert_eq!(
            BlkCacheResponse::decode(&resp.encode()).unwrap().status,
            BLKCACHE_STATUS_ERR_UNSUPPORTED
        );
    }

    #[test]
    fn write_block_uses_inline_block_write_codec() {
        assert_eq!(BLKCACHE_OP_WRITE_BLOCK, 5);
        assert_eq!(BlkCacheWriteBlockRequest::ENCODED_LEN, 128);
        assert_eq!(BlkCacheWriteBlockReply::ENCODED_LEN, 24);
    }
}
