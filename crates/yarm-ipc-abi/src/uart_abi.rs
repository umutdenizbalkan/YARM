// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! UART service wire ABI version 1.
//!
//! Requests and replies use fixed-size, allocation-free little-endian payloads
//! that fit within YARM's 128-byte inline IPC payload. This ABI describes a
//! generic polling UART service; it does not imply Raspberry Pi 5 hardware
//! discovery, MMIO access, or a live service process.

pub const UART_ABI_VERSION: u16 = 1;
pub const UART_MAX_INLINE_WRITE: usize = 96;
pub const UART_REQUEST_ENCODED_LEN: usize = 104;
pub const UART_REPLY_ENCODED_LEN: usize = 40;

pub const UART_OP_GET_INFO: u16 = 0x0f01;
pub const UART_OP_CONFIGURE_8N1: u16 = 0x0f02;
pub const UART_OP_WRITE_BYTE: u16 = 0x0f03;
pub const UART_OP_WRITE: u16 = 0x0f04;
pub const UART_OP_READ_BYTE: u16 = 0x0f05;
pub const UART_OP_GET_STATS: u16 = 0x0f06;
pub const UART_OP_CLEAR_INTERRUPTS: u16 = 0x0f07;

pub const UART_STATUS_OK: u32 = 0;
pub const UART_STATUS_TX_WOULD_BLOCK: u32 = 1;
pub const UART_STATUS_RX_WOULD_BLOCK: u32 = 2;
pub const UART_STATUS_INVALID_CONFIG: u32 = 3;
pub const UART_STATUS_INVALID_ARG: u32 = 4;
pub const UART_STATUS_UNSUPPORTED: u32 = 5;
pub const UART_STATUS_MALFORMED: u32 = 6;
pub const UART_STATUS_INTERNAL: u32 = 7;

pub const UART_FEATURE_CONFIGURE_8N1: u32 = 1 << 0;
pub const UART_FEATURE_NONBLOCKING_TX: u32 = 1 << 1;
pub const UART_FEATURE_NONBLOCKING_RX: u32 = 1 << 2;
pub const UART_FEATURE_INLINE_WRITE: u32 = 1 << 3;
pub const UART_FEATURE_STATS: u32 = 1 << 4;
pub const UART_FEATURE_CLEAR_INTERRUPTS: u32 = 1 << 5;
pub const UART_FEATURE_ALL: u32 = UART_FEATURE_CONFIGURE_8N1
    | UART_FEATURE_NONBLOCKING_TX
    | UART_FEATURE_NONBLOCKING_RX
    | UART_FEATURE_INLINE_WRITE
    | UART_FEATURE_STATS
    | UART_FEATURE_CLEAR_INTERRUPTS;

const REQUEST_HEADER_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum UartStatus {
    Ok = UART_STATUS_OK,
    TxWouldBlock = UART_STATUS_TX_WOULD_BLOCK,
    RxWouldBlock = UART_STATUS_RX_WOULD_BLOCK,
    InvalidConfig = UART_STATUS_INVALID_CONFIG,
    InvalidArg = UART_STATUS_INVALID_ARG,
    Unsupported = UART_STATUS_UNSUPPORTED,
    Malformed = UART_STATUS_MALFORMED,
    Internal = UART_STATUS_INTERNAL,
}

