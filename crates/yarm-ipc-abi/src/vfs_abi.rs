// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsCodecError {
    Malformed,
    UnsupportedFlags,
    InvalidDescriptor,
    RangeOverflow,
    PermissionMismatch,
}

pub const VFS_SERVER_ABI_VERSION: u16 = 1;

// ── VFS control-plane reply status codes ────────────────────────────────────
//
// Locally-generated error replies from the VFS router carry a 4-byte LE u32
// status in the reply payload (opcode=1 on error, opcode=0 on success).
// Backend success replies are forwarded verbatim and carry opcode=0.

pub const VFS_STATUS_OK: u32 = 0;
/// Opcode not handled by the VFS router (not STATX/OPENAT/READ/CLOSE).
pub const VFS_STATUS_ERR_UNKNOWN_OP: u32 = 1;
/// Path payload malformed, or path fails normalization (empty/non-abs/too-long).
pub const VFS_STATUS_ERR_INVALID_PATH: u32 = 2;
/// Normalized path matches no mount-table prefix.
pub const VFS_STATUS_ERR_NO_MOUNT: u32 = 3;
/// Fd absent from the table, or table entry belongs to a different client.
pub const VFS_STATUS_ERR_BAD_FD: u32 = 4;
/// Backend IPC timed out or returned no reply.
pub const VFS_STATUS_ERR_BACKEND: u32 = 5;
/// READ/CLOSE payload could not be decoded.
pub const VFS_STATUS_ERR_CODEC: u32 = 6;

/// Encode a VFS control-plane status as a 4-byte little-endian payload.
pub fn encode_vfs_status(status: u32) -> [u8; 4] {
    status.to_le_bytes()
}

/// Decode the leading 4-byte LE status from a reply payload.
/// Returns `None` if `payload` is shorter than 4 bytes.
pub fn decode_vfs_status(payload: &[u8]) -> Option<u32> {
    if payload.len() < 4 {
        return None;
    }
    let mut b = [0u8; 4];
    b.copy_from_slice(&payload[..4]);
    Some(u32::from_le_bytes(b))
}
pub const VFS_CODEC_V1_VERSION: u16 = 1;

pub const VFS_OP_STATX: u16 = 22;
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
pub const VFS_OP_MOUNT_REGISTER: u16 = 23;
/// Phase 2 bulk read opcode (4 KiB per IPC round trip via kernel-mediated copy bridge).
/// VFS router routes this via fd-table lookup, same as VFS_OP_READ.
/// Phase 3 will replace the kernel-mediated copy with page-cap zero-copy.
pub const VFS_OP_READ_BULK: u16 = 24;
/// Phase 3A: Request a read-only MemoryObject capability for the full content of an
/// already-opened file descriptor.  VFS routes this via fd-table lookup to the backend
/// (initramfs_srv).  The reply carries a transferred MemoryObject cap and the file length.
pub const VFS_OP_FILE_GRANT_RO: u16 = 25;
/// FS-11 reserved service opcode for the helper-only READ_SHARED_REPLY protocol.
/// No router or filesystem backend dispatches this opcode yet.
pub const VFS_OP_READ_SHARED_REPLY: u16 = 26;
/// FS-11 reserved service opcode for the helper-only WRITE_SHARED_REQUEST protocol.
/// No router or filesystem backend dispatches this opcode yet.
pub const VFS_OP_WRITE_SHARED_REQUEST: u16 = 27;
pub const VFS_OPENAT_INLINE_PATH_MAX: usize = 96;
pub const VFS_OPENAT_INLINE_PATH_HEADER_BYTES: usize = 25;
pub const VFS_OPENAT_INLINE_PATH_MAX_BYTES: usize =
    VFS_OPENAT_INLINE_PATH_HEADER_BYTES + VFS_OPENAT_INLINE_PATH_MAX;
pub const VFS_STATX_INLINE_PATH_MAX: usize = 96;
pub const VFS_STATX_INLINE_PATH_HEADER_BYTES: usize = 25;
pub const VFS_STATX_INLINE_PATH_MAX_BYTES: usize =
    VFS_STATX_INLINE_PATH_HEADER_BYTES + VFS_STATX_INLINE_PATH_MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAtInlinePath<'a> {
    pub dirfd: u64,
    pub flags: u64,
    pub mode: u64,
    pub path: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatxInlinePath<'a> {
    pub dirfd: u64,
    pub flags: u64,
    pub mask_or_buf: u64,
    pub path: &'a [u8],
}

impl<'a> StatxInlinePath<'a> {
    pub fn encode(self) -> Option<([u8; VFS_STATX_INLINE_PATH_MAX_BYTES], usize)> {
        if self.path.is_empty() || self.path.len() > VFS_STATX_INLINE_PATH_MAX {
            return None;
        }
        let mut out = [0u8; VFS_STATX_INLINE_PATH_MAX_BYTES];
        out[0..8].copy_from_slice(&self.dirfd.to_le_bytes());
        out[8..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16..24].copy_from_slice(&self.mask_or_buf.to_le_bytes());
        out[24] = self.path.len() as u8;
        out[25..25 + self.path.len()].copy_from_slice(self.path);
        Some((out, VFS_STATX_INLINE_PATH_HEADER_BYTES + self.path.len()))
    }

    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < VFS_STATX_INLINE_PATH_HEADER_BYTES {
            return None;
        }
        let path_len = bytes[24] as usize;
        if path_len == 0 || path_len > VFS_STATX_INLINE_PATH_MAX {
            return None;
        }
        let total = VFS_STATX_INLINE_PATH_HEADER_BYTES + path_len;
        if bytes.len() < total {
            return None;
        }
        let mut dirfd = [0u8; 8];
        let mut flags = [0u8; 8];
        let mut mask_or_buf = [0u8; 8];
        dirfd.copy_from_slice(&bytes[0..8]);
        flags.copy_from_slice(&bytes[8..16]);
        mask_or_buf.copy_from_slice(&bytes[16..24]);
        Some(Self {
            dirfd: u64::from_le_bytes(dirfd),
            flags: u64::from_le_bytes(flags),
            mask_or_buf: u64::from_le_bytes(mask_or_buf),
            path: &bytes[25..25 + path_len],
        })
    }
}

