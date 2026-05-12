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
        ReadWriteArgs, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX,
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

    const SYSCALL_IPC_SEND_NR: usize = 1;
    const SYSCALL_IPC_RECV_NR: usize = 2;
    const SYSCALL_IPC_RECV_TIMEOUT_NR: usize = 5;
    const SYSCALL_IPC_CALL_NR: usize = 6;
    const SYSCALL_IPC_REPLY_NR: usize = 7;
    const SYSCALL_YIELD_NR: usize = 0;
    pub const SYSCALL_SPAWN_PROCESS_NR: usize = 23;
    const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
    const SYSCALL_RECV_MAP_INTENT_DEFAULT: usize = 0;

    pub trait IpcTransport {
        fn send(&mut self, ep_cap: u32, msg: &Message) -> core::result::Result<(), SyscallError>;
        fn recv(&mut self, ep_cap: u32) -> core::result::Result<Option<Message>, SyscallError>;
        fn recv_with_deadline(
            &mut self,
            ep_cap: u32,
            timeout_ticks: u64,
        ) -> core::result::Result<Option<Message>, SyscallError>;
        fn recv_v2(
            &mut self,
            ep_cap: u32,
        ) -> core::result::Result<Option<(Message, Option<u32>)>, SyscallError> {
            match self.recv(ep_cap) {
                Ok(Some(msg)) => {
                    let reply_cap = msg.transferred_cap().map(|c| c.0 as u32);
                    Ok(Some((msg, reply_cap)))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn ipc_reply_v2(
            &mut self,
            reply_cap: u32,
            msg: &Message,
        ) -> core::result::Result<(), SyscallError> {
            unsafe { ipc_reply(reply_cap, msg) }
        }
    }

    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct SyscallIpcTransport;

    impl IpcTransport for SyscallIpcTransport {
        #[inline]
        fn send(&mut self, ep_cap: u32, msg: &Message) -> core::result::Result<(), SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_send(ep_cap, msg) }
        }

        #[inline]
        fn recv(&mut self, ep_cap: u32) -> core::result::Result<Option<Message>, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_recv(ep_cap) }
        }

        #[inline]
        fn recv_with_deadline(
            &mut self,
            ep_cap: u32,
            timeout_ticks: u64,
        ) -> core::result::Result<Option<Message>, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_recv_with_deadline(ep_cap, timeout_ticks) }
        }
    }

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
    pub unsafe fn ipc_recv_with_deadline(
        ep_cap: u32,
        timeout_ticks: u64,
    ) -> core::result::Result<Option<Message>, SyscallError> {
        let mut payload = [0u8; Message::MAX_PAYLOAD];
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            Message::MAX_PAYLOAD,
            timeout_ticks as usize,
            SYSCALL_RECV_MAP_INTENT_DEFAULT,
            SYSCALL_NO_TRANSFER_CAP as usize,
        ];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_TIMEOUT_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) {
                Ok(None)
            } else {
                Err(err)
            };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret1 == args[1] && ret.ret2 == args[2] {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) {
                Ok(None)
            } else {
                Err(err)
            };
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
    pub unsafe fn ipc_call(
        ep_cap: u32,
        reply_recv_cap: u32,
        msg: &Message,
    ) -> core::result::Result<(), SyscallError> {
        let args = [
            ep_cap as usize,
            msg.payload.as_ptr() as usize,
            msg.len as usize,
            0,
            0,
            reply_recv_cap as usize,
        ];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_CALL_NR, args) };
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
    pub unsafe fn spawn_process(
        image_id: u64,
        parent_pid: u64,
    ) -> core::result::Result<u64, SyscallError> {
        let args = [image_id as usize, parent_pid as usize, 0, 0, 0, 0];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_PROCESS_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(ret.ret1 as u64)
    }

    #[inline]
    pub unsafe fn ipc_reply(
        reply_cap: u32,
        msg: &Message,
    ) -> core::result::Result<(), SyscallError> {
        let transfer_cap = msg
            .transferred_cap()
            .map(|cap| cap.0 as usize)
            .unwrap_or(SYSCALL_NO_TRANSFER_CAP as usize);
        let args = [
            reply_cap as usize,
            msg.payload.as_ptr() as usize,
            msg.len as usize,
            0,
            0,
            transfer_cap,
        ];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_REPLY_NR, args) };
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
    pub fn yield_now() -> core::result::Result<(), SyscallError> {
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_YIELD_NR, [0; 6]) };
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
}