impl UartStatus {
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            UART_STATUS_OK => Some(Self::Ok),
            UART_STATUS_TX_WOULD_BLOCK => Some(Self::TxWouldBlock),
            UART_STATUS_RX_WOULD_BLOCK => Some(Self::RxWouldBlock),
            UART_STATUS_INVALID_CONFIG => Some(Self::InvalidConfig),
            UART_STATUS_INVALID_ARG => Some(Self::InvalidArg),
            UART_STATUS_UNSUPPORTED => Some(Self::Unsupported),
            UART_STATUS_MALFORMED => Some(Self::Malformed),
            UART_STATUS_INTERNAL => Some(Self::Internal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartCodecError {
    Malformed,
    InvalidArg,
    UnsupportedOpcode,
    MessageTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartConfig8N1 {
    pub integer_divisor: u16,
    pub fractional_divisor: u8,
    pub fifo_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartWrite {
    len: u8,
    data: [u8; UART_MAX_INLINE_WRITE],
}

impl UartWrite {
    pub fn new(bytes: &[u8]) -> Result<Self, UartCodecError> {
        if bytes.is_empty() {
            return Err(UartCodecError::InvalidArg);
        }
        if bytes.len() > UART_MAX_INLINE_WRITE {
            return Err(UartCodecError::MessageTooLarge);
        }
        let mut data = [0; UART_MAX_INLINE_WRITE];
        data[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            len: bytes.len() as u8,
            data,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartRequest {
    GetInfo,
    Configure8N1(UartConfig8N1),
    WriteByte(u8),
    Write(UartWrite),
    ReadByte,
    GetStats,
    ClearInterrupts,
}

impl UartRequest {
    pub const ENCODED_LEN: usize = UART_REQUEST_ENCODED_LEN;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut output = [0; Self::ENCODED_LEN];
        let (opcode, payload_len) = match self {
            Self::GetInfo => (UART_OP_GET_INFO, 0),
            Self::Configure8N1(config) => {
                write_u16(&mut output, REQUEST_HEADER_LEN, config.integer_divisor);
                output[REQUEST_HEADER_LEN + 2] = config.fractional_divisor;
                output[REQUEST_HEADER_LEN + 3] = u8::from(config.fifo_enabled);
                (UART_OP_CONFIGURE_8N1, 4)
            }
            Self::WriteByte(byte) => {
                output[REQUEST_HEADER_LEN] = byte;
                (UART_OP_WRITE_BYTE, 1)
            }
            Self::Write(write) => {
                let bytes = write.bytes();
                output[REQUEST_HEADER_LEN..REQUEST_HEADER_LEN + bytes.len()].copy_from_slice(bytes);
                (UART_OP_WRITE, bytes.len())
            }
            Self::ReadByte => (UART_OP_READ_BYTE, 0),
            Self::GetStats => (UART_OP_GET_STATS, 0),
            Self::ClearInterrupts => (UART_OP_CLEAR_INTERRUPTS, 0),
        };
        write_u16(&mut output, 0, opcode);
        write_u16(&mut output, 2, payload_len as u16);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, UartCodecError> {
        if input.len() != Self::ENCODED_LEN {
            return Err(UartCodecError::Malformed);
        }
        if input[4..REQUEST_HEADER_LEN].iter().any(|&byte| byte != 0) {
            return Err(UartCodecError::Malformed);
        }
        let opcode = read_u16(input, 0);
        let payload_len = usize::from(read_u16(input, 2));
        if payload_len > UART_MAX_INLINE_WRITE {
            return Err(UartCodecError::MessageTooLarge);
        }
        let payload_end = REQUEST_HEADER_LEN + payload_len;
        if input[payload_end..].iter().any(|&byte| byte != 0) {
            return Err(UartCodecError::Malformed);
        }
        let payload = &input[REQUEST_HEADER_LEN..payload_end];
        match opcode {
            UART_OP_GET_INFO | UART_OP_READ_BYTE | UART_OP_GET_STATS | UART_OP_CLEAR_INTERRUPTS => {
                if !payload.is_empty() {
                    return Err(UartCodecError::Malformed);
                }
                Ok(match opcode {
                    UART_OP_GET_INFO => Self::GetInfo,
                    UART_OP_READ_BYTE => Self::ReadByte,
                    UART_OP_GET_STATS => Self::GetStats,
                    _ => Self::ClearInterrupts,
                })
            }
            UART_OP_CONFIGURE_8N1 => {
                if payload.len() != 4 {
                    return Err(UartCodecError::Malformed);
                }
                let fifo_enabled = decode_bool(payload[3])?;
                Ok(Self::Configure8N1(UartConfig8N1 {
                    integer_divisor: read_u16(payload, 0),
                    fractional_divisor: payload[2],
                    fifo_enabled,
                }))
            }
            UART_OP_WRITE_BYTE => {
                if payload.len() != 1 {
                    return Err(UartCodecError::Malformed);
                }
                Ok(Self::WriteByte(payload[0]))
            }
            UART_OP_WRITE => Ok(Self::Write(UartWrite::new(payload)?)),
            _ => Err(UartCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartReply {
    pub status: UartStatus,
    pub abi_version: u16,
    pub max_inline_write: u16,
    pub features: u32,
    pub bytes_written: u16,
    pub byte_read: Option<u8>,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub dropped_tx_bytes: u64,
}

impl UartReply {
    pub const ENCODED_LEN: usize = UART_REPLY_ENCODED_LEN;

    pub const fn status(status: UartStatus) -> Self {
        Self {
            status,
            abi_version: 0,
            max_inline_write: 0,
            features: 0,
            bytes_written: 0,
            byte_read: None,
            tx_bytes: 0,
            rx_bytes: 0,
            dropped_tx_bytes: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut output = [0; Self::ENCODED_LEN];
        write_u32(&mut output, 0, self.status as u32);
        write_u16(&mut output, 4, self.abi_version);
        write_u16(&mut output, 6, self.max_inline_write);
        write_u32(&mut output, 8, self.features);
        write_u16(&mut output, 12, self.bytes_written);
        if let Some(byte) = self.byte_read {
            output[14] = 1;
            output[15] = byte;
        }
        write_u64(&mut output, 16, self.tx_bytes);
        write_u64(&mut output, 24, self.rx_bytes);
        write_u64(&mut output, 32, self.dropped_tx_bytes);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, UartCodecError> {
        if input.len() != Self::ENCODED_LEN {
            return Err(UartCodecError::Malformed);
        }
        let status = UartStatus::from_raw(read_u32(input, 0)).ok_or(UartCodecError::Malformed)?;
        let byte_read = match input[14] {
            0 if input[15] == 0 => None,
            1 => Some(input[15]),
            _ => return Err(UartCodecError::Malformed),
        };
        Ok(Self {
            status,
            abi_version: read_u16(input, 4),
            max_inline_write: read_u16(input, 6),
            features: read_u32(input, 8),
            bytes_written: read_u16(input, 12),
            byte_read,
            tx_bytes: read_u64(input, 16),
            rx_bytes: read_u64(input, 24),
            dropped_tx_bytes: read_u64(input, 32),
        })
    }
}

fn decode_bool(value: u8) -> Result<bool, UartCodecError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(UartCodecError::InvalidArg),
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}

fn write_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

const _: () = assert!(UART_REQUEST_ENCODED_LEN <= 128);
const _: () = assert!(UART_REPLY_ENCODED_LEN <= 128);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_and_status_values_are_stable() {
        assert_eq!(UART_ABI_VERSION, 1);
        assert_eq!(UART_MAX_INLINE_WRITE, 96);
        assert_eq!(UART_OP_GET_INFO, 0x0f01);
        assert_eq!(UART_OP_CONFIGURE_8N1, 0x0f02);
        assert_eq!(UART_OP_WRITE_BYTE, 0x0f03);
        assert_eq!(UART_OP_WRITE, 0x0f04);
        assert_eq!(UART_OP_READ_BYTE, 0x0f05);
        assert_eq!(UART_OP_GET_STATS, 0x0f06);
        assert_eq!(UART_OP_CLEAR_INTERRUPTS, 0x0f07);
        assert_eq!(UART_STATUS_OK, 0);
        assert_eq!(UART_STATUS_TX_WOULD_BLOCK, 1);
        assert_eq!(UART_STATUS_RX_WOULD_BLOCK, 2);
        assert_eq!(UART_STATUS_INVALID_CONFIG, 3);
        assert_eq!(UART_STATUS_INVALID_ARG, 4);
        assert_eq!(UART_STATUS_UNSUPPORTED, 5);
        assert_eq!(UART_STATUS_MALFORMED, 6);
        assert_eq!(UART_STATUS_INTERNAL, 7);
    }

    #[test]
    fn get_info_roundtrips() {
        let encoded = UartRequest::GetInfo.encode();
        assert_eq!(UartRequest::decode(&encoded), Ok(UartRequest::GetInfo));
    }

    #[test]
    fn configure_8n1_roundtrips() {
        let request = UartRequest::Configure8N1(UartConfig8N1 {
            integer_divisor: 26,
            fractional_divisor: 3,
            fifo_enabled: true,
        });
        assert_eq!(UartRequest::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn write_byte_roundtrips() {
        let request = UartRequest::WriteByte(b'X');
        assert_eq!(UartRequest::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn max_inline_write_roundtrips() {
        let bytes = [0xa5; UART_MAX_INLINE_WRITE];
        let request = UartRequest::Write(UartWrite::new(&bytes).unwrap());
        assert_eq!(UartRequest::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn malformed_and_truncated_requests_are_rejected() {
        let encoded = UartRequest::GetInfo.encode();
        assert_eq!(
            UartRequest::decode(&encoded[..encoded.len() - 1]),
            Err(UartCodecError::Malformed)
        );
        let mut malformed = encoded;
        malformed[4] = 1;
        assert_eq!(
            UartRequest::decode(&malformed),
            Err(UartCodecError::Malformed)
        );
    }

    #[test]
    fn write_over_max_is_rejected() {
        assert_eq!(
            UartWrite::new(&[0; UART_MAX_INLINE_WRITE + 1]),
            Err(UartCodecError::MessageTooLarge)
        );
    }
}