impl<'a> OpenAtInlinePath<'a> {
    pub fn encode(self) -> Option<([u8; VFS_OPENAT_INLINE_PATH_MAX_BYTES], usize)> {
        if self.path.is_empty() || self.path.len() > VFS_OPENAT_INLINE_PATH_MAX {
            return None;
        }
        let mut out = [0u8; VFS_OPENAT_INLINE_PATH_MAX_BYTES];
        out[0..8].copy_from_slice(&self.dirfd.to_le_bytes());
        out[8..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16..24].copy_from_slice(&self.mode.to_le_bytes());
        out[24] = self.path.len() as u8;
        out[25..25 + self.path.len()].copy_from_slice(self.path);
        Some((out, VFS_OPENAT_INLINE_PATH_HEADER_BYTES + self.path.len()))
    }

    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < VFS_OPENAT_INLINE_PATH_HEADER_BYTES {
            return None;
        }
        let path_len = bytes[24] as usize;
        if path_len == 0 || path_len > VFS_OPENAT_INLINE_PATH_MAX {
            return None;
        }
        let total = VFS_OPENAT_INLINE_PATH_HEADER_BYTES + path_len;
        if bytes.len() < total {
            return None;
        }
        let mut dirfd = [0u8; 8];
        let mut flags = [0u8; 8];
        let mut mode = [0u8; 8];
        dirfd.copy_from_slice(&bytes[0..8]);
        flags.copy_from_slice(&bytes[8..16]);
        mode.copy_from_slice(&bytes[16..24]);
        Some(Self {
            dirfd: u64::from_le_bytes(dirfd),
            flags: u64::from_le_bytes(flags),
            mode: u64::from_le_bytes(mode),
            path: &bytes[25..25 + path_len],
        })
    }
}

// ── VFS_OP_MOUNT_REGISTER ────────────────────────────────────────────────────

/// Maximum byte length of a mount-register prefix in the wire payload.
/// Matches `VFS_INLINE_PATH_MAX`; the server normalizes and appends `/`.
pub const VFS_MOUNT_REGISTER_PREFIX_MAX: usize = 96;

/// Byte offset at which the inline prefix starts in the request payload.
pub const VFS_MOUNT_REGISTER_HEADER_BYTES: usize = 17; // 8 + 8 + 1

/// Maximum total byte length of a `MountRegisterArgs` payload.
pub const VFS_MOUNT_REGISTER_MAX_BYTES: usize =
    VFS_MOUNT_REGISTER_HEADER_BYTES + VFS_MOUNT_REGISTER_PREFIX_MAX;

// Reply status codes embedded in the 4-byte LE payload of the reply message.
pub const VFS_MOUNT_STATUS_OK: u32 = 0;
pub const VFS_MOUNT_STATUS_ERR_DUPLICATE: u32 = 1;
pub const VFS_MOUNT_STATUS_ERR_FULL: u32 = 2;
pub const VFS_MOUNT_STATUS_ERR_INVALID_CAP: u32 = 3;
pub const VFS_MOUNT_STATUS_ERR_INVALID_PREFIX: u32 = 4;

/// Wire layout of a `VFS_OP_MOUNT_REGISTER` request payload.
///
/// ```text
/// offset  size  field
/// ------  ----  -----
///      0     8  backend_send_cap   LE u64 — capability to backend service
///      8     8  flags              LE u64 — mount flags (0 = none)
///     16     1  prefix_len         byte count of the inline prefix (1..=96)
///     17     N  prefix             raw path bytes, N = prefix_len
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRegisterArgs<'a> {
    pub backend_send_cap: u64,
    pub flags: u64,
    pub prefix: &'a [u8],
}

