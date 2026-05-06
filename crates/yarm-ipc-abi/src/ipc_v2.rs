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
}
