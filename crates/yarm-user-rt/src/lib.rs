// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

mod arch;
pub mod recv_v3_draft;

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
        ReadWriteArgs, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE,
    };
    pub use yarm_kernel::ipc::{IpcError, Message, SharedMemoryRegion, ThreadId, TransferCapId};
}

pub mod capability {
    pub use yarm_kernel::capability::{CapId, CapRights};
}

pub mod syscall {
    use crate::ipc::Message;

    pub mod recv_v3;
    pub mod shared_transfer;

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
    /// Control-plane: resize a process cnode (NR 8). Already part of the frozen
    /// syscall ABI; this constant only re-exposes the existing number, it does
    /// not add a syscall.
    pub const SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR: usize = 8;
    const SYSCALL_FUTEX_WAIT_NR: usize = 9;
    const SYSCALL_YIELD_NR: usize = 0;
    /// Stage 163 proof-only: `Fork` (NR 12). Already part of the frozen syscall
    /// ABI; this constant only re-exposes the existing number for the sender-wake
    /// proof's second execution context — it does NOT add a syscall.
    const SYSCALL_FORK_NR: usize = 12;
    pub const SYSCALL_SPAWN_PROCESS_NR: usize = 23;
    pub const SYSCALL_SPAWN_PROCESS_FROM_USER_BUF_NR: usize = 24;
    pub const SYSCALL_SPAWN_FROM_INITRAMFS_FILE_NR: usize = 26;
    /// Phase 2 bulk-copy bridge. TEMPORARY — replace with page-cap in Phase 3.
    pub const SYSCALL_INITRAMFS_READ_CHUNK_NR: usize = 27;
    /// Phase 3A: Create a read-only MemoryObject for a named CPIO file slice.
    pub const SYSCALL_CREATE_INITRAMFS_FILE_SLICE_MO_NR: usize = 28;
    /// Phase 3A: Spawn a process from a MemoryObject capability.
    pub const SYSCALL_SPAWN_FROM_MEMORY_OBJECT_NR: usize = 29;
    const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
    const SYSCALL_RECV_MAP_INTENT_DEFAULT: usize = 0;
    const SYSCALL_RECV_META_REPLY_CAP: usize = 1 << 0;
    const SYSCALL_RECV_META_TRANSFERRED_CAP: usize = 1 << 1;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ReceivedMessage {
        pub message: Message,
        pub reply_cap: Option<u32>,
        pub transferred_cap: Option<u32>,
        pub sender_tid: u64,
    }
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct IpcRecvMetaV2 {
        status: u64,
        opcode: u16,
        flags: u16,
        payload_len: u32,
        cap_id: u64,
        recv_meta_flags: u64,
        sender_tid: u64,
    }
    const _: () = assert!(core::mem::size_of::<IpcRecvMetaV2>() == 40);

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
        ) -> core::result::Result<Option<ReceivedMessage>, SyscallError> {
            match self.recv(ep_cap) {
                Ok(Some(msg)) => Ok(Some(ReceivedMessage {
                    message: msg,
                    reply_cap: None,
                    transferred_cap: msg.transferred_cap().map(|c| c.0 as u32),
                    sender_tid: msg.sender_tid.0,
                })),
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
        ) -> core::result::Result<Option<ReceivedMessage>, SyscallError> {
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
                    self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
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
        let (frame, frame_len, _tx_cap, _msg_flags) = ipc_call_prepare(msg);
        let transfer_cap = msg
            .transferred_cap()
            .map(|cap| cap.0 as usize)
            .unwrap_or(SYSCALL_NO_TRANSFER_CAP as usize);
        let args = [
            ep_cap as usize,
            frame.as_ptr() as usize,
            frame_len,
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

    /// Stage 163 proof-only: timed/blocking `IpcSend`. Identical to `ipc_send`
    /// except it sets the send-timeout field (`SYSCALL_ARG_INLINE_PAYLOAD1`, arg
    /// slot 4) to `timeout_ticks`, which the kernel routes through
    /// `ipc_send_with_deadline`. On a FULL endpoint the sender then becomes a real
    /// blocked sender-waiter (instead of the non-blocking `ipc_send`'s immediate
    /// `WouldBlock`), and the send completes successfully once a receiver drain
    /// frees a slot and refills this sender's message. Reuses the existing
    /// `IpcSend` syscall + ABI — no syscall number or IPC ABI change. Returns the
    /// kernel result (`Ok` on woken-and-delivered; `TimedOut` if the deadline
    /// elapses first).
    #[inline]
    pub unsafe fn ipc_send_timeout_ticks(
        ep_cap: u32,
        msg: &Message,
        timeout_ticks: u64,
    ) -> core::result::Result<(), SyscallError> {
        let (frame, frame_len, _tx_cap, _msg_flags) = ipc_call_prepare(msg);
        let transfer_cap = msg
            .transferred_cap()
            .map(|cap| cap.0 as usize)
            .unwrap_or(SYSCALL_NO_TRANSFER_CAP as usize);
        let args = [
            ep_cap as usize,
            frame.as_ptr() as usize,
            frame_len,
            0,
            timeout_ticks as usize,
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

    /// Stage 163 proof-only: `Fork` (NR 12). Returns the new child's TID to the
    /// parent and `0` to the child (POSIX-style; the kernel sets the child's
    /// return register to 0 via `child.user_context.arg0 = 0`). The child inherits
    /// a COW copy of the parent address space and its ordinary userspace IPC caps,
    /// so it can reuse the parent's proof endpoint caps with no manual stack/TLS
    /// bootstrap. Reuses the existing `Fork` syscall — no ABI change.
    ///
    /// Returns `None` on a fork failure that the ABI can flag (x86_64 sets the
    /// separate error lane), `Some(0)` in the child, and `Some(child_tid)` in the
    /// parent. On AArch64/riscv64 there is no separate error lane, so a failure
    /// there cannot be distinguished from a child TID and is returned as
    /// `Some(value)`; the proof-only caller must therefore still tolerate a missing
    /// child (bounded coordination poll, never an unbounded wait). Reuses the
    /// existing `Fork` syscall — no ABI change.
    #[inline]
    pub unsafe fn fork() -> Option<u64> {
        let args = [0usize; 6];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_FORK_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return None;
        }
        Some(ret.ret0 as u64)
    }

    #[inline]
    pub unsafe fn ipc_recv(ep_cap: u32) -> core::result::Result<Option<Message>, SyscallError> {
        match unsafe { ipc_recv_v2(ep_cap) }? {
            Some(received) => Ok(Some(received.message)),
            None => Ok(None),
        }
    }

    #[inline]
    pub unsafe fn ipc_recv_v2(
        ep_cap: u32,
    ) -> core::result::Result<Option<ReceivedMessage>, SyscallError> {
        // Receive buffer is sized for legacy inline-request framing cases where the
        // kernel may strip a request prefix before exposing payload via out-meta.
        // Userspace decodes payload/opcode exclusively from IpcRecvMetaV2.
        const FRAMED_MAX: usize = 2 + Message::MAX_PAYLOAD;
        let mut payload = [0u8; FRAMED_MAX];
        let mut meta = IpcRecvMetaV2 {
            status: u64::MAX,
            opcode: 0,
            flags: 0,
            payload_len: 0,
            cap_id: SYSCALL_NO_TRANSFER_CAP,
            recv_meta_flags: 0,
            sender_tid: 0,
        };
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            FRAMED_MAX,
            (&mut meta as *mut IpcRecvMetaV2) as usize,
            core::mem::size_of::<IpcRecvMetaV2>(),
            SYSCALL_RECV_MAP_INTENT_DEFAULT,
        ];
        // SAFETY: Uses architecture syscall ABI to enter kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock) {
                Ok(None)
            } else {
                Err(err)
            };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 && meta.status == u64::MAX {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock) {
                Ok(None)
            } else {
                Err(err)
            };
        }
        let payload_len = meta.payload_len as usize;
        if payload_len > Message::MAX_PAYLOAD || payload_len > FRAMED_MAX {
            return Err(SyscallError::Internal);
        }
        let opcode = meta.opcode;
        let msg_payload = &payload[..payload_len];
        let data_len = msg_payload.len();
        let returned_cap = if meta.cap_id == SYSCALL_NO_TRANSFER_CAP {
            None
        } else {
            Some(meta.cap_id as u32)
        };
        let preview_len = core::cmp::min(data_len, 32);
        let _ = preview_len;
        let recv_meta_flags = meta.recv_meta_flags as usize;
        let reply_cap = if (recv_meta_flags & SYSCALL_RECV_META_REPLY_CAP) != 0 {
            returned_cap
        } else {
            None
        };
        let transferred_cap = if (recv_meta_flags & SYSCALL_RECV_META_TRANSFERRED_CAP) != 0 {
            returned_cap
        } else {
            None
        };
        let flags = if transferred_cap.is_some() {
            Message::FLAG_CAP_TRANSFER
        } else {
            0
        };
        let msg = Message::with_header(
            meta.sender_tid,
            opcode,
            flags,
            transferred_cap.map(|c| c as u64),
            msg_payload,
        )
        .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(Some(ReceivedMessage {
            message: msg,
            reply_cap,
            transferred_cap,
            sender_tid: meta.sender_tid,
        }))
    }

    /// Stage 159BC/D proof-only recv-v2 with a deliberately undersized payload
    /// buffer.
    ///
    /// This is used ONLY by the default-off `yarm.ipc_recv_proof` oracle
    /// workload to deterministically drive the kernel queued-split rollback
    /// path: when a queued cap-bearing message is drained, the kernel
    /// materializes the carried capability and THEN discovers the receiver's
    /// payload buffer is too small (`RecvV2WritebackOutcome::PayloadUndersized`),
    /// rolling the freshly-minted cap back (`IPC_RECV_V2_ROLLBACK_OK
    /// site=queued_split_undersize`). The meta pointer is fully valid — only the
    /// payload region is intentionally too small — so the fault is a clean,
    /// deterministic undersize, not an unmapped-address guess.
    ///
    /// Returns `Ok(())` if the recv unexpectedly succeeded (no rollback), or the
    /// `SyscallError` the kernel returned for the undersized writeback
    /// (`InvalidArgs` on the undersize path). NOT for production IPC.
    #[inline]
    pub unsafe fn ipc_recv_v2_proof_undersized(
        ep_cap: u32,
    ) -> core::result::Result<(), SyscallError> {
        // Deliberately tiny payload buffer: any queued message with a payload
        // larger than this drives the undersize rollback path.
        let mut payload = [0u8; 8];
        let mut meta = IpcRecvMetaV2 {
            status: u64::MAX,
            opcode: 0,
            flags: 0,
            payload_len: 0,
            cap_id: SYSCALL_NO_TRANSFER_CAP,
            recv_meta_flags: 0,
            sender_tid: 0,
        };
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            payload.len(),
            (&mut meta as *mut IpcRecvMetaV2) as usize,
            core::mem::size_of::<IpcRecvMetaV2>(),
            SYSCALL_RECV_MAP_INTENT_DEFAULT,
        ];
        // SAFETY: Uses architecture syscall ABI to enter kernel. The payload
        // pointer is valid and writable; only its length is deliberately small.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        // AArch64/riscv64 return the syscall result via x0 (ret0); there is no
        // separate error lane. The general `ipc_recv_v2` distinguishes a delivered
        // message from WouldBlock with the `meta.status == u64::MAX` heuristic, but
        // that heuristic is INVALID for the undersize rollback: the recv-v2
        // writeback copies the meta FIRST (status = sender_tid) and only THEN
        // detects the undersized payload and rolls back, so `meta.status` is no
        // longer `u64::MAX` even though the syscall failed. The kernel encodes the
        // failure (InvalidArgs) into x0 via `set_err` + the Stage 160C export, so
        // for this proof-only undersize recv a non-zero x0 IS the error (a
        // successful recv-v2 sets x0 = 0). Detect it directly from x0.
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(())
    }

    #[inline]
    pub unsafe fn ipc_recv_with_deadline(
        ep_cap: u32,
        timeout_ticks: u64,
    ) -> core::result::Result<Option<Message>, SyscallError> {
        let mut payload = [0u8; 2 + Message::MAX_PAYLOAD];
        let mut meta = IpcRecvMetaV2 {
            status: u64::MAX,
            opcode: 0,
            flags: 0,
            payload_len: 0,
            cap_id: SYSCALL_NO_TRANSFER_CAP,
            recv_meta_flags: 0,
            sender_tid: 0,
        };
        let args = [
            ep_cap as usize,
            payload.as_mut_ptr() as usize,
            payload.len(),
            timeout_ticks as usize,
            (&mut meta as *mut IpcRecvMetaV2) as usize,
            core::mem::size_of::<IpcRecvMetaV2>(),
        ];
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
        if ret.ret0 != 0 && meta.status == u64::MAX {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) {
                Ok(None)
            } else {
                Err(err)
            };
        }
        let len = meta.payload_len as usize;
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::Internal);
        }
        let returned_cap = if meta.cap_id == SYSCALL_NO_TRANSFER_CAP {
            None
        } else {
            Some(meta.cap_id)
        };
        let recv_meta_flags = meta.recv_meta_flags as usize;
        let transfer_cap = if (recv_meta_flags & SYSCALL_RECV_META_TRANSFERRED_CAP) != 0 {
            returned_cap
        } else {
            None
        };
        let flags = if transfer_cap.is_some() {
            Message::FLAG_CAP_TRANSFER
        } else {
            0
        };
        let msg = Message::with_header(
            meta.sender_tid,
            meta.opcode,
            flags,
            transfer_cap,
            &payload[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(Some(msg))
    }

    #[inline]
    pub(crate) fn ipc_call_prepare(
        msg: &Message,
    ) -> ([u8; 2 + Message::MAX_PAYLOAD], usize, Option<u32>, u16) {
        let payload_len = msg.len as usize;
        let frame_len = 2 + payload_len;
        let mut frame = [0u8; 2 + Message::MAX_PAYLOAD];
        frame[0..2].copy_from_slice(&msg.opcode.to_le_bytes());
        frame[2..frame_len].copy_from_slice(&msg.payload[..payload_len]);
        (
            frame,
            frame_len,
            msg.transferred_cap().map(|c| c.0 as u32),
            msg.flags,
        )
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
        let (frame, frame_len, _tx_cap, _msg_flags) = ipc_call_prepare(msg);
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
            image_id as usize,              // arg0 = image_id
            elf_ptr as usize,               // arg1 = elf_user_ptr
            elf_len,                        // arg2 = elf_len
            parent_pid as usize,            // arg3 = parent_pid
            startup_args.as_ptr() as usize, // arg4 = startup_args_ptr
            startup_args.len(),             // arg5 = startup_args_count
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
            image_id as usize,              // arg0 = image_id
            name.as_ptr() as usize,         // arg1 = name_ptr
            name.len(),                     // arg2 = name_len
            parent_pid as usize,            // arg3 = parent_pid
            startup_args.as_ptr() as usize, // arg4 = startup_args_ptr
            startup_args.len(),             // arg5 = startup_args_count
        ];
        #[cfg(all(target_arch = "aarch64", not(test)))]
        {
            let lr: usize;
            let sp: usize;
            let fp: usize;
            let saved_lr: usize;
            unsafe {
                core::arch::asm!(
                    "mov {lr}, x30",
                    "mov {sp}, sp",
                    "mov {fp}, x29",
                    "ldr {slr}, [x29, #8]",
                    lr = out(reg) lr,
                    sp = out(reg) sp,
                    fp = out(reg) fp,
                    slr = out(reg) saved_lr,
                    options(nostack, readonly),
                );
            }
            crate::user_log!(
                "SPAWN26_RTLIB_STACK_BEFORE sp=0x{:x} fp=0x{:x} lr=0x{:x} saved_lr=0x{:x}",
                sp,
                fp,
                lr,
                saved_lr
            );
        }
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_FROM_INITRAMFS_FILE_NR, args) };
        #[cfg(all(target_arch = "aarch64", not(test)))]
        {
            let lr: usize;
            let sp: usize;
            let fp: usize;
            let saved_lr: usize;
            unsafe {
                core::arch::asm!(
                    "mov {lr}, x30",
                    "mov {sp}, sp",
                    "mov {fp}, x29",
                    "ldr {slr}, [x29, #8]",
                    lr = out(reg) lr,
                    sp = out(reg) sp,
                    fp = out(reg) fp,
                    slr = out(reg) saved_lr,
                    options(nostack, readonly),
                );
            }
            crate::user_log!(
                "SPAWN26_RTLIB_STACK_AFTER sp=0x{:x} fp=0x{:x} lr=0x{:x} saved_lr=0x{:x}",
                sp,
                fp,
                lr,
                saved_lr
            );
            crate::user_log!(
                "AARCH64_SYSCALL26_RETURN x0={} x1={} x2={} x3={} x4={} x5={}",
                ret.ret0,
                ret.ret1,
                ret.ret2,
                ret.ret3,
                ret.ret4,
                ret.ret5
            );
        }
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

    /// Read up to 4096 bytes from a named file in the boot initramfs CPIO at
    /// the given byte offset into `dst`.
    ///
    /// Returns the number of bytes actually copied (may be less than `dst.len()`
    /// at end-of-file).  Returns `Ok(0)` at EOF.
    /// Returns `Err` for not_found or access_denied — callers MUST NOT treat these as EOF.
    ///
    /// # Phase 2A bulk-copy bridge
    /// Calls kernel syscall nr=27 (arg5=0 = self-ASID). PM-only/privileged.
    /// Phase 2B adds VFS-mediated routing on top. Phase 3 uses page-cap zero-copy.
    ///
    /// # Safety
    /// `dst` must be a valid writable slice in the caller's address space for
    /// the duration of the syscall.
    #[inline]
    pub unsafe fn initramfs_read_chunk(
        name: &[u8],
        offset: u64,
        dst: &mut [u8],
    ) -> core::result::Result<usize, SyscallError> {
        let max_len = core::cmp::min(dst.len(), 4096);
        let args = [
            name.as_ptr() as usize,    // arg0 = name_ptr
            name.len(),                // arg1 = name_len
            offset as usize,           // arg2 = offset
            dst.as_mut_ptr() as usize, // arg3 = dst_ptr (self-ASID)
            max_len,                   // arg4 = max_len
            0,                         // arg5 = target_tid (0 = self)
        ];
        // SAFETY: Uses architecture syscall ABI; name and dst lifetimes cover the call.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_INITRAMFS_READ_CHUNK_NR, args) };
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

    /// Phase 2B bridge: copy up to 4096 bytes from a named CPIO file into PM's
    /// transfer buffer at `pm_dst_ptr` (PM's virtual address).
    ///
    /// Used by `initramfs_srv` to fill PM's 4 KiB transfer buffer as part of the
    /// VFS-mediated bulk read path.  Only callable by `TaskClass::SystemServer`.
    ///
    /// TEMPORARY — replace with page-cap grant in Phase 3.
    ///
    /// # Safety
    /// `name` must be a valid byte slice in the caller's address space.
    /// `pm_dst_ptr` must be a valid writable VA in PM's address space.
    #[inline]
    pub unsafe fn initramfs_write_to_pm_buf(
        name: &[u8],
        offset: u64,
        pm_dst_ptr: usize,
        max_len: usize,
    ) -> core::result::Result<usize, SyscallError> {
        // PM_BOOTSTRAP_TID = 3 (hardcoded temporary bridge; replace with page-cap in Phase 3).
        const PM_BOOTSTRAP_TID: usize = 3;
        let clamped_len = core::cmp::min(max_len, 4096);
        let args = [
            name.as_ptr() as usize, // arg0 = name_ptr
            name.len(),             // arg1 = name_len
            offset as usize,        // arg2 = offset
            pm_dst_ptr,             // arg3 = dst_ptr (PM's VA)
            clamped_len,            // arg4 = max_len
            PM_BOOTSTRAP_TID,       // arg5 = target_tid = PM_TID (Phase 2B bridge)
        ];
        // SAFETY: name lifetime covers the call; pm_dst_ptr validated by kernel.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_INITRAMFS_READ_CHUNK_NR, args) };
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

    /// Phase 3A: Create a read-only MemoryObject backed by a named CPIO file slice.
    ///
    /// Only callable by SystemServer tasks (initramfs_srv).
    ///
    /// Returns `(cap_id, file_len)` on success where `cap_id` is the new MemoryObject
    /// capability and `file_len` is the exact file data length.
    ///
    /// # Safety
    /// `name` must be a valid byte slice in the caller's address space.
    #[inline]
    pub unsafe fn create_initramfs_file_slice_mo(
        name: &[u8],
        flags: u64,
    ) -> core::result::Result<(u32, u64), SyscallError> {
        let args = [
            name.as_ptr() as usize, // arg0 = name_ptr
            name.len(),             // arg1 = name_len
            flags as usize,         // arg2 = flags (reserved, must be 0)
            0,
            0,
            0,
        ];
        // SAFETY: Uses architecture syscall ABI; name lifetime covers the call.
        let ret =
            unsafe { crate::arch::raw_syscall(SYSCALL_CREATE_INITRAMFS_FILE_SLICE_MO_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        // ret1 = cap_id (u32), ret2 = file_len (usize)
        Ok((ret.ret1 as u32, ret.ret2 as u64))
    }

    /// Phase 3A: Spawn a process from an InitramfsFileSlice MemoryObject capability.
    ///
    /// Only callable by PM (TID=3).
    ///
    /// Returns `(child_tid, caller_cap, spawner_cap)` on success, same layout as
    /// `spawn_process_from_user_buf`.
    ///
    /// # Safety
    /// `startup_args` must be a valid array in the caller's address space.
    #[inline]
    pub unsafe fn spawn_from_memory_object(
        image_id: u64,
        mo_cap: u32,
        parent_pid: u64,
        startup_args: &[u64; 18],
    ) -> core::result::Result<(u64, u32, u32), SyscallError> {
        let args = [
            image_id as usize,              // arg0 = image_id
            mo_cap as usize,                // arg1 = mo_cap
            parent_pid as usize,            // arg2 = parent_pid
            startup_args.as_ptr() as usize, // arg3 = startup_args_ptr
            startup_args.len(),             // arg4 = startup_args_count
            0,
        ];
        // SAFETY: Uses architecture syscall ABI; startup_args lifetime covers the call.
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_SPAWN_FROM_MEMORY_OBJECT_NR, args) };
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

    #[inline]
    pub fn futex_wait(
        addr: *const u32,
        expected: u32,
        observed: u32,
    ) -> core::result::Result<bool, SyscallError> {
        // SAFETY: Uses architecture syscall ABI to enter kernel. The kernel validates
        // that `addr` names a readable current-user futex word before blocking.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_FUTEX_WAIT_NR,
                [addr as usize, expected as usize, observed as usize, 0, 0, 0],
            )
        };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 > 1 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(ret.ret0 != 0)
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

    /// Resize a process cnode via the control-plane syscall (NR 8).
    ///
    /// This is the userspace stub for the already-frozen `ControlPlaneSetCnodeSlots`
    /// syscall: `arg0 = target_pid`, `arg1 = slot_capacity`. It enters the kernel
    /// through the normal architecture syscall trap, exactly as any other syscall,
    /// so on x86_64 (-smp 1) it flows through `handle_trap_entry_shared` →
    /// `try_split_dispatch_into_frame` (the NR-8 split-dispatch seam).
    #[inline]
    pub fn control_plane_set_cnode_slots(
        target_pid: u64,
        slot_capacity: usize,
    ) -> core::result::Result<usize, SyscallError> {
        // SAFETY: Uses architecture syscall ABI to enter kernel. The kernel
        // validates requester rights and target pid before mutating the cnode.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR,
                [target_pid as usize, slot_capacity, 0, 0, 0, 0],
            )
        };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        // ret0 == resized slot capacity on success (the kernel writes
        // set_ok(slot_capacity, target_pid, 0)).
        Ok(ret.ret0)
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
    pub const STARTUP_SLOT_INITRD_PTR: usize = 15;
    pub const STARTUP_SLOT_INITRD_LEN: usize = 16;
    pub const STARTUP_SLOT_PM_REQUEST_RECV_CAP: usize = 17;
    const STARTUP_SLOT_COUNT: usize = 18;

    static STARTUP_ARG_SLOTS: [AtomicU64; STARTUP_SLOT_COUNT] = [
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
        /// Boot initramfs pointer mapped read-only into this process's address space (slot 15).
        /// Zero/None if kernel did not provide an initrd mapping.
        pub initrd_ptr: Option<u64>,
        /// Boot initramfs length in bytes (slot 16).
        /// Zero/None if kernel did not provide an initrd mapping.
        pub initrd_len: Option<u64>,
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
            slots[0],
            slots[1],
            slots[2],
            startup_slots_len
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
    pub fn startup_arg_slot(index: usize) -> Option<u64> {
        if index >= STARTUP_SLOT_COUNT {
            return None;
        }
        Some(STARTUP_ARG_SLOTS[index].load(Ordering::Relaxed))
    }

    #[inline]
    pub fn startup_context() -> StartupContext {
        // Reads runtime-provided startup ABI slots. Zero/missing values map to
        // `None` for optional endpoint caps.
        let task_id = STARTUP_ARG_SLOTS[STARTUP_SLOT_TASK_ID].load(Ordering::Relaxed);
        let process_manager_request_send_cap = cap_from_slot(
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_REQUEST_SEND_CAP]
                .load(Ordering::Relaxed),
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
            STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_RESTART_CONTROL_SEND_CAP]
                .load(Ordering::Relaxed),
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
        let initrd_ptr = {
            let raw = STARTUP_ARG_SLOTS[STARTUP_SLOT_INITRD_PTR].load(Ordering::Relaxed);
            if raw == 0 { None } else { Some(raw) }
        };
        let initrd_len = {
            let raw = STARTUP_ARG_SLOTS[STARTUP_SLOT_INITRD_LEN].load(Ordering::Relaxed);
            if raw == 0 { None } else { Some(raw) }
        };
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
            initrd_ptr,
            initrd_len,
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
    use crate::ipc::Message;
    use crate::runtime::{install_startup_arg_slots, startup_context};
    use crate::syscall::ipc_call_prepare;

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
            original
                .supervisor_fault_recv_ep
                .map(u64::from)
                .unwrap_or(0),
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
            original
                .process_manager_restart_control_send_cap
                .map(|v| v as u64)
                .unwrap_or(0),
            original
                .process_manager_service_recv_ep
                .map(u64::from)
                .unwrap_or(0),
            0,
            0,
            0,
            0,
            original.pm_request_recv_cap.map(u64::from).unwrap_or(0),
        ]);
    }

    #[test]
    fn ipc_call_prepare_prefixes_opcode_and_payload() {
        let payload = [0xAA, 0xBB, 0xCC];
        let msg = Message::with_header(0, 0x1234, 0, None, &payload).unwrap();
        let (frame, frame_len, tx_cap, flags) = ipc_call_prepare(&msg);
        assert_eq!(frame_len, 5);
        assert_eq!(frame[0], 0x34);
        assert_eq!(frame[1], 0x12);
        assert_eq!(&frame[2..5], &payload);
        assert_eq!(tx_cap, None);
        assert_eq!(flags, 0);
    }

    #[test]
    fn ipc_call_prepare_preserves_cap_transfer_fields() {
        let msg =
            Message::with_header(0, 7, Message::FLAG_CAP_TRANSFER, Some(65551), &[1, 2]).unwrap();
        let (_frame, _len, tx_cap, flags) = ipc_call_prepare(&msg);
        assert_eq!(tx_cap, Some(65551));
        assert_ne!(flags & Message::FLAG_CAP_TRANSFER, 0);
    }

    #[test]
    fn startup_initrd_slots_decode_from_slots_15_16() {
        let original = startup_context();

        install_startup_arg_slots([
            42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC001000, // slot 15 = initrd_ptr
            4096,      // slot 16 = initrd_len
            0,
        ]);
        let ctx = startup_context();
        assert_eq!(ctx.initrd_ptr, Some(0xC001000u64));
        assert_eq!(ctx.initrd_len, Some(4096u64));

        install_startup_arg_slots([42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let ctx2 = startup_context();
        assert_eq!(ctx2.initrd_ptr, None);
        assert_eq!(ctx2.initrd_len, None);

        // Restore original
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
            original
                .supervisor_fault_recv_ep
                .map(u64::from)
                .unwrap_or(0),
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
            original
                .process_manager_restart_control_send_cap
                .map(|v| v as u64)
                .unwrap_or(0),
            original
                .process_manager_service_recv_ep
                .map(u64::from)
                .unwrap_or(0),
            0,
            0,
            original.initrd_ptr.unwrap_or(0),
            original.initrd_len.unwrap_or(0),
            original.pm_request_recv_cap.map(u64::from).unwrap_or(0),
        ]);
    }
}