impl<'a> MountRegisterArgs<'a> {
    pub fn encode(self) -> Option<([u8; VFS_MOUNT_REGISTER_MAX_BYTES], usize)> {
        if self.prefix.is_empty() || self.prefix.len() > VFS_MOUNT_REGISTER_PREFIX_MAX {
            return None;
        }
        let mut out = [0u8; VFS_MOUNT_REGISTER_MAX_BYTES];
        out[0..8].copy_from_slice(&self.backend_send_cap.to_le_bytes());
        out[8..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16] = self.prefix.len() as u8;
        out[17..17 + self.prefix.len()].copy_from_slice(self.prefix);
        Some((out, VFS_MOUNT_REGISTER_HEADER_BYTES + self.prefix.len()))
    }

    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < VFS_MOUNT_REGISTER_HEADER_BYTES {
            return None;
        }
        let prefix_len = bytes[16] as usize;
        if prefix_len == 0 || prefix_len > VFS_MOUNT_REGISTER_PREFIX_MAX {
            return None;
        }
        let total = VFS_MOUNT_REGISTER_HEADER_BYTES + prefix_len;
        if bytes.len() < total {
            return None;
        }
        let mut cap_bytes = [0u8; 8];
        let mut flags_bytes = [0u8; 8];
        cap_bytes.copy_from_slice(&bytes[0..8]);
        flags_bytes.copy_from_slice(&bytes[8..16]);
        Some(Self {
            backend_send_cap: u64::from_le_bytes(cap_bytes),
            flags: u64::from_le_bytes(flags_bytes),
            prefix: &bytes[17..17 + prefix_len],
        })
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

/// Shared-I/O capability bits advertised by a future VFS/filesystem handshake.
/// FS-11 defines the wire model only; no live endpoint advertises either bit.
pub const VFS_SHARED_IO_CAP_READ_SHARED_REPLY: u32 = 1 << 0;
pub const VFS_SHARED_IO_CAP_WRITE_SHARED_REQUEST: u32 = 1 << 1;
pub const VFS_SHARED_IO_CAP_KNOWN: u32 =
    VFS_SHARED_IO_CAP_READ_SHARED_REPLY | VFS_SHARED_IO_CAP_WRITE_SHARED_REQUEST;

/// Request uses the open file description's current offset. When set, `file_offset` must be zero.
pub const VFS_SHARED_IO_F_CURRENT_OFFSET: u32 = 1 << 0;
/// Requester permits the endpoint to fall back to the existing inline protocol.
pub const VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK: u32 = 1 << 1;
pub const VFS_SHARED_IO_F_KNOWN: u32 =
    VFS_SHARED_IO_F_CURRENT_OFFSET | VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK;

/// The filesystem server may read, but must not mutate, the shared object.
pub const VFS_SHARED_BUFFER_FS_READ: u32 = 1 << 0;
/// The filesystem server may write the shared object for a read reply.
pub const VFS_SHARED_BUFFER_FS_WRITE: u32 = 1 << 1;
pub const VFS_SHARED_BUFFER_ACCESS_KNOWN: u32 =
    VFS_SHARED_BUFFER_FS_READ | VFS_SHARED_BUFFER_FS_WRITE;

pub const VFS_SHARED_IO_STATUS_OK: u32 = 0;
pub const VFS_SHARED_IO_STATUS_INVALID_DESCRIPTOR: u32 = 1;
pub const VFS_SHARED_IO_STATUS_PERMISSION: u32 = 2;
pub const VFS_SHARED_IO_STATUS_BACKEND: u32 = 3;
pub const VFS_SHARED_IO_STATUS_CANCELLED: u32 = 4;
pub const VFS_SHARED_IO_STATUS_UNSUPPORTED: u32 = 5;

/// Helper-only placeholder for a future userspace-visible MemoryObject transfer.
///
/// `object_handle` and `object_generation` are opaque correlation fields. They are not kernel
/// capability slots and FS-11 does not define how they are transferred, mapped, or revoked.
/// Live code must validate the actual object size and rights after a future transfer primitive
/// resolves this descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsSharedBufferDescriptor {
    pub object_handle: u64,
    pub object_generation: u64,
    pub buffer_offset: u64,
    pub buffer_len: u64,
    pub access: u32,
}

impl VfsSharedBufferDescriptor {
    pub const ENCODED_LEN: usize = 40;

    pub const fn new(
        object_handle: u64,
        object_generation: u64,
        buffer_offset: u64,
        buffer_len: u64,
        access: u32,
    ) -> Self {
        Self {
            object_handle,
            object_generation,
            buffer_offset,
            buffer_len,
            access,
        }
    }

    pub fn validate(self, required_access: u32, requested_len: u64) -> Result<(), VfsCodecError> {
        if self.object_handle == 0 || self.buffer_len < requested_len {
            return Err(VfsCodecError::InvalidDescriptor);
        }
        if self.access & !VFS_SHARED_BUFFER_ACCESS_KNOWN != 0 {
            return Err(VfsCodecError::UnsupportedFlags);
        }
        if self.access != required_access {
            return Err(VfsCodecError::PermissionMismatch);
        }
        self.buffer_offset
            .checked_add(self.buffer_len)
            .ok_or(VfsCodecError::RangeOverflow)?;
        Ok(())
    }

    fn encode_into(self, out: &mut [u8]) {
        out[0..8].copy_from_slice(&self.object_handle.to_le_bytes());
        out[8..16].copy_from_slice(&self.object_generation.to_le_bytes());
        out[16..24].copy_from_slice(&self.buffer_offset.to_le_bytes());
        out[24..32].copy_from_slice(&self.buffer_len.to_le_bytes());
        out[32..36].copy_from_slice(&self.access.to_le_bytes());
        // bytes 36..40 are reserved and remain zero
    }

    fn decode_from(bytes: &[u8]) -> Result<Self, VfsCodecError> {
        if bytes.len() != Self::ENCODED_LEN || bytes[36..40] != [0; 4] {
            return Err(VfsCodecError::Malformed);
        }
        Ok(Self {
            object_handle: decode_u64_at(bytes, 0)?,
            object_generation: decode_u64_at(bytes, 8)?,
            buffer_offset: decode_u64_at(bytes, 16)?,
            buffer_len: decode_u64_at(bytes, 24)?,
            access: decode_u32_at(bytes, 32)?,
        })
    }
}

macro_rules! shared_request {
    ($name:ident, $required_access:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            pub fd: u64,
            pub file_offset: u64,
            pub requested_len: u64,
            pub request_id: u64,
            pub flags: u32,
            pub buffer: VfsSharedBufferDescriptor,
        }

        impl $name {
            pub const ENCODED_LEN: usize = 80;

            pub fn validate(self) -> Result<(), VfsCodecError> {
                if self.flags & !VFS_SHARED_IO_F_KNOWN != 0 {
                    return Err(VfsCodecError::UnsupportedFlags);
                }
                if self.flags & VFS_SHARED_IO_F_CURRENT_OFFSET != 0 && self.file_offset != 0 {
                    return Err(VfsCodecError::Malformed);
                }
                self.buffer.validate($required_access, self.requested_len)
            }

            pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], VfsCodecError> {
                self.validate()?;
                let mut out = [0u8; Self::ENCODED_LEN];
                out[0..8].copy_from_slice(&self.fd.to_le_bytes());
                out[8..16].copy_from_slice(&self.file_offset.to_le_bytes());
                out[16..24].copy_from_slice(&self.requested_len.to_le_bytes());
                out[24..32].copy_from_slice(&self.request_id.to_le_bytes());
                out[32..36].copy_from_slice(&self.flags.to_le_bytes());
                // bytes 36..40 are reserved and remain zero
                self.buffer.encode_into(&mut out[40..80]);
                Ok(out)
            }

            pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
                if payload.len() != Self::ENCODED_LEN || payload[36..40] != [0; 4] {
                    return Err(VfsCodecError::Malformed);
                }
                let value = Self {
                    fd: decode_u64_at(payload, 0)?,
                    file_offset: decode_u64_at(payload, 8)?,
                    requested_len: decode_u64_at(payload, 16)?,
                    request_id: decode_u64_at(payload, 24)?,
                    flags: decode_u32_at(payload, 32)?,
                    buffer: VfsSharedBufferDescriptor::decode_from(&payload[40..80])?,
                };
                value.validate()?;
                Ok(value)
            }
        }
    };
}