pub mod runtime {
    use crate::capability::CapId;
    use crate::syscall::SyscallError;
    use core::sync::atomic::{AtomicU64, Ordering};

    pub const STARTUP_SLOT_TASK_ID: usize = 0;
    pub const STARTUP_SLOT_PROCESS_MANAGER_REQUEST_SEND_CAP: usize = 1;
    pub const STARTUP_SLOT_PROCESS_MANAGER_REPLY_RECV_CAP: usize = 2;
    pub const STARTUP_SLOT_SUPERVISOR_FAULT_RECV_EP: usize = 3;
    pub const STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP: usize = 4;
    pub const STARTUP_SLOT_SUPERVISOR_CONTROL_RECV_EP: usize = 5;
    pub const STARTUP_SLOT_INIT_ALERT_SEND_EP: usize = 6;
    pub const STARTUP_SLOT_INIT_ALERT_RECV_EP: usize = 7;
    pub const STARTUP_SLOT_OPTIONAL_INIT_TID: usize = 8;
    pub const STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID: usize = 9;
    pub const STARTUP_SLOT_SUPERVISOR_RESTART_WINDOW_TICKS: usize = 10;
    pub const STARTUP_SLOT_PROCESS_MANAGER_RESTART_CONTROL_SEND_CAP: usize = 11;
    pub const STARTUP_SLOT_PROCESS_MANAGER_SERVICE_RECV_EP: usize = 12;
    pub const STARTUP_SLOT_PM_REQUEST_RECV_CAP: usize = 17;
    const STARTUP_SLOT_COUNT: usize = 18;

