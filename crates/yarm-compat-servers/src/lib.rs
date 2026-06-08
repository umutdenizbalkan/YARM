// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[macro_export]
macro_rules! yarm_log {
    ($($arg:tt)*) => {{
        yarm_server_runtime::user_rt::user_log!($($arg)*);
    }};
}

#[cfg(feature = "posix-compat")]
pub const LINUX_COMPAT_ABI_VERSION: u16 = 1;
#[cfg(feature = "posix-compat")]
pub const LINUX_COMPAT_SYSCALL_COUNT: usize = 23;
#[cfg(feature = "posix-compat")]
pub const POSIX_COMPAT_ABI_VERSION: u16 = LINUX_COMPAT_ABI_VERSION;
#[cfg(feature = "posix-compat")]
pub const POSIX_COMPAT_SYSCALL_COUNT: usize = LINUX_COMPAT_SYSCALL_COUNT;

#[cfg(feature = "posix-compat")]
pub const LINUX_NR_BRK: usize = 214;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_MUNMAP: usize = 215;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_MMAP: usize = 222;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_MPROTECT: usize = 226;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_GETPID: usize = 172;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_EXIT: usize = 93;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_GETPPID: usize = 173;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_OPENAT: usize = 56;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_CLOSE: usize = 57;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_READ: usize = 63;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_WRITE: usize = 64;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_IOCTL: usize = 29;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_DUP: usize = 23;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_FCNTL: usize = 25;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_POLL: usize = 73;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_EPOLL_CREATE1: usize = 20;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_EPOLL_CTL: usize = 21;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_EPOLL_PWAIT: usize = 22;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_SENDFILE: usize = 71;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_STATX: usize = 291;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_SOCKET: usize = 198;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_CONNECT: usize = 203;
#[cfg(feature = "posix-compat")]
pub const LINUX_NR_SENDTO: usize = 206;

#[cfg(feature = "posix-compat")]
pub const EINVAL: i32 = 22;
#[cfg(feature = "posix-compat")]
pub const EPERM: i32 = 1;
#[cfg(feature = "posix-compat")]
pub const EINTR: i32 = 4;
#[cfg(feature = "posix-compat")]
pub const EAGAIN: i32 = 11;
#[cfg(feature = "posix-compat")]
pub const ENOMEM: i32 = 12;
#[cfg(feature = "posix-compat")]
pub const ETIMEDOUT: i32 = 110;
#[cfg(feature = "posix-compat")]
pub const ENOSYS: i32 = 38;

#[cfg(feature = "posix-compat")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosixErrno {
    Inval,
    Perm,
    Intr,
    Again,
    NoMem,
    TimedOut,
    NoSys,
}

#[cfg(feature = "posix-compat")]
impl PosixErrno {
    pub const fn code(self) -> i32 {
        match self {
            Self::Inval => EINVAL,
            Self::Perm => EPERM,
            Self::Intr => EINTR,
            Self::Again => EAGAIN,
            Self::NoMem => ENOMEM,
            Self::TimedOut => ETIMEDOUT,
            Self::NoSys => ENOSYS,
        }
    }

    pub const fn neg_code(self) -> isize {
        -(self.code() as isize)
    }

    pub const fn from_raw_errno(errno: i32) -> Self {
        match errno {
            EINVAL => Self::Inval,
            EPERM => Self::Perm,
            EINTR => Self::Intr,
            EAGAIN => Self::Again,
            ENOMEM => Self::NoMem,
            ETIMEDOUT => Self::TimedOut,
            ENOSYS => Self::NoSys,
            _ => Self::Inval,
        }
    }
}

#[cfg(feature = "posix-compat")]
impl From<yarm_user_rt::syscall::SyscallError> for PosixErrno {
    fn from(value: yarm_user_rt::syscall::SyscallError) -> Self {
        match value {
            yarm_user_rt::syscall::SyscallError::MissingRight => Self::Perm,
            yarm_user_rt::syscall::SyscallError::WouldBlock => Self::Intr,
            yarm_user_rt::syscall::SyscallError::QueueFull
            | yarm_user_rt::syscall::SyscallError::Internal => Self::NoMem,
            yarm_user_rt::syscall::SyscallError::TimedOut => Self::TimedOut,
            yarm_user_rt::syscall::SyscallError::InvalidNumber
            | yarm_user_rt::syscall::SyscallError::InvalidArgs
            | yarm_user_rt::syscall::SyscallError::InvalidCapability
            | yarm_user_rt::syscall::SyscallError::WrongObject
            | yarm_user_rt::syscall::SyscallError::PageFault => Self::Inval,
        }
    }
}

#[cfg(feature = "posix-compat")]
pub mod posix_compat {
    #[path = "sysdeps.rs"]
    pub mod sysdeps;

    #[path = "service.rs"]
    mod service;

    pub fn run() {
        service::run();
    }
}

#[cfg(feature = "posix-compat")]
pub mod yarm_compat_servers {
    pub use crate::posix_compat::sysdeps;
    pub use crate::{
        EAGAIN, EINTR, EINVAL, ENOMEM, ENOSYS, EPERM, ETIMEDOUT, LINUX_COMPAT_ABI_VERSION,
        LINUX_COMPAT_SYSCALL_COUNT, LINUX_NR_BRK, LINUX_NR_CLOSE, LINUX_NR_CONNECT, LINUX_NR_DUP,
        LINUX_NR_EPOLL_CREATE1, LINUX_NR_EPOLL_CTL, LINUX_NR_EPOLL_PWAIT, LINUX_NR_EXIT,
        LINUX_NR_FCNTL, LINUX_NR_GETPID, LINUX_NR_GETPPID, LINUX_NR_IOCTL, LINUX_NR_MMAP,
        LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LINUX_NR_OPENAT, LINUX_NR_POLL, LINUX_NR_READ,
        LINUX_NR_SENDFILE, LINUX_NR_SENDTO, LINUX_NR_SOCKET, LINUX_NR_STATX, LINUX_NR_WRITE,
        POSIX_COMPAT_ABI_VERSION, POSIX_COMPAT_SYSCALL_COUNT, PosixErrno,
    };
}

#[cfg(feature = "posix-compat")]
pub fn run_posix_compat_server() {
    posix_compat::run();
}

#[cfg(test)]
mod tests {
    use yarm_server_runtime::ipc_abi::process_abi::PROC_OP_GETPID;
    const PROC_GETPID_REPLY_REQUIRED_BYTES: usize = 8;

    fn decode_getpid_reply(opcode: u16, payload: &[u8]) -> Result<u64, ()> {
        if opcode != PROC_OP_GETPID || payload.len() < PROC_GETPID_REPLY_REQUIRED_BYTES {
            return Err(());
        }
        let mut pid_bytes = [0u8; PROC_GETPID_REPLY_REQUIRED_BYTES];
        pid_bytes.copy_from_slice(&payload[..PROC_GETPID_REPLY_REQUIRED_BYTES]);
        Ok(u64::from_le_bytes(pid_bytes))
    }

    #[test]
    fn getpid_ipc_rejects_malformed_reply_payload() {
        assert_eq!(decode_getpid_reply(PROC_OP_GETPID, &[1, 2, 3]), Err(()));
    }
}