shared_request!(VfsReadSharedRequest, VFS_SHARED_BUFFER_FS_WRITE);
shared_request!(VfsWriteSharedRequest, VFS_SHARED_BUFFER_FS_READ);

macro_rules! shared_reply {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            pub request_id: u64,
            pub bytes_completed: u64,
            pub status: u32,
            pub flags: u32,
        }

        impl $name {
            pub const ENCODED_LEN: usize = 24;

            pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], VfsCodecError> {
                if self.flags != 0 {
                    return Err(VfsCodecError::UnsupportedFlags);
                }
                let mut out = [0u8; Self::ENCODED_LEN];
                out[0..8].copy_from_slice(&self.request_id.to_le_bytes());
                out[8..16].copy_from_slice(&self.bytes_completed.to_le_bytes());
                out[16..20].copy_from_slice(&self.status.to_le_bytes());
                out[20..24].copy_from_slice(&self.flags.to_le_bytes());
                Ok(out)
            }

            pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
                if payload.len() != Self::ENCODED_LEN {
                    return Err(VfsCodecError::Malformed);
                }
                let value = Self {
                    request_id: decode_u64_at(payload, 0)?,
                    bytes_completed: decode_u64_at(payload, 8)?,
                    status: decode_u32_at(payload, 16)?,
                    flags: decode_u32_at(payload, 20)?,
                };
                if value.flags != 0 {
                    return Err(VfsCodecError::UnsupportedFlags);
                }
                Ok(value)
            }
        }
    };
}

shared_reply!(VfsReadSharedReply);
shared_reply!(VfsWriteSharedReply);

fn decode_u64_at(bytes: &[u8], offset: usize) -> Result<u64, VfsCodecError> {
    let input = bytes
        .get(offset..offset + 8)
        .ok_or(VfsCodecError::Malformed)?;
    let mut value = [0u8; 8];
    value.copy_from_slice(input);
    Ok(u64::from_le_bytes(value))
}

fn decode_u32_at(bytes: &[u8], offset: usize) -> Result<u32, VfsCodecError> {
    let input = bytes
        .get(offset..offset + 4)
        .ok_or(VfsCodecError::Malformed)?;
    let mut value = [0u8; 4];
    value.copy_from_slice(input);
    Ok(u32::from_le_bytes(value))
}

/// Wire format for VFS_OP_READ_BULK requests (fits in 32 bytes).
///
/// Layout (32 bytes, LE):
/// ```text
/// offset  size  field
///      0     8  fd              file descriptor
///      8     8  requested_len   bytes to read (≤4096)
///     16     8  offset          byte offset in file (absolute)
///     24     8  dst_ptr         caller's VA for transfer buffer
///                               (Phase 2B: PM passes its bulk_buf address here;
///                                was "reserved=0" before Phase 2B — wire-compatible)
/// ```
///
/// Phase 2B transfer-buffer bridge: PM fills `dst_ptr` with its own stack buffer VA.
/// initramfs_srv calls the kernel (syscall nr=27 with target_tid=PM_TID) to write
/// CPIO data directly into PM's buffer.  PM reads from its stack buffer after the
/// IPC round trip completes.
///
/// Missing primitive for pure page-cap: `MemoryObject` capability grant that lets
/// initramfs_srv write to PM's page without kernel-mediated cross-ASID copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkReadArgs {
    pub fd: u64,
    pub requested_len: u64,
    pub offset: u64,
    /// Phase 2B: caller's destination buffer VA (was reserved=0 before Phase 2B).
    /// Wire-compatible with prior encoding (reserved field promoted to named field).
    pub dst_ptr: u64,
}

impl BulkReadArgs {
    pub const fn new(fd: u64, requested_len: u64, offset: u64) -> Self {
        Self {
            fd,
            requested_len,
            offset,
            dst_ptr: 0,
        }
    }

    pub const fn new_with_dst(fd: u64, requested_len: u64, offset: u64, dst_ptr: u64) -> Self {
        Self {
            fd,
            requested_len,
            offset,
            dst_ptr,
        }
    }

    pub const fn encode(self) -> [u8; VfsV1Args::ENCODED_LEN] {
        VfsV1Args::new(self.fd, self.requested_len, self.offset, self.dst_ptr).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        let args = VfsV1Args::decode(payload)?;
        Ok(Self {
            fd: args.arg0,
            requested_len: args.arg1,
            offset: args.arg2,
            dst_ptr: args.arg3,
        })
    }
}

/// Reply payload for VFS_OP_READ_BULK (12 bytes).
///
/// Layout:
/// ```text
/// offset  size  field
///      0     8  copied_len  bytes actually copied into caller's buffer
///      8     1  eof         1 = end of file reached, 0 = more data available
///      9     3  pad         reserved, zero
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkReadReply {
    pub copied_len: u64,
    pub eof: bool,
}

