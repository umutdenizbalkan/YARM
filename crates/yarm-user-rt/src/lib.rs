// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[macro_export]
macro_rules! user_log {
    ($($arg:tt)*) => {{
        let _ = core::format_args!($($arg)*);
    }};
}

pub mod ipc {
    pub use yarm_kernel::ipc::{IpcError, Message, SharedMemoryRegion, ThreadId, TransferCapId};
    pub use yarm_ipc_abi::vfs_abi::{
        OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX,
        VFS_OP_WRITE,
    };
}

pub mod syscall {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Error {
        Unsupported,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    #[inline]
    pub fn yield_now() -> Result<()> {
        Err(Error::Unsupported)
    }
}

pub mod runtime {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StartupContext {
        pub task_id: u64,
    }

    #[inline]
    pub fn startup_context() -> StartupContext {
        StartupContext { task_id: 0 }
    }
}