    static STARTUP_ARG_SLOTS: [AtomicU64; STARTUP_SLOT_COUNT] =
        [
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StartupContext {
        pub task_id: u64,
        /// Optional process-manager request endpoint send capability.
        ///
        /// This remains `None` until the startup ABI wires concrete cap slots.
        pub process_manager_request_send_cap: Option<u32>,
        /// Optional process-manager reply endpoint receive capability.
        ///
        /// This remains `None` until the startup ABI wires concrete cap slots.
        pub process_manager_reply_recv_cap: Option<u32>,
        /// Optional supervisor fault receive endpoint cap.
        pub supervisor_fault_recv_ep: Option<u32>,
        /// Optional supervisor control send endpoint cap.
        pub supervisor_control_send_ep: Option<u32>,
        /// Optional supervisor control receive endpoint cap.
        pub supervisor_control_recv_ep: Option<u32>,
        /// Optional init alert send endpoint cap.
        pub init_alert_send_ep: Option<u32>,
        /// Optional init alert receive endpoint cap.
        pub init_alert_recv_ep: Option<u32>,
        /// Optional init task id conveyed during runtime handoff.
        pub init_tid: Option<u64>,
        /// Optional supervisor task id conveyed during runtime handoff.
        pub supervisor_tid: Option<u64>,
        /// Optional supervisor restart window ticks.
        pub supervisor_restart_window_ticks: Option<u64>,
        /// Optional process-manager restart-control SEND cap.
        pub process_manager_restart_control_send_cap: Option<u32>,
        /// Optional process-manager service receive endpoint cap.
        ///
        /// Passed to process_manager (TID 2) so it knows which endpoint to recv on.
        pub process_manager_service_recv_ep: Option<u32>,
        /// Optional process-manager inbound request receive cap (slot 17).
        ///
        /// Passed to the PM server (TID 3) so it knows which endpoint to block on.
        pub pm_request_recv_cap: Option<u32>,
    }

    impl StartupContext {
        #[inline]
        pub const fn process_manager_caps(self) -> Option<(u32, u32)> {
            match (
                self.process_manager_request_send_cap,
                self.process_manager_reply_recv_cap,
            ) {
                (Some(request_send), Some(reply_recv)) => Some((request_send, reply_recv)),
                _ => None,
            }
        }
    }

    #[inline]
    const fn cap_from_slot(raw: u64) -> Option<u32> {
        if raw == 0 || raw > (u32::MAX as u64) {
            None
        } else {
            Some(raw as u32)
        }
    }

    #[inline]
    const fn optional_tid_from_slot(raw: u64) -> Option<u64> {
        if raw == 0 { None } else { Some(raw) }
    }

    /// Install raw startup ABI slot values captured by runtime entry code.
    ///
    /// Slot mapping:
    /// - `STARTUP_SLOT_TASK_ID`
    /// - `STARTUP_SLOT_PROCESS_MANAGER_REQUEST_SEND_CAP`
    /// - `STARTUP_SLOT_PROCESS_MANAGER_REPLY_RECV_CAP`
    /// - `STARTUP_SLOT_SUPERVISOR_FAULT_RECV_EP`
    /// - `STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP`
    /// - `STARTUP_SLOT_SUPERVISOR_CONTROL_RECV_EP`
    /// - `STARTUP_SLOT_INIT_ALERT_SEND_EP`
    /// - `STARTUP_SLOT_INIT_ALERT_RECV_EP`
    /// - `STARTUP_SLOT_OPTIONAL_INIT_TID`
    /// - `STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID`
    /// - `STARTUP_SLOT_SUPERVISOR_RESTART_WINDOW_TICKS`
    /// - `STARTUP_SLOT_PROCESS_MANAGER_RESTART_CONTROL_SEND_CAP`
    ///
    /// Missing/unset slots should be provided as `0`.
    #[inline]
    pub fn install_startup_arg_slots(slots: [u64; STARTUP_SLOT_COUNT]) {
        let mut index = 0usize;
        while index < STARTUP_SLOT_COUNT {
            STARTUP_ARG_SLOTS[index].store(slots[index], Ordering::Relaxed);
            index += 1;
        }
    }

    #[inline]
    fn install_startup_args_from_abi(
        startup_task_id: u64,
        startup_proc_mgr_request_send_cap: u64,
        startup_proc_mgr_reply_recv_cap: u64,
        startup_slots_ptr: usize,
        startup_slots_len: usize,
    ) {
        let mut slots = [
            startup_task_id,
            startup_proc_mgr_request_send_cap,
            startup_proc_mgr_reply_recv_cap,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        if startup_slots_ptr != 0 && startup_slots_len >= slots.len() {
            let src = startup_slots_ptr as *const u64;
            let mut index = 0usize;
            while index < slots.len() {
                // SAFETY: bounded by `slots.len()` and guarded by non-zero pointer
                // + contract length check above.
                slots[index] = unsafe { core::ptr::read(src.add(index)) };
                index += 1;
            }
        }
        install_startup_arg_slots(slots);
    }

    #[inline(never)]
    pub fn enter_user_entrypoint(
        startup_task_id: u64,
        startup_proc_mgr_request_send_cap: u64,
        startup_proc_mgr_reply_recv_cap: u64,
        startup_slots_ptr: usize,
        startup_slots_len: usize,
        user_entry: extern "C" fn() -> !,
    ) -> ! {
        install_startup_args_from_abi(
            startup_task_id,
            startup_proc_mgr_request_send_cap,
            startup_proc_mgr_reply_recv_cap,
            startup_slots_ptr,
            startup_slots_len,
        );
        // SAFETY: reading a function pointer from its local storage is valid.
        let entry = unsafe { core::ptr::read_volatile(&user_entry) };
        entry()
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
        // Reads runtime-provided startup ABI slots. Zero/missing values map to
        // `None` for optional endpoint caps.
        let task_id = STARTUP_ARG_SLOTS[STARTUP_SLOT_TASK_ID].load(Ordering::Relaxed);
        let process_manager_request_send_cap = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_REQUEST_SEND_CAP].load(Ordering::Relaxed),
        );
        let process_manager_reply_recv_cap = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_REPLY_RECV_CAP].load(Ordering::Relaxed),
        );
        let supervisor_fault_recv_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SUPERVISOR_FAULT_RECV_EP].load(Ordering::Relaxed),
        );
        let supervisor_control_send_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP].load(Ordering::Relaxed),
        );
        let supervisor_control_recv_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SUPERVISOR_CONTROL_RECV_EP].load(Ordering::Relaxed),
        );
        let init_alert_send_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_INIT_ALERT_SEND_EP].load(Ordering::Relaxed),
        );
        let init_alert_recv_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_INIT_ALERT_RECV_EP].load(Ordering::Relaxed),
        );
        let init_tid = optional_tid_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_OPTIONAL_INIT_TID].load(Ordering::Relaxed),
        );
        let supervisor_tid = optional_tid_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_OPTIONAL_SUPERVISOR_TID].load(Ordering::Relaxed),
        );
        let supervisor_restart_window_ticks = optional_tid_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SUPERVISOR_RESTART_WINDOW_TICKS].load(Ordering::Relaxed),
        );
        let process_manager_restart_control_send_cap = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_RESTART_CONTROL_SEND_CAP].load(Ordering::Relaxed),
        );
        let process_manager_service_recv_ep = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_SERVICE_RECV_EP].load(Ordering::Relaxed),
        );
        let pm_request_recv_cap = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PM_REQUEST_RECV_CAP].load(Ordering::Relaxed),
        );
        StartupContext {
            task_id,
            process_manager_request_send_cap,
            process_manager_reply_recv_cap,
            supervisor_fault_recv_ep,
            supervisor_control_send_ep,
            supervisor_control_recv_ep,
            init_alert_send_ep,
            init_alert_recv_ep,
            init_tid,
            supervisor_tid,
            supervisor_restart_window_ticks,
            process_manager_restart_control_send_cap,
            process_manager_service_recv_ep,
            pm_request_recv_cap,
        }
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

