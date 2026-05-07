// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const IPC_ABI_V1_REGISTER_WORDS: usize = 2;
pub const IPC_ABI_V2_INLINE_WORDS: usize = 8;
pub const IPC_ABI_V2_VERSION: u16 = 2;

pub const IPC_V2_OP_SEND: u16 = 1;
pub const IPC_V2_OP_RECV: u16 = 2;
pub const IPC_V2_OP_CALL: u16 = 3;
pub const IPC_V2_OP_REPLY: u16 = 4;

pub const IPC_V2_FLAG_INLINE_PAYLOAD: u32 = 1 << 0;
pub const IPC_V2_FLAG_TRANSFER_CAP: u32 = 1 << 1;
pub const IPC_V2_FLAG_RECV_COPYOUT: u32 = 1 << 2;
pub const IPC_V2_FLAG_RET_COPYOUT: u32 = 1 << 3;

pub const IPC_V2_NO_TRANSFER_CAP: u64 = u64::MAX;
pub const IPC_V2_SHARED_REPLY_META_VERSION: u16 = 1;
pub const IPC_V2_SHARED_REPLY_FLAG_READ_ONLY: u16 = 1 << 0;
pub const IPC_V2_SHARED_REPLY_FLAG_WRITABLE: u16 = 1 << 1;
pub const IPC_V2_SHARED_REPLY_ALLOWED_FLAGS: u16 =
    IPC_V2_SHARED_REPLY_FLAG_READ_ONLY | IPC_V2_SHARED_REPLY_FLAG_WRITABLE;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IpcRegisterBlockV2 {
    pub abi_version: u16,
    pub op: u16,
    pub flags: u32,

    pub endpoint_cap: u64,
    pub ptr_or_offset: u64,
    pub len: u64,

    pub inline_words: [u64; IPC_ABI_V2_INLINE_WORDS],
    pub transfer_cap: u64,

    pub aux0: u64,
    pub aux1: u64,

    pub ret_status: u64,
    pub ret_len: u64,
    pub ret_transfer_cap: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IpcV2SharedReplyMeta {
    pub version: u16,
    pub flags: u16,
    pub reserved: u32,
    pub offset: u64,
    pub len: u64,
}

pub const IPC_V2_SHARED_REPLY_META_SIZE: usize = core::mem::size_of::<IpcV2SharedReplyMeta>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedReplyMetaError {
    InvalidSize,
    InvalidVersion,
    InvalidFlags,
    ReservedNonZero,
    ZeroLength,
    Overflow,
}

pub fn encode_shared_reply_meta(meta: IpcV2SharedReplyMeta) -> Result<[u8; IPC_V2_SHARED_REPLY_META_SIZE], SharedReplyMetaError> {
    validate_shared_reply_meta(meta)?;
    let mut out = [0u8; IPC_V2_SHARED_REPLY_META_SIZE];
    out[0..2].copy_from_slice(&meta.version.to_le_bytes());
    out[2..4].copy_from_slice(&meta.flags.to_le_bytes());
    out[4..8].copy_from_slice(&meta.reserved.to_le_bytes());
    out[8..16].copy_from_slice(&meta.offset.to_le_bytes());
    out[16..24].copy_from_slice(&meta.len.to_le_bytes());
    Ok(out)
}

pub fn decode_shared_reply_meta(payload: &[u8]) -> Result<IpcV2SharedReplyMeta, SharedReplyMetaError> {
    if payload.len() != IPC_V2_SHARED_REPLY_META_SIZE {
        return Err(SharedReplyMetaError::InvalidSize);
    }
    let meta = IpcV2SharedReplyMeta {
        version: u16::from_le_bytes([payload[0], payload[1]]),
        flags: u16::from_le_bytes([payload[2], payload[3]]),
        reserved: u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]),
        offset: u64::from_le_bytes([
            payload[8], payload[9], payload[10], payload[11], payload[12], payload[13], payload[14], payload[15],
        ]),
        len: u64::from_le_bytes([
            payload[16], payload[17], payload[18], payload[19], payload[20], payload[21], payload[22], payload[23],
        ]),
    };
    validate_shared_reply_meta(meta)?;
    Ok(meta)
}

fn validate_shared_reply_meta(meta: IpcV2SharedReplyMeta) -> Result<(), SharedReplyMetaError> {
    if meta.version != IPC_V2_SHARED_REPLY_META_VERSION {
        return Err(SharedReplyMetaError::InvalidVersion);
    }
    if meta.reserved != 0 {
        return Err(SharedReplyMetaError::ReservedNonZero);
    }
    if (meta.flags & !IPC_V2_SHARED_REPLY_ALLOWED_FLAGS) != 0 {
        return Err(SharedReplyMetaError::InvalidFlags);
    }
    if meta.len == 0 {
        return Err(SharedReplyMetaError::ZeroLength);
    }
    if meta.offset.checked_add(meta.len).is_none() {
        return Err(SharedReplyMetaError::Overflow);
    }
    Ok(())
}

