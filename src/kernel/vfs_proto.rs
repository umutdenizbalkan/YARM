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
pub struct OpenAtArgs {
    pub dirfd: u64,
    pub path_ptr: u64,
    pub flags: u64,
    pub mode: u64,
}

impl OpenAtArgs {
    pub const VERSION: u16 = VFS_CODEC_V1_VERSION;

    pub const fn new(dirfd: u64, path_ptr: u64, flags: u64, mode: u64) -> Self {
        Self {
            dirfd,
            path_ptr,
            flags,
            mode,
        }
    }

    pub const fn encode(self) -> [u8; VfsV1Args::ENCODED_LEN] {
        VfsV1Args::new(self.dirfd, self.path_ptr, self.flags, self.mode).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        let args = VfsV1Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2, args.arg3))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWriteArgs {
    pub fd: u64,
    pub buf_ptr: u64,
    pub len: u64,
}

impl ReadWriteArgs {
    pub const VERSION: u16 = VFS_CODEC_V1_VERSION;

    pub const fn new(fd: u64, buf_ptr: u64, len: u64) -> Self {
        Self { fd, buf_ptr, len }
    }

    pub const fn encode(self) -> [u8; VfsV1Args::ENCODED_LEN] {
        VfsV1Args::new(self.fd, self.buf_ptr, self.len, 0).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        let args = VfsV1Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatxArgs {
    pub dirfd: u64,
    pub path_ptr: u64,
    pub flags: u64,
    pub mask_or_buf: u64,
}

impl StatxArgs {
    pub const VERSION: u16 = VFS_CODEC_V1_VERSION;

    pub const fn new(dirfd: u64, path_ptr: u64, flags: u64, mask_or_buf: u64) -> Self {
        Self {
            dirfd,
            path_ptr,
            flags,
            mask_or_buf,
        }
    }

    pub const fn encode(self) -> [u8; VfsV1Args::ENCODED_LEN] {
        VfsV1Args::new(self.dirfd, self.path_ptr, self.flags, self.mask_or_buf).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        let args = VfsV1Args::decode(payload)?;
        Ok(Self::new(args.arg0, args.arg1, args.arg2, args.arg3))
    }
}

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
        if payload.len() != Self::ENCODED_LEN {
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


    #[test]
    fn vfs_v1_rejects_non_exact_payload_lengths() {
        let short = [0u8; VfsV1Args::ENCODED_LEN - 1];
        assert_eq!(VfsV1Args::decode(&short), Err(VfsCodecError::Malformed));

        let long = [0u8; VfsV1Args::ENCODED_LEN + 1];
        assert_eq!(VfsV1Args::decode(&long), Err(VfsCodecError::Malformed));
    }

    #[test]
    fn vfs_v1_constants_are_stable() {
        assert_eq!(VFS_SERVER_ABI_VERSION, 1);
        assert_eq!(VFS_CODEC_V1_VERSION, 1);
        assert_eq!(VfsV1Args::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(VfsV1Args::ENCODED_LEN, 32);
        assert_eq!(OpenAtArgs::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(ReadWriteArgs::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(StatxArgs::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(VFS_OP_OPENAT, 10);
        assert_eq!(VFS_OP_READ, 12);
    }

    #[test]
    fn typed_vfs_wrappers_roundtrip_via_frozen_codec() {
        let open = OpenAtArgs::new(1, 2, 3, 4);
        assert_eq!(OpenAtArgs::decode(&open.encode()), Ok(open));

        let rw = ReadWriteArgs::new(7, 8, 9);
        assert_eq!(ReadWriteArgs::decode(&rw.encode()), Ok(rw));

        let stat = StatxArgs::new(10, 11, 12, 13);
        assert_eq!(StatxArgs::decode(&stat.encode()), Ok(stat));
    }
}
