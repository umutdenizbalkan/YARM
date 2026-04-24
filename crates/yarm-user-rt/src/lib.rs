// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

mod arch;

#[macro_export]
macro_rules! user_log {
    ($($arg:tt)*) => {{
        let _ = core::format_args!($($arg)*);
    }};
}

pub mod ipc {
    pub use yarm_ipc_abi::vfs_abi::{
        OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX,
        VFS_OP_WRITE,
    };
    pub use yarm_kernel::ipc::{IpcError, Message, SharedMemoryRegion, ThreadId, TransferCapId};
}

pub mod capability {
    pub use yarm_kernel::capability::{CapId, CapRights};
}

pub mod syscall {
    use crate::ipc::Message;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(usize)]
    pub enum SyscallError {
        InvalidNumber = 1,
        InvalidArgs = 2,
        InvalidCapability = 3,
        MissingRight = 4,
        WrongObject = 5,
        QueueFull = 6,
        WouldBlock = 7,
        PageFault = 8,
        TimedOut = 9,
        Internal = 255,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Error {
        Unsupported,
    }

    pub type Result<T> = core::result::Result<T, Error>;

    const SYSCALL_IPC_SEND_NR: usize = 1;
    const SYSCALL_IPC_RECV_NR: usize = 2;
    const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
    const SYSCALL_RECV_MAP_INTENT_DEFAULT: usize = 0;

    #[inline]
    const fn decode_syscall_error(code: usize) -> SyscallError {
        match code {
            1 => SyscallError::InvalidNumber,
            2 => SyscallError::InvalidArgs,
            3 => SyscallError::InvalidCapability,
            4 => SyscallError::MissingRight,
            5 => SyscallError::WrongObject,
            6 => SyscallError::QueueFull,
            7 => SyscallError::WouldBlock,
            8 => SyscallError::PageFault,
            9 => SyscallError::TimedOut,
            _ => SyscallError::Internal,
        }
    }

    #[inline]
    pub unsafe fn ipc_send(ep_cap: u32, msg: &Message) -> core::result::Result<(), SyscallError> {
        let transfer_cap = msg
            .transferred_cap()
            .map(|cap| cap.0 as usize)
            .unwrap_or(SYSCALL_NO_TRANSFER_CAP as usize);
        let args = [
            ep_cap as usize,
            msg.payload.as_ptr() as usize,
            msg.len as usize,
            0,
            0,
            transfer_cap,
        ];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_SEND_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(())
    }

    #[inline]
    pub unsafe fn ipc_recv(ep_cap: u32) -> core::result::Result<Option<Message>, SyscallError> {
        let mut payload = [0u8; Message::MAX_PAYLOAD];
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            Message::MAX_PAYLOAD,
            0,
            SYSCALL_RECV_MAP_INTENT_DEFAULT,
            SYSCALL_NO_TRANSFER_CAP as usize,
        ];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock) { Ok(None) } else { Err(err) };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret1 == args[1] && ret.ret2 == args[2] {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock) { Ok(None) } else { Err(err) };
        }
        let len = ret.ret1;
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::Internal);
        }
        let transfer_cap = if (ret.ret2 as u64) == SYSCALL_NO_TRANSFER_CAP {
            None
        } else {
            Some(ret.ret2 as u64)
        };
        let msg = Message::with_header(ret.ret0 as u64, 0, 0, transfer_cap, &payload[..len])
            .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(Some(msg))
    }

    #[inline]
    pub fn yield_now() -> Result<()> {
        Err(Error::Unsupported)
    }
}

pub mod runtime {
    use crate::capability::CapId;
    use crate::syscall::SyscallError;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StartupContext {
        pub task_id: u64,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum KernelIpcError {
        MissingRight,
        WouldBlock,
        CapabilityFull,
        EndpointFull,
        EndpointQueueFull,
        TaskTableFull,
        MemoryObjectFull,
        SchedulerFull,
        VmFull,
        InvalidCapability,
        WrongObject,
        StaleCapability,
        UserMemoryFault,
        TaskMissing,
        MemoryObjectMissing,
        VmFault,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrapIpcError {
        Syscall(SyscallError),
        MissingTrapFrame,
    }

    pub trait RuntimeStateAccess<State> {
        fn with_state<R, F>(&self, f: F) -> R
        where
            F: FnOnce(&mut State) -> R;
    }

    pub trait DriverControlOps {
        fn register_driver(&mut self, tid: u64) -> Result<(), KernelIpcError>;
        fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelIpcError>;
        fn grant_driver_irq(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError>;
        fn mint_dma_region_cap(
            &mut self,
            mem_cap: CapId,
            offset: usize,
            len: usize,
        ) -> Result<CapId, KernelIpcError>;
        fn grant_driver_dma(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError>;
        fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelIpcError>;
    }

    #[inline]
    pub fn startup_context() -> StartupContext {
        StartupContext { task_id: 0 }
    }
}

pub mod time {
    use core::ops::{Add, Sub};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct TickInstant(pub u64);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct TickDuration(pub u64);

    impl Add<TickDuration> for TickInstant {
        type Output = TickInstant;

        fn add(self, rhs: TickDuration) -> Self::Output {
            TickInstant(self.0.wrapping_add(rhs.0))
        }
    }

    impl Sub for TickInstant {
        type Output = TickDuration;

        fn sub(self, rhs: Self) -> Self::Output {
            TickDuration(self.0.wrapping_sub(rhs.0))
        }
    }

    impl Add for TickDuration {
        type Output = TickDuration;

        fn add(self, rhs: Self) -> Self::Output {
            TickDuration(self.0.wrapping_add(rhs.0))
        }
    }

    impl TickDuration {
        #[inline]
        pub const fn has_elapsed_since(self, start: TickInstant, now: TickInstant) -> bool {
            now.0.wrapping_sub(start.0) >= self.0
        }
    }
}

pub mod task {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TaskClass {
        App,
        Driver,
        SystemServer,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TaskStatus {
        Runnable,
        Running,
        Blocked,
        Faulted,
        Exited(u64),
        Dead,
    }
}

pub mod vm {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Asid(pub u16);

    pub const PAGE_SIZE: usize = 4096;
}

pub mod process {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ProcessId(pub u64);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProcessError {
        Malformed,
        Unsupported,
        TableFull,
        UnknownProcess,
        InvalidTransport,
        PermissionDenied,
        WouldBlock,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WaitResult {
        pub waited_pid: ProcessId,
        pub exit_code: u64,
    }

    pub trait ProcessManagerOps {
        fn process_id_for_tid(&self, tid: u64) -> ProcessId;
        fn parent_of(&self, pid: ProcessId) -> Option<ProcessId>;
        fn allocate_process(&mut self, parent_pid: ProcessId) -> Result<ProcessId, ProcessError>;
        fn insert_synthetic_exit_for_tid(
            &mut self,
            tid: u64,
            code: u64,
        ) -> Result<(), ProcessError>;
        fn wait_exited(&mut self, pid: ProcessId) -> Result<WaitResult, ProcessError>;
        fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessError>;
    }
}
