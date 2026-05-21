// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! devfs-server IPC protocol constants and wire types.

pub const DEVFS_SERVER_ABI_VERSION: u16 = 1;

pub const DEVFS_OP_REGISTER_NODE: u16 = 1;

// Node kind discriminants.
pub const DEVFS_NODE_KIND_CHAR: u32 = 1;
pub const DEVFS_NODE_KIND_BLOCK: u32 = 2;

// Registration reply status codes.
pub const DEVFS_REGISTER_STATUS_OK: u32 = 0;
pub const DEVFS_REGISTER_STATUS_ERR_DUPLICATE: u32 = 1;
pub const DEVFS_REGISTER_STATUS_ERR_FULL: u32 = 2;
pub const DEVFS_REGISTER_STATUS_ERR_INVALID_PATH: u32 = 3;
pub const DEVFS_REGISTER_STATUS_ERR_INVALID_KIND: u32 = 4;

/// Maximum device-node name length (not including the `/dev/` prefix).
pub const DEVFS_REGISTER_MAX_NAME: usize = 64;

/// Fixed header size: kind(4) + flags(4) + backend_cap(8) + name_len(1) = 17 bytes.
pub const DEVFS_REGISTER_HEADER_BYTES: usize = 17;

/// Maximum total encoded size.
pub const DEVFS_REGISTER_MAX_BYTES: usize = DEVFS_REGISTER_HEADER_BYTES + DEVFS_REGISTER_MAX_NAME;

/// Wire layout for `DEVFS_OP_REGISTER_NODE` payloads.
///
/// Encoding: `[kind:u32 LE][flags:u32 LE][backend_cap:u64 LE][name_len:u8][name: name_len bytes]`
pub struct NodeRegistrationArgs<'a> {
    pub kind: u32,
    pub flags: u32,
    pub backend_cap: u64,
    pub name: &'a [u8],
}

impl<'a> NodeRegistrationArgs<'a> {
    /// Encode into a fixed-size buffer. Returns `None` if `name` exceeds
    /// `DEVFS_REGISTER_MAX_NAME`.
    pub fn encode(&self) -> Option<([u8; DEVFS_REGISTER_MAX_BYTES], usize)> {
        if self.name.len() > DEVFS_REGISTER_MAX_NAME {
            return None;
        }
        let total = DEVFS_REGISTER_HEADER_BYTES + self.name.len();
        let mut buf = [0u8; DEVFS_REGISTER_MAX_BYTES];
        buf[0..4].copy_from_slice(&self.kind.to_le_bytes());
        buf[4..8].copy_from_slice(&self.flags.to_le_bytes());
        buf[8..16].copy_from_slice(&self.backend_cap.to_le_bytes());
        buf[16] = self.name.len() as u8;
        buf[DEVFS_REGISTER_HEADER_BYTES..total].copy_from_slice(self.name);
        Some((buf, total))
    }

