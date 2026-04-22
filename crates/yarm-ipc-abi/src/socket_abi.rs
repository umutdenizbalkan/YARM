// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketCodecError {
    Malformed,
}

pub const SOCKET_SERVER_ABI_VERSION: u16 = 1;
pub const SOCKET_CODEC_V1_VERSION: u16 = 1;

pub const SOCKET_OP_SOCKET: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketArgs {
    pub domain: u64,
    pub sock_type: u64,
    pub protocol: u64,
    pub reserved: u64,
}

impl SocketArgs {
    pub const VERSION: u16 = SOCKET_CODEC_V1_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(domain: u64, sock_type: u64, protocol: u64) -> Self {
        Self {
            domain,
            sock_type,
            protocol,
            reserved: 0,
        }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let values = [self.domain, self.sock_type, self.protocol, self.reserved];
        let mut idx = 0;
        while idx < values.len() {
            let bytes = values[idx].to_le_bytes();
            let mut offset = 0;
            while offset < 8 {
                payload[idx * 8 + offset] = bytes[offset];
                offset += 1;
            }
            idx += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, SocketCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(SocketCodecError::Malformed);
        }
        let mut values = [0u64; 4];
        let mut idx = 0;
        while idx < values.len() {
            let start = idx * 8;
            let end = start + 8;
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&payload[start..end]);
            values[idx] = u64::from_le_bytes(bytes);
            idx += 1;
        }
        Ok(Self {
            domain: values[0],
            sock_type: values[1],
            protocol: values[2],
            reserved: values[3],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_args_roundtrip() {
        let args = SocketArgs::new(2, 1, 0);
        assert_eq!(SocketArgs::decode(&args.encode()), Ok(args));
    }

    #[test]
    fn socket_args_reject_non_exact_payload_lengths() {
        let short = [0u8; SocketArgs::ENCODED_LEN - 1];
        assert_eq!(SocketArgs::decode(&short), Err(SocketCodecError::Malformed));

        let long = [0u8; SocketArgs::ENCODED_LEN + 1];
        assert_eq!(SocketArgs::decode(&long), Err(SocketCodecError::Malformed));
    }

    #[test]
    fn socket_abi_constants_are_stable() {
        assert_eq!(SOCKET_SERVER_ABI_VERSION, 1);
        assert_eq!(SOCKET_CODEC_V1_VERSION, 1);
        assert_eq!(SocketArgs::VERSION, SOCKET_CODEC_V1_VERSION);
        assert_eq!(SocketArgs::ENCODED_LEN, 32);
        assert_eq!(SOCKET_OP_SOCKET, 1);
    }
}
