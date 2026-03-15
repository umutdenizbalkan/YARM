#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsCodecError {
    Malformed,
}

pub const VFS_SERVER_ABI_VERSION: u16 = 1;
pub const VFS_CODEC_V1_VERSION: u16 = 1;

pub const VFS_OP_OPENAT: u16 = 10;
pub const VFS_OP_CLOSE: u16 = 11;
pub const VFS_OP_READ: u16 = 12;
pub const VFS_OP_WRITE: u16 = 13;
pub const VFS_OP_IOCTL: u16 = 14;
pub const VFS_OP_DUP: u16 = 15;
pub const VFS_OP_FCNTL: u16 = 16;
pub const VFS_OP_POLL: u16 = 17;
pub const VFS_OP_EPOLL_CREATE1: u16 = 18;
pub const VFS_OP_EPOLL_CTL: u16 = 19;
pub const VFS_OP_EPOLL_PWAIT: u16 = 20;
pub const VFS_OP_SENDFILE: u16 = 21;
pub const VFS_OP_STATX: u16 = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsV1Args {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
}

impl VfsV1Args {
    pub const VERSION: u16 = VFS_CODEC_V1_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> Self {
        Self {
            arg0,
            arg1,
            arg2,
            arg3,
        }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let values = [self.arg0, self.arg1, self.arg2, self.arg3];
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

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        if payload.len() < Self::ENCODED_LEN {
            return Err(VfsCodecError::Malformed);
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
        Ok(Self::new(values[0], values[1], values[2], values[3]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfs_v1_roundtrip() {
        let args = VfsV1Args::new(1, 2, 3, 4);
        let enc = args.encode();
        assert_eq!(VfsV1Args::decode(&enc), Ok(args));
    }
}
