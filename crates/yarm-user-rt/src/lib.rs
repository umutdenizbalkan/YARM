// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

mod arch;

#[macro_export]
macro_rules! user_log {
    ($($arg:tt)*) => {{
        $crate::__user_log_emit(core::format_args!($($arg)*));
    }};
}

#[doc(hidden)]
pub fn __user_log_emit(args: core::fmt::Arguments<'_>) {
    syscall::debug_log(args);
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
    pub const SYSCALL_SPAWN_PROCESS_FROM_USER_BUF_NR: usize = 24;
    pub const SYSCALL_READ_INITRAMFS_FILE_NR: usize = 25;
    pub const SYSCALL_SPAWN_FROM_INITRAMFS_FILE_NR: usize = 26;
    const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
    const SYSCALL_RECV_MAP_INTENT_DEFAULT: usize = 0;

    #[used]
    #[unsafe(link_section = ".rodata")]
    static USER_RT_RECV_BEFORE_SYSCALL_MARKER: [u8; 28] = *b"USER_RT_RECV_BEFORE_SYSCALL\0";

    #[used]
    #[unsafe(link_section = ".rodata")]
    static USER_RT_RECV_AFTER_SYSCALL_MARKER: [u8; 27] = *b"USER_RT_RECV_AFTER_SYSCALL\0";


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

        #[inline]
        fn recv_v2(
            &mut self,
            ep_cap: u32,
        ) -> core::result::Result<Option<(Message, Option<u32>)>, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_recv_v2(ep_cap) }
        }
    }

    #[inline]
    pub fn debug_log(args: core::fmt::Arguments<'_>) {
        #[cfg(target_os = "none")]
        {
            const SYSCALL_DEBUG_LOG_NR: usize = 15;
            const MAX_LOG_LEN: usize = 128;
            struct LogBuf {
                buf: [u8; MAX_LOG_LEN],
                len: usize,
            }
            impl core::fmt::Write for LogBuf {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    let bytes = s.as_bytes();
                    let available = MAX_LOG_LEN - self.len;
                    let to_copy = bytes.len().min(available);
                    self.buf[self.len..self.len + to_copy]
                        .copy_from_slice(&bytes[..to_copy]);
                    self.len += to_copy;
                    Ok(())
                }
            }
            let mut buf = LogBuf {
                buf: [0u8; MAX_LOG_LEN],
                len: 0,
            };
            let _ = core::fmt::write(&mut buf, args);
            if buf.len > 0 {
                let sysargs = [buf.buf.as_ptr() as usize, buf.len, 0, 0, 0, 0];
                // SAFETY: kernel validates ptr/len from current user task's memory.
                unsafe { crate::arch::raw_syscall(SYSCALL_DEBUG_LOG_NR, sysargs) };
            }
        }
        #[cfg(not(target_os = "none"))]
        {
            let _ = args;
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
        match unsafe { ipc_recv_v2(ep_cap) }? {
            Some((msg, _reply_cap)) => Ok(Some(msg)),
            None => Ok(None),
        }
    }

    #[inline]
    pub unsafe fn ipc_recv_v2(
        ep_cap: u32,
    ) -> core::result::Result<Option<(Message, Option<u32>)>, SyscallError> {
        // Buffer is 2 bytes larger than MAX_PAYLOAD to accommodate the opcode prefix
        // that ipc_call prepends. The kernel receives [opcode_lo, opcode_hi, ...data].
        const FRAMED_MAX: usize = 2 + Message::MAX_PAYLOAD;
        let mut payload = [0u8; FRAMED_MAX];
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            FRAMED_MAX,
            0,
            SYSCALL_RECV_MAP_INTENT_DEFAULT,
            SYSCALL_NO_TRANSFER_CAP as usize,
        ];
        crate::user_log!(
            "USER_RT_RECV_BEFORE_SYSCALL cap={} buf=0x{:x} len={}",
            args[0],
            args[1],
            args[2]
        );
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_NR, args) };
        crate::user_log!(
            "USER_RT_RECV_RAW ret0={} ret1={} ret2={} buf_ptr=0x{:x} buf_len={}",
            ret.ret0,
            ret.ret1,
            ret.ret2,
            args[1],
            args[2]
        );
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock) { Ok(None) } else { Err(err) };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        {
            // x0 in 1..=9 is an error code when x1==0 && x2==0 (set_err zeroes all ret
            // fields; export_syscall_result_to_user_gprs then sets x0=code, x1=0, x2=0).
            // Also detect the pre-retry stale case: x1==buf_ptr && x2==FRAMED_MAX.
            let is_exported_error = ret.ret1 == 0 && ret.ret2 == 0 && (1..=9).contains(&ret.ret0);
            let is_stale_blocking = ret.ret1 == args[1] && ret.ret2 == args[2] && (1..=9).contains(&ret.ret0);
            if is_exported_error || is_stale_blocking {
                let err = decode_syscall_error(ret.ret0);
                return if matches!(err, SyscallError::WouldBlock) { Ok(None) } else { Err(err) };
            }
        }
        let len = ret.ret1;
        // Must have at least the 2-byte opcode prefix.
        if len < 2 || len > FRAMED_MAX {
            return Err(SyscallError::Internal);
        }
        // Extract application-level opcode from the first 2 bytes of the frame.
        let opcode = u16::from_le_bytes([payload[0], payload[1]]);
        let data_len = len - 2;
        let reply_cap = if ret.ret2 == SYSCALL_NO_TRANSFER_CAP as usize {
            None
        } else {
            Some(ret.ret2 as u32)
        };
        crate::user_log!(
            "USER_RT_RECV_DECODE_OK status={} len={} opcode={} payload_len={} reply_cap={}",
            ret.ret0,
            len,
            opcode,
            data_len,
            reply_cap.map(|c| c as u64).unwrap_or(SYSCALL_NO_TRANSFER_CAP)
        );
        let msg = Message::with_header(ret.ret0 as u64, opcode, 0, None, &payload[2..len])
            .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(Some((msg, reply_cap)))
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
        crate::user_log!(
            "USER_RT_RECV_BEFORE_SYSCALL cap={} buf=0x{:x} len={}",
            args[0],
            args[1],
            args[2]
        );
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_TIMEOUT_NR, args) };
        crate::user_log!(
            "USER_RT_RECV_AFTER_SYSCALL x0={} x1={} x2={} x3={}",
            ret.ret0,
            ret.ret1,
            ret.ret2,
            args[3]
        );
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
        if ret.ret1 == args[1] && ret.ret2 == args[2] && (1..=9).contains(&ret.ret0) {
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
        let flags = if transfer_cap.is_some() {
            Message::FLAG_CAP_TRANSFER
        } else {
            0
        };
        crate::user_log!(
            "USER_RT_RECV_DECODE status={} len={} reply_cap={} ok={}",
            ret.ret0,
            len,
            transfer_cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP),
            true
        );
        let msg = Message::with_header(ret.ret0 as u64, 0, flags, transfer_cap, &payload[..len])
            .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(Some(msg))
    }

    #[inline]
    pub unsafe fn ipc_call(
        ep_cap: u32,
        reply_recv_cap: u32,
        msg: &Message,
    ) -> core::result::Result<(), SyscallError> {
        // Prepend the 2-byte opcode (LE) before the payload bytes so the receiver
        // can reconstruct the application-level opcode. The kernel ABI only passes
        // raw bytes; msg.opcode is not carried in return registers.
        let payload_len = msg.len as usize;
        let frame_len = 2 + payload_len;
        let mut frame = [0u8; 2 + Message::MAX_PAYLOAD];
        frame[0..2].copy_from_slice(&msg.opcode.to_le_bytes());
        frame[2..frame_len].copy_from_slice(&msg.payload[..payload_len]);
        let args = [
            ep_cap as usize,
            frame.as_ptr() as usize,
            frame_len,
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
        if ret.ret1 == args[1] && ret.ret2 == args[2] {
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
    pub unsafe fn spawn_process_with_startup_caps(
        image_id: u64,
        parent_pid: u64,
        startup_args: &[u64; 18],
    ) -> core::result::Result<(u64, u32, u32), SyscallError> {
        let args = [
            image_id as usize,
            parent_pid as usize,
            startup_args.as_ptr() as usize,
            18usize,
            0,
            0,
        ];
        // SAFETY: Uses architecture syscall ABI; startup_args lifetime covers the call.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_PROCESS_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        // ret2 may pack two 32-bit cap IDs: spawner's own cap in the high 32 bits
        // and the parent-delegated cap in the low 32 bits (set by the kernel when
        // parent_pid != 0 and delegation occurred).
        let caller_cap = (ret.ret2 & 0xFFFF_FFFF) as u32;
        let spawner_cap = (ret.ret2 >> 32) as u32;
        Ok((ret.ret1 as u64, caller_cap, spawner_cap))
    }

    /// Spawn a new process from an ELF image already loaded into a userspace buffer.
    ///
    /// The kernel copies the ELF bytes from the caller's address space into an
    /// internal staging buffer before parsing and loading them, so the caller's
    /// buffer may be reused immediately after this call returns.
    ///
    /// Returns `(child_tid, caller_cap, spawner_cap)` on success.
    #[inline]
    pub unsafe fn spawn_process_from_user_buf(
        image_id: u64,
        elf_ptr: *const u8,
        elf_len: usize,
        parent_pid: u64,
        startup_args: &[u64; 18],
    ) -> core::result::Result<(u64, u32, u32), SyscallError> {
        let args = [
            image_id as usize,                // arg0 = image_id
            elf_ptr as usize,                 // arg1 = elf_user_ptr
            elf_len,                          // arg2 = elf_len
            parent_pid as usize,             // arg3 = parent_pid
            startup_args.as_ptr() as usize,  // arg4 = startup_args_ptr
            startup_args.len(),              // arg5 = startup_args_count
        ];
        // SAFETY: Uses architecture syscall ABI; elf_ptr lifetime covers the call.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_PROCESS_FROM_USER_BUF_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        let caller_cap = (ret.ret2 & 0xFFFF_FFFF) as u32;
        let spawner_cap = (ret.ret2 >> 32) as u32;
        Ok((ret.ret1 as u64, caller_cap, spawner_cap))
    }

    /// Read bytes from a named file inside the boot initramfs CPIO.
    ///
    /// `name` is the CPIO entry name (e.g. `b"sbin/initramfs_srv"`).
    /// `offset` is the byte offset within the file.
    /// `out_buf` receives the bytes; returns the number of bytes actually copied.
    /// Returns 0 when `offset >= file_size` (EOF).
    pub unsafe fn read_initramfs_file(
        name: &[u8],
        offset: usize,
        out_buf: &mut [u8],
    ) -> core::result::Result<usize, SyscallError> {
        if name.is_empty() || out_buf.is_empty() {
            return Ok(0);
        }
        let args = [
            name.as_ptr() as usize,     // arg0 = name_ptr
            name.len(),                  // arg1 = name_len
            offset,                      // arg2 = file_offset
            out_buf.as_mut_ptr() as usize, // arg3 = out_ptr
            out_buf.len(),               // arg4 = out_len
            0,
        ];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_READ_INITRAMFS_FILE_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(ret.ret1)
    }

    /// Spawn a process directly from a named file inside the boot initramfs CPIO.
    ///
    /// The kernel reads the ELF into an internal staging buffer (no user-space
    /// buffer required) and spawns the process, returning `(tid, caller_cap, spawner_cap)`.
    pub unsafe fn spawn_from_initramfs_file(
        image_id: u64,
        name: &[u8],
        parent_pid: u64,
        startup_args: &[u64; 18],
    ) -> core::result::Result<(u64, u32, u32), SyscallError> {
        if name.is_empty() {
            return Err(SyscallError::InvalidArgs);
        }
        let args = [
            image_id as usize,               // arg0 = image_id
            name.as_ptr() as usize,          // arg1 = name_ptr
            name.len(),                      // arg2 = name_len
            parent_pid as usize,             // arg3 = parent_pid
            startup_args.as_ptr() as usize,  // arg4 = startup_args_ptr
            startup_args.len(),              // arg5 = startup_args_count
        ];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_FROM_INITRAMFS_FILE_NR, args) };
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        let caller_cap = (ret.ret2 & 0xFFFF_FFFF) as u32;
        let spawner_cap = (ret.ret2 >> 32) as u32;
        Ok((ret.ret1 as u64, caller_cap, spawner_cap))
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
    pub const STARTUP_SLOT_SERVICE_EXTRA_CAP_0: usize = 13;
    pub const STARTUP_SLOT_SERVICE_EXTRA_CAP_1: usize = 14;
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
        /// Optional extra service send cap 0 (slot 13).
        pub service_extra_cap_0: Option<u32>,
        /// Optional extra service send cap 1 (slot 14).
        pub service_extra_cap_1: Option<u32>,
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
            // Slots 0-2 are authoritatively provided via registers (x0/x1/x2).
            // If the slot block left them as zero (e.g. not yet committed to user
            // virtual memory), restore the register-supplied values so the PM caps
            // are never silently lost.
            if slots[0] == 0 {
                slots[0] = startup_task_id;
            }
            if slots[1] == 0 {
                slots[1] = startup_proc_mgr_request_send_cap;
            }
            if slots[2] == 0 {
                slots[2] = startup_proc_mgr_reply_recv_cap;
            }
        }
        user_log!(
            "STARTUP_INSTALL_FINAL task_id={} pm_send={} pm_reply={} slots_len={}",
            slots[0], slots[1], slots[2], startup_slots_len
        );
        install_startup_arg_slots(slots);
    }

    // AArch64 bare-metal only: a non-inlined extern "C" shim so the naked asm
    // can reach install_startup_args_from_abi via a stable bl target symbol.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    #[inline(never)]
    extern "C" fn aarch64_install_shim(a: u64, b: u64, c: u64, d: usize, e: usize) {
        install_startup_args_from_abi(a, b, c, d, e);
    }

    // AArch64 bare-metal: naked function that saves the user-entry fn ptr in the
    // callee-saved register x19 before calling the install shim.
    //
    // Broken pattern avoided:
    //   stp x30, x5, [sp, #-16]!       ← saves fn ptr on stack
    //   bl  install_startup_args_from_abi
    //   ldr x8, [sp, #8]               ← restores into x8 (syscall-number scratch!)
    //   blr x8
    //
    // Correct pattern used here:
    //   mov x19, x5   ← fn ptr in callee-saved x19; install preserves x19 (ABI)
    //   bl  {install}
    //   br  x19       ← branch to fn ptr; x19 guaranteed valid after bl returns
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    #[unsafe(naked)]
    pub extern "C" fn enter_user_entrypoint(
        _startup_task_id: u64,
        _startup_proc_mgr_request_send_cap: u64,
        _startup_proc_mgr_reply_recv_cap: u64,
        _startup_slots_ptr: usize,
        _startup_slots_len: usize,
        _user_entry: extern "C" fn() -> !,
    ) -> ! {
        // x0-x4 = startup args (passed straight through to install shim)
        // x5    = user_entry fn ptr
        core::arch::naked_asm!(
            "mov x19, x5",      // save fn ptr; install preserves callee-saved x19
            "bl {install}",     // install_startup_args_from_abi(x0..x4)
            "br x19",           // jump to user entry — never returns
            install = sym aarch64_install_shim,
        )
    }

    // All other targets (hosted-dev on any arch, or bare-metal non-AArch64).
    #[cfg(not(all(not(feature = "hosted-dev"), target_arch = "aarch64")))]
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
        let service_extra_cap_0 = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SERVICE_EXTRA_CAP_0].load(Ordering::Relaxed),
        );
        let service_extra_cap_1 = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_SERVICE_EXTRA_CAP_1].load(Ordering::Relaxed),
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
            service_extra_cap_0,
            service_extra_cap_1,
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

pub mod vfs_client;

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