impl BulkReadReply {
    pub const ENCODED_LEN: usize = 12;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..8].copy_from_slice(&self.copied_len.to_le_bytes());
        out[8] = if self.eof { 1 } else { 0 };
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut b = [0u8; 8];
        b.copy_from_slice(&payload[0..8]);
        let copied_len = u64::from_le_bytes(b);
        let eof = payload[8] != 0;
        Some(Self { copied_len, eof })
    }
}

/// Wire format for VFS_OP_FILE_GRANT_RO requests (32 bytes, 4 × u64 LE).
///
/// Phase 3A: requests a read-only MemoryObject cap for the file referenced by `fd`.
/// `flags`, `offset`, `len` are reserved for future use and must be 0 in Phase 3A.
///
/// Layout:
/// ```text
/// offset  size  field
///      0     8  fd       file descriptor (already opened by caller)
///      8     8  flags    reserved — must be 0
///     16     8  offset   reserved — must be 0 (full-file grant only in Phase 3A)
///     24     8  len      reserved — must be 0 (full-file grant only in Phase 3A)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileGrantRoArgs {
    pub fd: u64,
    pub flags: u64,
    pub offset: u64,
    pub len: u64,
}

impl FileGrantRoArgs {
    pub const fn new(fd: u64) -> Self {
        Self {
            fd,
            flags: 0,
            offset: 0,
            len: 0,
        }
    }

    pub const fn encode(self) -> [u8; VfsV1Args::ENCODED_LEN] {
        VfsV1Args::new(self.fd, self.flags, self.offset, self.len).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, VfsCodecError> {
        let args = VfsV1Args::decode(payload)?;
        Ok(Self {
            fd: args.arg0,
            flags: args.arg1,
            offset: args.arg2,
            len: args.arg3,
        })
    }
}

/// Reply payload for VFS_OP_FILE_GRANT_RO (12 bytes).
///
/// The reply MESSAGE also carries a transferred MemoryObject cap via FLAG_CAP_TRANSFER.
/// The payload contains the file length for convenience so the receiver does not need
/// to stat the file separately.
///
/// Layout:
/// ```text
/// offset  size  field
///      0     8  file_len  exact file data length in bytes (u64 LE)
///      8     4  status    0=ok, non-zero=error (u32 LE)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileGrantRoReply {
    pub file_len: u64,
    pub status: u32,
}

