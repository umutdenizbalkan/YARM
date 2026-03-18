//! log.srv IPC contract for freestanding-friendly userspace logging.

pub const LOG_SERVER_ABI_VERSION: u16 = 1;
pub const LOG_OP_WRITE: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogSeverity {
    Error = 3,
    Warn = 4,
    Info = 6,
    Debug = 7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogV1Args {
    pub severity: u8,
    pub message_ptr: u64,
    pub message_len: u32,
    pub reserved: u32,
}

impl LogV1Args {
    pub const ENCODED_LEN: usize = 24;

    pub const fn new(severity: LogSeverity, message_ptr: u64, message_len: u32) -> Self {
        Self {
            severity: severity as u8,
            message_ptr,
            message_len,
            reserved: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0] = self.severity;
        out[8..16].copy_from_slice(&self.message_ptr.to_le_bytes());
        out[16..20].copy_from_slice(&self.message_len.to_le_bytes());
        out[20..24].copy_from_slice(&self.reserved.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != Self::ENCODED_LEN {
            return Err(());
        }
        let mut ptr = [0u8; 8];
        let mut len = [0u8; 4];
        let mut reserved = [0u8; 4];
        ptr.copy_from_slice(&bytes[8..16]);
        len.copy_from_slice(&bytes[16..20]);
        reserved.copy_from_slice(&bytes[20..24]);
        Ok(Self {
            severity: bytes[0],
            message_ptr: u64::from_le_bytes(ptr),
            message_len: u32::from_le_bytes(len),
            reserved: u32::from_le_bytes(reserved),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_v1_args_roundtrip() {
        let args = LogV1Args::new(LogSeverity::Info, 0x1000, 17);
        let encoded = args.encode();
        assert_eq!(LogV1Args::decode(&encoded).expect("decode"), args);
    }

    #[test]
    fn log_contract_constants_stable() {
        assert_eq!(LOG_SERVER_ABI_VERSION, 1);
        assert_eq!(LOG_OP_WRITE, 1);
        assert_eq!(LogV1Args::ENCODED_LEN, 24);
    }
}