    /// Decode from a raw payload slice. Returns `None` on any malformation.
    pub fn decode(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < DEVFS_REGISTER_HEADER_BYTES {
            return None;
        }
        let kind = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let flags = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let backend_cap = u64::from_le_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
        ]);
        let name_len = buf[16] as usize;
        if name_len > DEVFS_REGISTER_MAX_NAME {
            return None;
        }
        let end = DEVFS_REGISTER_HEADER_BYTES + name_len;
        if buf.len() < end {
            return None;
        }
        Some(NodeRegistrationArgs {
            kind,
            flags,
            backend_cap,
            name: &buf[DEVFS_REGISTER_HEADER_BYTES..end],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_constants_are_frozen() {
        assert_eq!(DEVFS_SERVER_ABI_VERSION, 1);
        assert_eq!(DEVFS_OP_REGISTER_NODE, 1);
        assert_eq!(DEVFS_NODE_KIND_CHAR, 1);
        assert_eq!(DEVFS_NODE_KIND_BLOCK, 2);
        assert_eq!(DEVFS_REGISTER_STATUS_OK, 0);
        assert_eq!(DEVFS_REGISTER_STATUS_ERR_DUPLICATE, 1);
        assert_eq!(DEVFS_REGISTER_STATUS_ERR_FULL, 2);
        assert_eq!(DEVFS_REGISTER_STATUS_ERR_INVALID_PATH, 3);
        assert_eq!(DEVFS_REGISTER_STATUS_ERR_INVALID_KIND, 4);
        assert_eq!(DEVFS_REGISTER_HEADER_BYTES, 17);
        assert_eq!(DEVFS_REGISTER_MAX_BYTES, 81);
    }

    #[test]
    fn encode_decode_roundtrip_char_node() {
        let args = NodeRegistrationArgs {
            kind: DEVFS_NODE_KIND_CHAR,
            flags: 0,
            backend_cap: 0xDEAD_BEEF_0000_0001,
            name: b"uart0",
        };
        let (buf, len) = args.encode().expect("encode");
        let decoded = NodeRegistrationArgs::decode(&buf[..len]).expect("decode");
        assert_eq!(decoded.kind, DEVFS_NODE_KIND_CHAR);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.backend_cap, 0xDEAD_BEEF_0000_0001);
        assert_eq!(decoded.name, b"uart0");
    }

    #[test]
    fn encode_decode_roundtrip_block_node() {
        let args = NodeRegistrationArgs {
            kind: DEVFS_NODE_KIND_BLOCK,
            flags: 0xFFFF_FFFF,
            backend_cap: 42,
            name: b"blk0",
        };
        let (buf, len) = args.encode().expect("encode");
        let decoded = NodeRegistrationArgs::decode(&buf[..len]).expect("decode");
        assert_eq!(decoded.kind, DEVFS_NODE_KIND_BLOCK);
        assert_eq!(decoded.flags, 0xFFFF_FFFF);
        assert_eq!(decoded.backend_cap, 42);
        assert_eq!(decoded.name, b"blk0");
    }

    #[test]
    fn encode_rejects_name_too_long() {
        let long_name = [b'x'; DEVFS_REGISTER_MAX_NAME + 1];
        let args = NodeRegistrationArgs {
            kind: DEVFS_NODE_KIND_CHAR,
            flags: 0,
            backend_cap: 1,
            name: &long_name,
        };
        assert!(args.encode().is_none());
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert!(NodeRegistrationArgs::decode(&[0u8; 16]).is_none());
        assert!(NodeRegistrationArgs::decode(&[]).is_none());
    }

    #[test]
    fn decode_rejects_name_len_overflow() {
        let mut buf = [0u8; DEVFS_REGISTER_HEADER_BYTES];
        buf[16] = (DEVFS_REGISTER_MAX_NAME + 1) as u8;
        assert!(NodeRegistrationArgs::decode(&buf).is_none());
    }

    #[test]
    fn decode_rejects_truncated_name() {
        let args = NodeRegistrationArgs {
            kind: DEVFS_NODE_KIND_CHAR,
            flags: 0,
            backend_cap: 1,
            name: b"uart0",
        };
        let (buf, len) = args.encode().expect("encode");
        // Truncate one byte off the name.
        assert!(NodeRegistrationArgs::decode(&buf[..len - 1]).is_none());
    }

    #[test]
    fn golden_vector_uart0_char() {
        // kind=1, flags=0, backend_cap=7, name="uart0"
        let args = NodeRegistrationArgs {
            kind: 1,
            flags: 0,
            backend_cap: 7,
            name: b"uart0",
        };
        let (buf, len) = args.encode().expect("encode");
        assert_eq!(len, DEVFS_REGISTER_HEADER_BYTES + 5);
        // kind LE
        assert_eq!(&buf[0..4], &[1, 0, 0, 0]);
        // flags LE
        assert_eq!(&buf[4..8], &[0, 0, 0, 0]);
        // backend_cap LE
        assert_eq!(&buf[8..16], &[7, 0, 0, 0, 0, 0, 0, 0]);
        // name_len
        assert_eq!(buf[16], 5);
        // name
        assert_eq!(&buf[17..22], b"uart0");
    }
}
