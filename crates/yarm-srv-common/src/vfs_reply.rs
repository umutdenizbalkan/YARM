// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::vfs_abi::{
    VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT,
    VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_SENDFILE,
    VFS_OP_STATX, VFS_OP_WRITE,
};

use crate::decode::{DecodeError, decode_u64_le};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsReply {
    OpenAtFd(u64),
    CloseResult(u64),
    ReadLen(u64),
    WriteLen(u64),
    StatxValue(u64),
    IoctlResult(u64),
    DupFd(u64),
    FcntlResult(u64),
    PollEvents(u64),
    EpollFd(u64),
    EpollCtlResult(u64),
    EpollWaitEvents(u64),
    SendfileLen(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsReplyDecodeError {
    Payload(DecodeError),
    UnsupportedOpcode { opcode: u16 },
    UnexpectedReplyKind { opcode: u16 },
}

impl VfsReply {
    pub fn from_opcode_payload_checked(
        opcode: u16,
        payload: &[u8],
    ) -> Result<Self, VfsReplyDecodeError> {
        let value = decode_u64_le(payload).map_err(VfsReplyDecodeError::Payload)?;
        match opcode {
            VFS_OP_OPENAT => Ok(Self::OpenAtFd(value)),
            VFS_OP_CLOSE => Ok(Self::CloseResult(value)),
            VFS_OP_READ => Ok(Self::ReadLen(value)),
            VFS_OP_WRITE => Ok(Self::WriteLen(value)),
            VFS_OP_STATX => Ok(Self::StatxValue(value)),
            VFS_OP_IOCTL => Ok(Self::IoctlResult(value)),
            VFS_OP_DUP => Ok(Self::DupFd(value)),
            VFS_OP_FCNTL => Ok(Self::FcntlResult(value)),
            VFS_OP_POLL => Ok(Self::PollEvents(value)),
            VFS_OP_EPOLL_CREATE1 => Ok(Self::EpollFd(value)),
            VFS_OP_EPOLL_CTL => Ok(Self::EpollCtlResult(value)),
            VFS_OP_EPOLL_PWAIT => Ok(Self::EpollWaitEvents(value)),
            VFS_OP_SENDFILE => Ok(Self::SendfileLen(value)),
            _ => Err(VfsReplyDecodeError::UnsupportedOpcode { opcode }),
        }
    }

    pub const fn as_u64(self) -> u64 {
        match self {
            Self::OpenAtFd(v)
            | Self::CloseResult(v)
            | Self::ReadLen(v)
            | Self::WriteLen(v)
            | Self::StatxValue(v)
            | Self::IoctlResult(v)
            | Self::DupFd(v)
            | Self::FcntlResult(v)
            | Self::PollEvents(v)
            | Self::EpollFd(v)
            | Self::EpollCtlResult(v)
            | Self::EpollWaitEvents(v)
            | Self::SendfileLen(v) => v,
        }
    }

    pub const fn as_fd(self) -> Option<u64> {
        match self {
            Self::OpenAtFd(fd) | Self::DupFd(fd) | Self::EpollFd(fd) => Some(fd),
            _ => None,
        }
    }

    pub const fn expect_fd(self, opcode: u16) -> Result<u64, VfsReplyDecodeError> {
        match self {
            Self::OpenAtFd(fd) | Self::DupFd(fd) | Self::EpollFd(fd) => Ok(fd),
            _ => Err(VfsReplyDecodeError::UnexpectedReplyKind { opcode }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::vfs_abi::{VFS_OP_CLOSE, VFS_OP_OPENAT};

    #[test]
    fn checked_decode_rejects_short_payloads() {
        assert_eq!(
            VfsReply::from_opcode_payload_checked(VFS_OP_OPENAT, &[1, 2, 3]),
            Err(VfsReplyDecodeError::Payload(DecodeError::PayloadTooShort {
                expected: 8,
                actual: 3
            }))
        );
    }

    #[test]
    fn checked_decode_rejects_unsupported_opcodes() {
        let payload = 12u64.to_le_bytes();
        assert_eq!(
            VfsReply::from_opcode_payload_checked(0xFFFF, &payload),
            Err(VfsReplyDecodeError::UnsupportedOpcode { opcode: 0xFFFF })
        );
    }

    #[test]
    fn expect_fd_rejects_non_fd_reply_variants() {
        let payload = 0u64.to_le_bytes();
        let close = VfsReply::from_opcode_payload_checked(VFS_OP_CLOSE, &payload).expect("decode");
        assert_eq!(
            close.expect_fd(VFS_OP_CLOSE),
            Err(VfsReplyDecodeError::UnexpectedReplyKind {
                opcode: VFS_OP_CLOSE
            })
        );
    }
}