impl IpcRegisterBlockV2 {
    pub const BLOCK_SIZE: usize = core::mem::size_of::<Self>();

    pub const fn new_v2(op: u16) -> Self {
        Self {
            abi_version: IPC_ABI_V2_VERSION,
            op,
            flags: 0,
            endpoint_cap: 0,
            ptr_or_offset: 0,
            len: 0,
            inline_words: [0; IPC_ABI_V2_INLINE_WORDS],
            transfer_cap: IPC_V2_NO_TRANSFER_CAP,
            aux0: 0,
            aux1: 0,
            ret_status: 0,
            ret_len: 0,
            ret_transfer_cap: IPC_V2_NO_TRANSFER_CAP,
        }
    }
}

pub const IPC_ABI_V2_BLOCK_SIZE: usize = IpcRegisterBlockV2::BLOCK_SIZE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_is_stable() {
        assert_eq!(core::mem::size_of::<IpcRegisterBlockV2>(), 144);
        assert_eq!(IPC_ABI_V2_BLOCK_SIZE, 144);
    }

    #[test]
    fn block_alignment_is_u64_aligned() {
        assert_eq!(core::mem::align_of::<IpcRegisterBlockV2>(), 8);
    }

    #[test]
    fn inline_word_count_is_eight() {
        let block = IpcRegisterBlockV2::default();
        assert_eq!(block.inline_words.len(), IPC_ABI_V2_INLINE_WORDS);
        assert_eq!(block.inline_words.len(), 8);
    }

    #[test]
    fn default_and_new_v2_version_behavior() {
        let default_block = IpcRegisterBlockV2::default();
        assert_eq!(default_block.abi_version, 0);

        let v2 = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        assert_eq!(v2.abi_version, IPC_ABI_V2_VERSION);
        assert_eq!(v2.op, IPC_V2_OP_SEND);
    }

    #[test]
    fn field_offsets_match_contract() {
        assert_eq!(core::mem::offset_of!(IpcRegisterBlockV2, abi_version), 0);
        assert_eq!(core::mem::offset_of!(IpcRegisterBlockV2, endpoint_cap), 8);
        assert_eq!(core::mem::offset_of!(IpcRegisterBlockV2, inline_words), 32);
        assert_eq!(core::mem::offset_of!(IpcRegisterBlockV2, ret_status), 120);
    }

    #[test]
    fn shared_reply_meta_layout_is_stable() {
        assert_eq!(core::mem::size_of::<IpcV2SharedReplyMeta>(), 24);
        assert_eq!(core::mem::align_of::<IpcV2SharedReplyMeta>(), 8);
        assert_eq!(IPC_V2_SHARED_REPLY_META_SIZE, 24);
    }

    #[test]
    fn shared_reply_meta_encode_decode_roundtrip() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0x4000,
            len: 0x2000,
        };
        let encoded = encode_shared_reply_meta(meta).expect("encode");
        let decoded = decode_shared_reply_meta(&encoded).expect("decode");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn shared_reply_meta_rejects_unknown_flags() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: 1 << 7,
            reserved: 0,
            offset: 0,
            len: 1,
        };
        assert_eq!(
            encode_shared_reply_meta(meta),
            Err(SharedReplyMetaError::InvalidFlags)
        );
    }

    #[test]
    fn shared_reply_meta_rejects_overflow() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: IPC_V2_SHARED_REPLY_FLAG_WRITABLE,
            reserved: 0,
            offset: u64::MAX,
            len: 1,
        };
        assert_eq!(
            encode_shared_reply_meta(meta),
            Err(SharedReplyMetaError::Overflow)
        );
    }

    #[test]
    fn shared_reply_meta_rejects_zero_len() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: 0,
            reserved: 0,
            offset: 0x1000,
            len: 0,
        };
        assert_eq!(
            encode_shared_reply_meta(meta),
            Err(SharedReplyMetaError::ZeroLength)
        );
    }

    #[test]
    fn shared_reply_meta_rejects_nonzero_reserved() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: 0,
            reserved: 42,
            offset: 0x1000,
            len: 16,
        };
        assert_eq!(
            encode_shared_reply_meta(meta),
            Err(SharedReplyMetaError::ReservedNonZero)
        );
    }

    #[test]
    fn shared_reply_meta_rejects_bad_version_and_size() {
        let mut bytes = [0u8; IPC_V2_SHARED_REPLY_META_SIZE];
        bytes[0..2].copy_from_slice(&(IPC_V2_SHARED_REPLY_META_VERSION + 1).to_le_bytes());
        bytes[16..24].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            decode_shared_reply_meta(&bytes),
            Err(SharedReplyMetaError::InvalidVersion)
        );
        assert_eq!(
            decode_shared_reply_meta(&bytes[..IPC_V2_SHARED_REPLY_META_SIZE - 1]),
            Err(SharedReplyMetaError::InvalidSize)
        );
    }
}