impl FileGrantRoReply {
    pub const ENCODED_LEN: usize = 12;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..8].copy_from_slice(&self.file_len.to_le_bytes());
        out[8..12].copy_from_slice(&self.status.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::ENCODED_LEN {
            return None;
        }
        let mut b8 = [0u8; 8];
        let mut b4 = [0u8; 4];
        b8.copy_from_slice(&payload[0..8]);
        b4.copy_from_slice(&payload[8..12]);
        Some(Self {
            file_len: u64::from_le_bytes(b8),
            status: u32::from_le_bytes(b4),
        })
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
        assert_eq!(ReadWriteArgs::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(VFS_OP_OPENAT, 10);
        assert_eq!(VFS_OP_READ, 12);
    }

    #[test]
    fn typed_vfs_wrappers_roundtrip_via_frozen_codec() {
        let rw = ReadWriteArgs::new(7, 8, 9);
        assert_eq!(ReadWriteArgs::decode(&rw.encode()), Ok(rw));
    }

    #[test]
    fn openat_inline_path_roundtrip() {
        let (encoded, len) = OpenAtInlinePath {
            dirfd: 1,
            flags: 2,
            mode: 3,
            path: b"/initramfs/boot-marker",
        }
        .encode()
        .expect("encode");
        let decoded = OpenAtInlinePath::decode(&encoded[..len]).expect("decode");
        assert_eq!(decoded.dirfd, 1);
        assert_eq!(decoded.flags, 2);
        assert_eq!(decoded.mode, 3);
        assert_eq!(decoded.path, b"/initramfs/boot-marker");
    }

    #[test]
    fn statx_inline_path_roundtrip() {
        let (encoded, len) = StatxInlinePath {
            dirfd: 4,
            flags: 5,
            mask_or_buf: 6,
            path: b"/initramfs/vfs",
        }
        .encode()
        .expect("encode");
        let decoded = StatxInlinePath::decode(&encoded[..len]).expect("decode");
        assert_eq!(decoded.dirfd, 4);
        assert_eq!(decoded.flags, 5);
        assert_eq!(decoded.mask_or_buf, 6);
        assert_eq!(decoded.path, b"/initramfs/vfs");
    }

    #[test]
    fn vfs_v1_golden_vector_is_stable() {
        let args = VfsV1Args::new(
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            0x2122_2324_2526_2728,
            0x3132_3334_3536_3738,
        );
        let expected = [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // arg0 LE
            0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, // arg1 LE
            0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, // arg2 LE
            0x38, 0x37, 0x36, 0x35, 0x34, 0x33, 0x32, 0x31, // arg3 LE
        ];
        assert_eq!(args.encode(), expected);
        assert_eq!(VfsV1Args::decode(&expected), Ok(args));
    }

    #[test]
    fn mount_register_args_roundtrip() {
        let args = MountRegisterArgs {
            backend_send_cap: 0xDEAD_BEEF_0000_0001,
            flags: 0,
            prefix: b"/initramfs/",
        };
        let (encoded, len) = args.encode().expect("encode");
        let decoded = MountRegisterArgs::decode(&encoded[..len]).expect("decode");
        assert_eq!(decoded.backend_send_cap, args.backend_send_cap);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.prefix, b"/initramfs/");
    }

    #[test]
    fn mount_register_args_golden_vector_is_stable() {
        // cap=0x0102030405060708, flags=0, prefix=b"/dev/"
        let args = MountRegisterArgs {
            backend_send_cap: 0x0102_0304_0506_0708,
            flags: 0,
            prefix: b"/dev/",
        };
        let (encoded, len) = args.encode().expect("encode");
        assert_eq!(len, 22); // 17 header + 5 prefix bytes
                             // bytes 0-7: cap LE
        assert_eq!(
            &encoded[0..8],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        // bytes 8-15: flags LE (zero)
        assert_eq!(&encoded[8..16], &[0u8; 8]);
        // byte 16: prefix_len = 5
        assert_eq!(encoded[16], 5);
        // bytes 17-21: b"/dev/"
        assert_eq!(&encoded[17..22], b"/dev/");
    }

    #[test]
    fn mount_register_args_rejects_empty_prefix() {
        let args = MountRegisterArgs {
            backend_send_cap: 1,
            flags: 0,
            prefix: b"",
        };
        assert!(args.encode().is_none());
    }

    #[test]
    fn mount_register_args_rejects_oversized_prefix() {
        let long_prefix = [b'a'; VFS_MOUNT_REGISTER_PREFIX_MAX + 1];
        let args = MountRegisterArgs {
            backend_send_cap: 1,
            flags: 0,
            prefix: &long_prefix,
        };
        assert!(args.encode().is_none());
    }

    #[test]
    fn mount_register_args_decode_rejects_short_payload() {
        let short = [0u8; VFS_MOUNT_REGISTER_HEADER_BYTES - 1];
        assert!(MountRegisterArgs::decode(&short).is_none());
    }

    #[test]
    fn mount_register_args_decode_rejects_truncated_prefix() {
        // Build a valid encoding, then trim it
        let args = MountRegisterArgs {
            backend_send_cap: 1,
            flags: 0,
            prefix: b"/initramfs/",
        };
        let (encoded, len) = args.encode().expect("encode");
        // Remove 3 bytes from the end — prefix is now truncated
        assert!(MountRegisterArgs::decode(&encoded[..len - 3]).is_none());
    }

    #[test]
    fn mount_register_opcode_and_status_constants_are_stable() {
        assert_eq!(VFS_OP_MOUNT_REGISTER, 23);
        assert_eq!(VFS_MOUNT_STATUS_OK, 0);
        assert_eq!(VFS_MOUNT_STATUS_ERR_DUPLICATE, 1);
        assert_eq!(VFS_MOUNT_STATUS_ERR_FULL, 2);
        assert_eq!(VFS_MOUNT_STATUS_ERR_INVALID_CAP, 3);
        assert_eq!(VFS_MOUNT_STATUS_ERR_INVALID_PREFIX, 4);
    }

    // ── VFS router status code tests ─────────────────────────────────────────

    #[test]
    fn vfs_status_constants_are_frozen() {
        assert_eq!(VFS_STATUS_OK, 0);
        assert_eq!(VFS_STATUS_ERR_UNKNOWN_OP, 1);
        assert_eq!(VFS_STATUS_ERR_INVALID_PATH, 2);
        assert_eq!(VFS_STATUS_ERR_NO_MOUNT, 3);
        assert_eq!(VFS_STATUS_ERR_BAD_FD, 4);
        assert_eq!(VFS_STATUS_ERR_BACKEND, 5);
        assert_eq!(VFS_STATUS_ERR_CODEC, 6);
    }

    #[test]
    fn encode_decode_vfs_status_roundtrips_all_codes() {
        let codes = [
            VFS_STATUS_OK,
            VFS_STATUS_ERR_UNKNOWN_OP,
            VFS_STATUS_ERR_INVALID_PATH,
            VFS_STATUS_ERR_NO_MOUNT,
            VFS_STATUS_ERR_BAD_FD,
            VFS_STATUS_ERR_BACKEND,
            VFS_STATUS_ERR_CODEC,
        ];
        for &status in &codes {
            let encoded = encode_vfs_status(status);
            assert_eq!(
                decode_vfs_status(&encoded),
                Some(status),
                "roundtrip failed for status={status}"
            );
        }
    }

    #[test]
    fn decode_vfs_status_rejects_short_payload() {
        assert_eq!(decode_vfs_status(&[]), None);
        assert_eq!(decode_vfs_status(&[0u8; 1]), None);
        assert_eq!(decode_vfs_status(&[0u8; 3]), None);
        // Exactly 4 bytes succeeds.
        assert_eq!(decode_vfs_status(&[0u8; 4]), Some(0));
    }

    #[test]
    fn decode_vfs_status_reads_first_four_bytes_ignores_trailing() {
        // VFS_STATUS_ERR_BAD_FD=4 in LE, followed by noise.
        let payload = [4u8, 0, 0, 0, 0xFF, 0xFF, 0xFF];
        assert_eq!(decode_vfs_status(&payload), Some(VFS_STATUS_ERR_BAD_FD));
    }

    #[test]
    fn encode_vfs_status_is_little_endian() {
        // VFS_STATUS_ERR_INVALID_PATH = 2 → [0x02, 0x00, 0x00, 0x00]
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_INVALID_PATH), [2, 0, 0, 0]);
        // VFS_STATUS_ERR_NO_MOUNT = 3 → [0x03, 0x00, 0x00, 0x00]
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_NO_MOUNT), [3, 0, 0, 0]);
        // VFS_STATUS_ERR_BAD_FD = 4 → [0x04, 0x00, 0x00, 0x00]
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_BAD_FD), [4, 0, 0, 0]);
    }

    #[test]
    fn vfs_error_path_mapping_invariants() {
        // Path normalization / decode failures → INVALID_PATH (not NO_MOUNT)
        assert_ne!(VFS_STATUS_ERR_INVALID_PATH, VFS_STATUS_ERR_NO_MOUNT);
        // Mount miss → NO_MOUNT (not BAD_FD, the path was valid)
        assert_ne!(VFS_STATUS_ERR_NO_MOUNT, VFS_STATUS_ERR_BAD_FD);
        // Fd miss / wrong client → BAD_FD (not BACKEND)
        assert_ne!(VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_BACKEND);
        // Codec failure on fd-op → CODEC (distinct from BAD_FD)
        assert_ne!(VFS_STATUS_ERR_CODEC, VFS_STATUS_ERR_BAD_FD);
        // All error codes are non-zero (distinct from OK)
        for &code in &[
            VFS_STATUS_ERR_UNKNOWN_OP,
            VFS_STATUS_ERR_INVALID_PATH,
            VFS_STATUS_ERR_NO_MOUNT,
            VFS_STATUS_ERR_BAD_FD,
            VFS_STATUS_ERR_BACKEND,
            VFS_STATUS_ERR_CODEC,
        ] {
            assert_ne!(code, VFS_STATUS_OK, "error code {code} must not equal OK");
        }
    }

    // ── BulkReadArgs / BulkReadReply tests ───────────────────────────────────

    #[test]
    fn bulk_read_args_roundtrip() {
        let args = BulkReadArgs::new(7, 4096, 0);
        let enc = args.encode();
        let dec = BulkReadArgs::decode(&enc).expect("decode");
        assert_eq!(dec.fd, 7);
        assert_eq!(dec.requested_len, 4096);
        assert_eq!(dec.offset, 0);
        // new() must set dst_ptr=0 (wire-compatible with Phase 2A reserved=0).
        assert_eq!(dec.dst_ptr, 0);
    }

    #[test]
    fn bulk_read_args_new_sets_dst_ptr_zero() {
        // Phase 2A compatibility: BulkReadArgs::new() must have dst_ptr=0.
        let args = BulkReadArgs::new(3, 512, 100);
        assert_eq!(args.dst_ptr, 0);
    }

    #[test]
    fn bulk_read_args_new_with_dst_roundtrip() {
        // Phase 2B: new_with_dst encodes the PM transfer buffer VA in dst_ptr.
        let dst = 0xFFFF_0000_1000usize as u64;
        let args = BulkReadArgs::new_with_dst(5, 4096, 8192, dst);
        let enc = args.encode();
        let dec = BulkReadArgs::decode(&enc).expect("decode");
        assert_eq!(dec.fd, 5);
        assert_eq!(dec.requested_len, 4096);
        assert_eq!(dec.offset, 8192);
        assert_eq!(dec.dst_ptr, dst);
    }

    #[test]
    fn bulk_read_args_rejects_oversized_payload() {
        // BulkReadArgs uses VfsV1Args::decode which requires exactly 32 bytes.
        let short = [0u8; 16];
        assert!(BulkReadArgs::decode(&short).is_err());
    }

    #[test]
    fn bulk_read_reply_roundtrip() {
        let r = BulkReadReply {
            copied_len: 4096,
            eof: false,
        };
        let enc = r.encode();
        let dec = BulkReadReply::decode(&enc).expect("decode");
        assert_eq!(dec.copied_len, 4096);
        assert!(!dec.eof);
    }

    #[test]
    fn bulk_read_reply_eof_roundtrip() {
        let r = BulkReadReply {
            copied_len: 512,
            eof: true,
        };
        let enc = r.encode();
        let dec = BulkReadReply::decode(&enc).expect("decode");
        assert_eq!(dec.copied_len, 512);
        assert!(dec.eof);
    }

    #[test]
    fn bulk_read_reply_rejects_short_payload() {
        assert!(BulkReadReply::decode(&[0u8; 11]).is_none());
    }

    #[test]
    fn vfs_op_read_bulk_constant_is_distinct() {
        assert_ne!(VFS_OP_READ_BULK, VFS_OP_READ);
        assert_ne!(VFS_OP_READ_BULK, VFS_OP_OPENAT);
        assert_ne!(VFS_OP_READ_BULK, VFS_OP_CLOSE);
        assert_ne!(VFS_OP_READ_BULK, VFS_OP_STATX);
        assert_ne!(VFS_OP_READ_BULK, VFS_OP_MOUNT_REGISTER);
        assert_eq!(VFS_OP_READ_BULK, 24);
    }

    #[test]
    fn vfs_op_file_grant_ro_constant_is_distinct() {
        assert_eq!(VFS_OP_FILE_GRANT_RO, 25);
        assert_ne!(VFS_OP_FILE_GRANT_RO, VFS_OP_READ_BULK);
        assert_ne!(VFS_OP_FILE_GRANT_RO, VFS_OP_READ);
        assert_ne!(VFS_OP_FILE_GRANT_RO, VFS_OP_OPENAT);
    }

    #[test]
    fn file_grant_ro_args_roundtrip() {
        let args = FileGrantRoArgs::new(42);
        let enc = args.encode();
        let dec = FileGrantRoArgs::decode(&enc).expect("decode");
        assert_eq!(dec.fd, 42);
        assert_eq!(dec.flags, 0);
        assert_eq!(dec.offset, 0);
        assert_eq!(dec.len, 0);
    }

    #[test]
    fn file_grant_ro_reply_roundtrip() {
        let r = FileGrantRoReply {
            file_len: 0xDEAD_BEEF_1234,
            status: 0,
        };
        let enc = r.encode();
        let dec = FileGrantRoReply::decode(&enc).expect("decode");
        assert_eq!(dec.file_len, 0xDEAD_BEEF_1234);
        assert_eq!(dec.status, 0);
    }

    #[test]
    fn file_grant_ro_reply_error_status_roundtrip() {
        let r = FileGrantRoReply {
            file_len: 0,
            status: 5,
        };
        let enc = r.encode();
        let dec = FileGrantRoReply::decode(&enc).expect("decode");
        assert_eq!(dec.file_len, 0);
        assert_eq!(dec.status, 5);
    }

    #[test]
    fn file_grant_ro_reply_rejects_short_payload() {
        assert!(FileGrantRoReply::decode(&[0u8; 11]).is_none());
    }

    #[test]
    fn vfs_status_golden_vectors_for_each_error_path() {
        // Unknown opcode → status=1, LE [01 00 00 00]
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_UNKNOWN_OP), [1, 0, 0, 0]);
        // Invalid path  → status=2
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_INVALID_PATH), [2, 0, 0, 0]);
        // No mount      → status=3
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_NO_MOUNT), [3, 0, 0, 0]);
        // Bad fd        → status=4
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_BAD_FD), [4, 0, 0, 0]);
        // Backend error → status=5
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_BACKEND), [5, 0, 0, 0]);
        // Codec error   → status=6
        assert_eq!(encode_vfs_status(VFS_STATUS_ERR_CODEC), [6, 0, 0, 0]);
    }
    fn read_shared_request() -> VfsReadSharedRequest {
        VfsReadSharedRequest {
            fd: 7,
            file_offset: 0,
            requested_len: 4096,
            request_id: 0x1122_3344,
            flags: VFS_SHARED_IO_F_CURRENT_OFFSET | VFS_SHARED_IO_F_ALLOW_INLINE_FALLBACK,
            buffer: VfsSharedBufferDescriptor::new(9, 3, 8192, 4096, VFS_SHARED_BUFFER_FS_WRITE),
        }
    }

    fn write_shared_request() -> VfsWriteSharedRequest {
        VfsWriteSharedRequest {
            fd: 8,
            file_offset: 64,
            requested_len: 512,
            request_id: 0x5566_7788,
            flags: 0,
            buffer: VfsSharedBufferDescriptor::new(10, 4, 128, 1024, VFS_SHARED_BUFFER_FS_READ),
        }
    }

    #[test]
    fn shared_read_request_and_reply_roundtrip() {
        let request = read_shared_request();
        let encoded = request.encode().expect("encode read shared request");
        assert_eq!(VfsReadSharedRequest::decode(&encoded), Ok(request));

        let reply = VfsReadSharedReply {
            request_id: request.request_id,
            bytes_completed: 2048,
            status: VFS_SHARED_IO_STATUS_OK,
            flags: 0,
        };
        let encoded = reply.encode().expect("encode read shared reply");
        assert_eq!(VfsReadSharedReply::decode(&encoded), Ok(reply));
    }

    #[test]
    fn shared_write_request_and_reply_roundtrip() {
        let request = write_shared_request();
        let encoded = request.encode().expect("encode write shared request");
        assert_eq!(VfsWriteSharedRequest::decode(&encoded), Ok(request));

        let reply = VfsWriteSharedReply {
            request_id: request.request_id,
            bytes_completed: 256,
            status: VFS_SHARED_IO_STATUS_BACKEND,
            flags: 0,
        };
        let encoded = reply.encode().expect("encode write shared reply");
        assert_eq!(VfsWriteSharedReply::decode(&encoded), Ok(reply));
    }

    #[test]
    fn shared_requests_reject_bad_lengths_overflow_flags_and_permissions() {
        let read = read_shared_request();
        let encoded = read.encode().expect("encode");
        assert_eq!(
            VfsReadSharedRequest::decode(&encoded[..encoded.len() - 1]),
            Err(VfsCodecError::Malformed)
        );

        let mut too_long = read;
        too_long.requested_len = too_long.buffer.buffer_len + 1;
        assert_eq!(too_long.encode(), Err(VfsCodecError::InvalidDescriptor));

        let mut overflow = read;
        overflow.buffer.buffer_offset = u64::MAX - 8;
        overflow.buffer.buffer_len = 16;
        overflow.requested_len = 16;
        assert_eq!(overflow.encode(), Err(VfsCodecError::RangeOverflow));

        let mut unknown_flags = read;
        unknown_flags.flags |= 1 << 31;
        assert_eq!(unknown_flags.encode(), Err(VfsCodecError::UnsupportedFlags));

        let mut wrong_permission = read;
        wrong_permission.buffer.access = VFS_SHARED_BUFFER_FS_READ;
        assert_eq!(
            wrong_permission.encode(),
            Err(VfsCodecError::PermissionMismatch)
        );

        let mut write_permission = write_shared_request();
        write_permission.buffer.access = VFS_SHARED_BUFFER_FS_WRITE;
        assert_eq!(
            write_permission.encode(),
            Err(VfsCodecError::PermissionMismatch)
        );
    }

    #[test]
    fn shared_current_offset_and_reserved_bytes_are_strict() {
        let mut request = read_shared_request();
        request.file_offset = 1;
        assert_eq!(request.encode(), Err(VfsCodecError::Malformed));

        let mut encoded = read_shared_request().encode().expect("encode");
        encoded[36] = 1;
        assert_eq!(
            VfsReadSharedRequest::decode(&encoded),
            Err(VfsCodecError::Malformed)
        );

        let reply = VfsWriteSharedReply {
            request_id: 1,
            bytes_completed: 0,
            status: VFS_SHARED_IO_STATUS_UNSUPPORTED,
            flags: 1,
        };
        assert_eq!(reply.encode(), Err(VfsCodecError::UnsupportedFlags));
    }

    #[test]
    fn shared_io_reservations_do_not_change_live_read_write_opcodes() {
        assert_eq!(VFS_OP_READ, 12);
        assert_eq!(VFS_OP_WRITE, 13);
        assert_eq!(VFS_OP_READ_SHARED_REPLY, 26);
        assert_eq!(VFS_OP_WRITE_SHARED_REQUEST, 27);
        assert_eq!(
            VFS_SHARED_IO_CAP_KNOWN,
            VFS_SHARED_IO_CAP_READ_SHARED_REPLY | VFS_SHARED_IO_CAP_WRITE_SHARED_REQUEST
        );

        let legacy = ReadWriteArgs::new(1, 2, 3);
        assert_eq!(ReadWriteArgs::decode(&legacy.encode()), Ok(legacy));
    }
}