#[cfg(test)]
mod tests {
    use crate::runtime::{install_startup_arg_slots, startup_context};

    #[test]
    fn startup_process_manager_caps_require_both_slots() {
        let original = startup_context();

        install_startup_arg_slots([42, 11, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), Some((11, 12)));

        install_startup_arg_slots([42, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), None);

        install_startup_arg_slots([42, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), None);

        install_startup_arg_slots([
            original.task_id,
            original
                .process_manager_request_send_cap
                .map(u64::from)
                .unwrap_or(0),
            original
                .process_manager_reply_recv_cap
                .map(u64::from)
                .unwrap_or(0),
            original.supervisor_fault_recv_ep.map(u64::from).unwrap_or(0),
            original
                .supervisor_control_send_ep
                .map(u64::from)
                .unwrap_or(0),
            original
                .supervisor_control_recv_ep
                .map(u64::from)
                .unwrap_or(0),
            original.init_alert_send_ep.map(u64::from).unwrap_or(0),
            original.init_alert_recv_ep.map(u64::from).unwrap_or(0),
            original.init_tid.unwrap_or(0),
            original.supervisor_tid.unwrap_or(0),
            original.supervisor_restart_window_ticks.unwrap_or(0),
            original.process_manager_restart_control_send_cap.map(|v| v as u64).unwrap_or(0),
            original.process_manager_service_recv_ep.map(u64::from).unwrap_or(0),
            0,
            0,
            0,
            0,
            original.pm_request_recv_cap.map(u64::from).unwrap_or(0),
        ]);
    }
}
