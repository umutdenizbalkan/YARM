// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::vfs_abi::{
    VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL, VFS_OP_EPOLL_PWAIT,
    VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ, VFS_OP_SENDFILE,
    VFS_OP_STATX, VFS_OP_WRITE,
};

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

impl VfsReply {
    pub fn from_opcode_payload(opcode: u16, payload: &[u8]) -> Option<Self> {
        if payload.len() < 8 {
            return None;
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&payload[..8]);
        let value = u64::from_le_bytes(raw);
        match opcode {
            VFS_OP_OPENAT => Some(Self::OpenAtFd(value)),
            VFS_OP_CLOSE => Some(Self::CloseResult(value)),
            VFS_OP_READ => Some(Self::ReadLen(value)),
            VFS_OP_WRITE => Some(Self::WriteLen(value)),
            VFS_OP_STATX => Some(Self::StatxValue(value)),
            VFS_OP_IOCTL => Some(Self::IoctlResult(value)),
            VFS_OP_DUP => Some(Self::DupFd(value)),
            VFS_OP_FCNTL => Some(Self::FcntlResult(value)),
            VFS_OP_POLL => Some(Self::PollEvents(value)),
            VFS_OP_EPOLL_CREATE1 => Some(Self::EpollFd(value)),
            VFS_OP_EPOLL_CTL => Some(Self::EpollCtlResult(value)),
            VFS_OP_EPOLL_PWAIT => Some(Self::EpollWaitEvents(value)),
            VFS_OP_SENDFILE => Some(Self::SendfileLen(value)),
            _ => None,
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
}
