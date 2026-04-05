// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransferCapId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    PayloadTooLarge,
    MissingCapTransferFlag,
    InconsistentCapTransferFlag,
    InvalidEndpointDepth,
    EndpointFull,
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::PayloadTooLarge => "IPC payload exceeds register/message capacity",
            Self::MissingCapTransferFlag => "transferred capability is missing the transfer flag",
            Self::InconsistentCapTransferFlag => {
                "capability transfer flag set without a transferred capability"
            }
            Self::InvalidEndpointDepth => "endpoint depth is outside the supported range",
            Self::EndpointFull => "endpoint queue is full",
        };
        f.write_str(message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedMemoryRegion {
    pub offset: u64,
    pub len: u64,
}

impl SharedMemoryRegion {
    pub const ENCODED_LEN: usize = 16;

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let off = self.offset.to_le_bytes();
        let len = self.len.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            out[i] = off[i];
            out[8 + i] = len[i];
            i += 1;
        }
        out
    }

    pub const fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut off = [0u8; 8];
        let mut len = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            off[i] = payload[i];
            len[i] = payload[8 + i];
            i += 1;
        }
        Some(Self {
            offset: u64::from_le_bytes(off),
            len: u64::from_le_bytes(len),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub sender_tid: ThreadId,
    pub opcode: u16,
    pub flags: u16,
    transferred_cap: u64,
    pub len: u8,
    pub payload: [u8; Self::MAX_PAYLOAD],
}

const _: () = assert!(Message::MAX_PAYLOAD <= (u8::MAX as usize));

impl Message {
    pub const MAX_PAYLOAD: usize = 128;
    pub const FLAG_CAP_TRANSFER: u16 = 1 << 0;
    pub const NO_TRANSFER_CAP: u64 = u64::MAX;

    pub fn new(sender_tid: u64, bytes: &[u8]) -> Result<Self, IpcError> {
        Self::with_header(sender_tid, 0, 0, None, bytes)
    }

    pub fn with_header(
        sender_tid: u64,
        opcode: u16,
        flags: u16,
        transferred_cap: Option<u64>,
        bytes: &[u8],
    ) -> Result<Self, IpcError> {
        if bytes.len() > Self::MAX_PAYLOAD {
            return Err(IpcError::PayloadTooLarge);
        }

        let has_cap = transferred_cap.is_some();
        let flag_set = (flags & Self::FLAG_CAP_TRANSFER) != 0;

        if has_cap && !flag_set {
            return Err(IpcError::MissingCapTransferFlag);
        }
        if !has_cap && flag_set {
            return Err(IpcError::InconsistentCapTransferFlag);
        }

        let mut payload = [0u8; Self::MAX_PAYLOAD];
        payload[..bytes.len()].copy_from_slice(bytes);

        Ok(Self {
            sender_tid: ThreadId(sender_tid),
            opcode,
            flags,
            transferred_cap: transferred_cap.unwrap_or(Self::NO_TRANSFER_CAP),
            len: bytes.len() as u8,
            payload,
        })
    }

    pub const fn transferred_cap(&self) -> Option<TransferCapId> {
        if self.transferred_cap == Self::NO_TRANSFER_CAP {
            None
        } else {
            Some(TransferCapId(self.transferred_cap))
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip_preserves_header_and_payload() {
        let msg = Message::with_header(42, 0x88, 0, None, b"abc").expect("message");
        assert_eq!(msg.sender_tid, ThreadId(42));
        assert_eq!(msg.opcode, 0x88);
        assert_eq!(msg.flags, 0);
        assert_eq!(msg.transferred_cap(), None);
        assert_eq!(msg.as_slice(), b"abc");
    }

    #[test]
    fn message_requires_transfer_flag_consistency() {
        assert_eq!(
            Message::with_header(0, 1, 0, Some(9), &[]),
            Err(IpcError::MissingCapTransferFlag)
        );
        assert_eq!(
            Message::with_header(0, 1, Message::FLAG_CAP_TRANSFER, None, &[]),
            Err(IpcError::InconsistentCapTransferFlag)
        );
    }

    #[test]
    fn message_rejects_oversized_payload() {
        let payload = [0u8; Message::MAX_PAYLOAD + 1];
        assert_eq!(Message::new(0, &payload), Err(IpcError::PayloadTooLarge));
    }

    #[test]
    fn shared_memory_region_codec_roundtrip() {
        let region = SharedMemoryRegion {
            offset: 0x2000,
            len: 16384,
        };
        let encoded = region.encode();
        assert_eq!(SharedMemoryRegion::decode(&encoded), Some(region));
    }
}
