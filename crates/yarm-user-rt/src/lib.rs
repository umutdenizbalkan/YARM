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

#[inline]
pub fn serial_marker_line(message: &str) {
    let bytes = message.as_bytes();
    let mut line = [0u8; 257];
    let len = core::cmp::min(bytes.len(), 256);
    line[..len].copy_from_slice(&bytes[..len]);
    line[len] = b'\n';
    crate::arch::serial_write_bytes(&line[..len + 1]);
}

pub mod capability {
    pub use yarm_kernel::capability::{CapId, CapRights};
}

pub mod syscall {
    use crate::ipc::Message;
    use yarm_ipc_abi::ipc_v2::{
        IpcRegisterBlockV2, IpcV2SharedReplyMeta, IPC_ABI_V2_BLOCK_SIZE,
        IPC_V2_FLAG_INLINE_PAYLOAD, IPC_V2_FLAG_RECV_COPYOUT, IPC_V2_FLAG_RET_COPYOUT,
        IPC_V2_FLAG_TRANSFER_CAP, IPC_V2_NO_TRANSFER_CAP, IPC_V2_OP_CALL, IPC_V2_OP_RECV,
        IPC_V2_OP_REPLY, IPC_V2_OP_SEND, decode_shared_reply_meta, encode_shared_reply_meta,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(usize)]
    pub enum SyscallError {
        InvalidNumber = 1,
        InvalidArgs = 2,
        BufferTooSmall = 10,
        InvalidCapability = 3,
        MissingRight = 4,
        WrongObject = 5,
        QueueFull = 6,
        WouldBlock = 7,
        PageFault = 8,
        TimedOut = 9,
        Internal = 255,
    }

    const SYSCALL_IPC_SEND_V2_NR: usize = 15;
    const SYSCALL_IPC_RECV_V2_NR: usize = 16;
    const SYSCALL_IPC_CALL_V2_NR: usize = 17;
    const SYSCALL_IPC_REPLY_V2_NR: usize = 18;
    const SYSCALL_VM_ANON_MAP_NR: usize = 13;
    const SYSCALL_VM_UNMAP_NR: usize = 19;
    const SYSCALL_CAP_RELEASE_NR: usize = 20;
    const SYSCALL_YIELD_NR: usize = 0;
    pub const IPC_V2_DEFAULT_OPCODE: u16 = 0;
    pub trait IpcTransportV2 {
        fn send_v2(
            &mut self,
            endpoint_cap: u32,
            payload: &[u8],
            transfer_cap: Option<u64>,
        ) -> core::result::Result<(), SyscallError>;
        fn recv_v2(
            &mut self,
            recv_cap: u32,
        ) -> core::result::Result<Option<IpcV2Response>, SyscallError>;
        fn recv_v2_with_deadline(
            &mut self,
            recv_cap: u32,
            timeout_ticks: u64,
        ) -> core::result::Result<Option<IpcV2Response>, SyscallError>;
        fn reply_v2(
            &mut self,
            reply_cap: u32,
            payload: &[u8],
            transfer_cap: Option<u64>,
        ) -> core::result::Result<(), SyscallError>;
        fn call_v2(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
        ) -> core::result::Result<IpcV2Response, SyscallError>;
        fn request_reply_v2<T>(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
            decode_reply: impl FnOnce(&[u8]) -> Option<T>,
        ) -> core::result::Result<T, SyscallError>;
    }

    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub struct SyscallIpcTransport;

    impl IpcTransportV2 for SyscallIpcTransport {
        #[inline]
        fn send_v2(
            &mut self,
            endpoint_cap: u32,
            payload: &[u8],
            transfer_cap: Option<u64>,
        ) -> core::result::Result<(), SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_send_v2(endpoint_cap, payload, transfer_cap) }
        }

        #[inline]
        fn recv_v2(
            &mut self,
            recv_cap: u32,
        ) -> core::result::Result<Option<IpcV2Response>, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_recv_v2(recv_cap) }
        }
        #[inline]
        fn recv_v2_with_deadline(
            &mut self,
            recv_cap: u32,
            timeout_ticks: u64,
        ) -> core::result::Result<Option<IpcV2Response>, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_recv_v2_with_deadline(recv_cap, timeout_ticks) }
        }

        #[inline]
        fn reply_v2(
            &mut self,
            reply_cap: u32,
            payload: &[u8],
            transfer_cap: Option<u64>,
        ) -> core::result::Result<(), SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_reply_v2(reply_cap, payload, transfer_cap) }
        }

        #[inline]
        fn call_v2(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
        ) -> core::result::Result<IpcV2Response, SyscallError> {
            // SAFETY: forwards directly to syscall wrapper.
            unsafe { ipc_call_v2(send_cap, reply_recv_cap, payload) }
        }

        #[inline]
        fn request_reply_v2<T>(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
            decode_reply: impl FnOnce(&[u8]) -> Option<T>,
        ) -> core::result::Result<T, SyscallError> {
            request_reply_v2(self, send_cap, reply_recv_cap, payload, decode_reply)
        }
    }

    #[inline]
    pub fn request_reply_v2<T>(
        transport: &mut impl IpcTransportV2,
        send_cap: u32,
        reply_recv_cap: u32,
        payload: &[u8],
        decode_reply: impl FnOnce(&[u8]) -> Option<T>,
    ) -> core::result::Result<T, SyscallError> {
        let response = transport.call_v2(send_cap, reply_recv_cap, payload)?;
        decode_reply(&response.payload[..response.len]).ok_or(SyscallError::InvalidArgs)
    }

    #[inline]
    const fn decode_syscall_error(code: usize) -> SyscallError {
        match code {
            1 => SyscallError::InvalidNumber,
            2 => SyscallError::InvalidArgs,
            10 => SyscallError::BufferTooSmall,
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IpcV2Response {
        /// For `RECV_V2`/`CALL_V2`, this carries received message opcode.
        pub status: u64,
        pub len: usize,
        pub transfer_cap: Option<u64>,
        pub payload: [u8; Message::MAX_PAYLOAD],
    }
    impl IpcV2Response {
        #[inline]
        pub const fn opcode(&self) -> u16 {
            self.status as u16
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SharedReplyResponse {
        /// Compatibility field retained from pre-opcode-carriage naming.
        /// In IPC v2 receive/call responses this carries message opcode.
        pub status: u64,
        pub transfer_cap: u64,
        pub offset: u64,
        pub len: u64,
        pub flags: u16,
    }
    impl SharedReplyResponse {
        #[inline]
        pub const fn opcode(&self) -> u16 {
            self.status as u16
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AnonMapResult {
        pub base: usize,
        pub len: usize,
        pub mem_cap: u64,
    }

    #[inline]
    pub fn decode_shared_reply_response(
        response: &IpcV2Response,
    ) -> core::result::Result<SharedReplyResponse, SyscallError> {
        let transfer_cap = response.transfer_cap.ok_or(SyscallError::InvalidArgs)?;
        let meta = decode_shared_reply_meta(&response.payload[..response.len])
            .map_err(|_| SyscallError::InvalidArgs)?;
        Ok(SharedReplyResponse {
            status: response.status,
            transfer_cap,
            offset: meta.offset,
            len: meta.len,
            flags: meta.flags,
        })
    }

    #[inline]
    pub unsafe fn vm_anon_map(
        base: usize,
        len: usize,
        prot: u64,
    ) -> core::result::Result<AnonMapResult, SyscallError> {
        let args = [base, len, prot as usize, 0, 0, 0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_VM_ANON_MAP_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            return Err(decode_syscall_error(ret.error));
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            return Err(decode_syscall_error(ret.ret0));
        }
        Ok(AnonMapResult {
            base: ret.ret0,
            len: ret.ret1,
            mem_cap: ret.ret2 as u64,
        })
    }

    #[inline]
    pub unsafe fn vm_unmap(base: usize, len: usize) -> core::result::Result<(), SyscallError> {
        let args = [base, len, 0, 0, 0, 0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_VM_UNMAP_NR, args) };
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
    pub unsafe fn cap_release(cap: u64) -> core::result::Result<(), SyscallError> {
        let args = [cap as usize, 0, 0, 0, 0, 0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_CAP_RELEASE_NR, args) };
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

    pub(crate) fn fill_v2_payload_block(block: &mut IpcRegisterBlockV2, payload: &[u8]) -> core::result::Result<(), SyscallError> {
        if payload.len() > Message::MAX_PAYLOAD {
            return Err(SyscallError::InvalidArgs);
        }
        block.len = payload.len() as u64;
        if payload.len() <= 64 {
            block.flags |= IPC_V2_FLAG_INLINE_PAYLOAD;
            block.inline_words = [0; 8];
            for (idx, chunk) in payload.chunks(8).enumerate() {
                let mut lane = [0u8; 8];
                lane[..chunk.len()].copy_from_slice(chunk);
                block.inline_words[idx] = u64::from_le_bytes(lane);
            }
        } else {
            block.ptr_or_offset = payload.as_ptr() as u64;
            block.flags &= !IPC_V2_FLAG_INLINE_PAYLOAD;
        }
        Ok(())
    }

    pub(crate) fn decode_v2_response(block: &IpcRegisterBlockV2) -> core::result::Result<IpcV2Response, SyscallError> {
        let len = usize::try_from(block.ret_len).map_err(|_| SyscallError::Internal)?;
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::Internal);
        }
        let mut payload = [0u8; Message::MAX_PAYLOAD];
        if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0 {
            if len > 64 {
                return Err(SyscallError::Internal);
            }
            for (idx, word) in block.inline_words.iter().enumerate() {
                let start = idx * 8;
                if start >= len {
                    break;
                }
                let bytes = word.to_le_bytes();
                let take = core::cmp::min(8, len - start);
                payload[start..start + take].copy_from_slice(&bytes[..take]);
            }
        } else if (block.flags & IPC_V2_FLAG_RET_COPYOUT) != 0 {
            // Payload bytes are already copied to caller-provided output buffer.
        }
        let transfer_cap = if block.ret_transfer_cap == IPC_V2_NO_TRANSFER_CAP {
            None
        } else {
            Some(block.ret_transfer_cap)
        };
        Ok(IpcV2Response {
            status: block.ret_status,
            len,
            transfer_cap,
            payload,
        })
    }

    #[inline]
    pub unsafe fn ipc_send_v2(
        endpoint_cap: u32,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> core::result::Result<(), SyscallError> {
        // SAFETY: delegates to opcode-aware wrapper with default opcode.
        unsafe { ipc_send_v2_msg(endpoint_cap, IPC_V2_DEFAULT_OPCODE, payload, transfer_cap) }
    }

    #[inline]
    pub unsafe fn ipc_send_v2_msg(
        endpoint_cap: u32,
        opcode: u16,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> core::result::Result<(), SyscallError> {
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.endpoint_cap = endpoint_cap as u64;
        block.aux0 = opcode as u64;
        fill_v2_payload_block(&mut block, payload)?;
        if let Some(cap) = transfer_cap {
            block.flags |= IPC_V2_FLAG_TRANSFER_CAP;
            block.transfer_cap = cap;
        }
        let args = [
            (&mut block as *mut IpcRegisterBlockV2) as usize,
            IPC_ABI_V2_BLOCK_SIZE,
            0,0,0,0
        ];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_SEND_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 { return Err(decode_syscall_error(ret.error)); }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 { return Err(decode_syscall_error(ret.ret0)); }
        Ok(())
    }

    #[inline]
    pub unsafe fn ipc_recv_v2(recv_cap: u32) -> core::result::Result<Option<IpcV2Response>, SyscallError> {
        // SAFETY: delegate to deadline variant with nonblocking timeout.
        unsafe { ipc_recv_v2_with_deadline(recv_cap, 0) }
    }

    #[inline]
    pub unsafe fn ipc_recv_v2_with_deadline(
        recv_cap: u32,
        timeout_ticks: u64,
    ) -> core::result::Result<Option<IpcV2Response>, SyscallError> {
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        block.endpoint_cap = recv_cap as u64;
        block.aux0 = timeout_ticks;
        let args = [(&mut block as *mut IpcRegisterBlockV2) as usize, IPC_ABI_V2_BLOCK_SIZE, 0,0,0,0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) { Ok(None) } else { Err(err) };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) { Ok(None) } else { Err(err) };
        }
        Ok(Some(decode_v2_response(&block)?))
    }

    #[inline]
    pub unsafe fn ipc_reply_v2(
        reply_cap: u32,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> core::result::Result<(), SyscallError> {
        // SAFETY: delegates to opcode-aware wrapper with default opcode.
        unsafe { ipc_reply_v2_msg(reply_cap, IPC_V2_DEFAULT_OPCODE, payload, transfer_cap) }
    }

    #[inline]
    pub unsafe fn ipc_reply_v2_msg(
        reply_cap: u32,
        opcode: u16,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> core::result::Result<(), SyscallError> {
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        block.endpoint_cap = reply_cap as u64;
        block.aux0 = opcode as u64;
        fill_v2_payload_block(&mut block, payload)?;
        if let Some(cap) = transfer_cap {
            block.flags |= IPC_V2_FLAG_TRANSFER_CAP;
            block.transfer_cap = cap;
        }
        let args = [(&mut block as *mut IpcRegisterBlockV2) as usize, IPC_ABI_V2_BLOCK_SIZE, 0,0,0,0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_REPLY_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 { return Err(decode_syscall_error(ret.error)); }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 { return Err(decode_syscall_error(ret.ret0)); }
        Ok(())
    }

    /// Convenience helper for stage-1 shared replies.
    ///
    /// This helper only sends metadata + transfer cap via existing `IPC_REPLY_V2`.
    /// It does not map memory automatically.
    #[inline]
    pub unsafe fn ipc_reply_v2_shared(
        reply_cap: u32,
        mem_cap: u64,
        offset: u64,
        len: u64,
        flags: u16,
    ) -> core::result::Result<(), SyscallError> {
        let meta = IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags,
            reserved: 0,
            offset,
            len,
        };
        let payload = encode_shared_reply_meta(meta).map_err(|_| SyscallError::InvalidArgs)?;
        // SAFETY: wrapper over existing reply syscall path.
        unsafe { ipc_reply_v2_msg(reply_cap, IPC_V2_DEFAULT_OPCODE, &payload, Some(mem_cap)) }
    }

    #[inline]
    pub unsafe fn ipc_call_v2(
        send_cap: u32,
        reply_recv_cap: u32,
        payload: &[u8],
    ) -> core::result::Result<IpcV2Response, SyscallError> {
        // SAFETY: delegates to opcode-aware wrapper with default opcode.
        unsafe { ipc_call_v2_msg(send_cap, reply_recv_cap, IPC_V2_DEFAULT_OPCODE, payload) }
    }

    #[inline]
    pub unsafe fn ipc_call_v2_msg(
        send_cap: u32,
        reply_recv_cap: u32,
        opcode: u16,
        payload: &[u8],
    ) -> core::result::Result<IpcV2Response, SyscallError> {
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        block.endpoint_cap = send_cap as u64;
        block.aux0 = reply_recv_cap as u64;
        block.aux1 = opcode as u64;
        fill_v2_payload_block(&mut block, payload)?;
        let args = [(&mut block as *mut IpcRegisterBlockV2) as usize, IPC_ABI_V2_BLOCK_SIZE, 0,0,0,0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_CALL_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 { return Err(decode_syscall_error(ret.error)); }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 { return Err(decode_syscall_error(ret.ret0)); }
        decode_v2_response(&block)
    }

    /// Convenience helper that expects shared-reply metadata in the response payload.
    ///
    /// This helper does not map transferred memory automatically.
    #[inline]
    pub unsafe fn ipc_call_v2_expect_shared(
        send_cap: u32,
        reply_recv_cap: u32,
        payload: &[u8],
    ) -> core::result::Result<SharedReplyResponse, SyscallError> {
        // SAFETY: wrapper over existing call syscall path.
        let response = unsafe { ipc_call_v2(send_cap, reply_recv_cap, payload) }?;
        decode_shared_reply_response(&response)
    }

    #[inline]
    pub unsafe fn ipc_recv_v2_into(
        recv_cap: u32,
        timeout_ticks: u64,
        out: &mut [u8],
    ) -> core::result::Result<Option<(u64, usize, Option<u64>)>, SyscallError> {
        // Return tuple: (opcode, payload_len, transfer_cap).
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        block.endpoint_cap = recv_cap as u64;
        block.aux0 = timeout_ticks;
        block.aux1 = out.as_mut_ptr() as u64;
        block.len = out.len() as u64;
        block.flags = IPC_V2_FLAG_RECV_COPYOUT;
        let args = [(&mut block as *mut IpcRegisterBlockV2) as usize, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_RECV_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 {
            let err = decode_syscall_error(ret.error);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) { Ok(None) } else { Err(err) };
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 {
            let err = decode_syscall_error(ret.ret0);
            return if matches!(err, SyscallError::WouldBlock | SyscallError::TimedOut) { Ok(None) } else { Err(err) };
        }
        let len = usize::try_from(block.ret_len).map_err(|_| SyscallError::Internal)?;
        let transfer_cap = if block.ret_transfer_cap == IPC_V2_NO_TRANSFER_CAP {
            None
        } else {
            Some(block.ret_transfer_cap)
        };
        Ok(Some((block.ret_status, len, transfer_cap)))
    }

    #[inline]
    pub unsafe fn ipc_call_v2_into(
        send_cap: u32,
        reply_recv_cap: u32,
        payload: &[u8],
        out: &mut [u8],
    ) -> core::result::Result<(u64, usize, Option<u64>), SyscallError> {
        // Return tuple: (reply_opcode, payload_len, transfer_cap).
        if payload.len() > 64 {
            return Err(SyscallError::InvalidArgs);
        }
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        block.endpoint_cap = send_cap as u64;
        block.aux0 = reply_recv_cap as u64;
        block.aux1 = out.as_mut_ptr() as u64;
        block.len = out.len() as u64;
        block.flags = IPC_V2_FLAG_RECV_COPYOUT | IPC_V2_FLAG_INLINE_PAYLOAD;
        block.inline_words = [0; 8];
        for (idx, chunk) in payload.chunks(8).enumerate() {
            let mut lane = [0u8; 8];
            lane[..chunk.len()].copy_from_slice(chunk);
            block.inline_words[idx] = u64::from_le_bytes(lane);
        }
        let args = [(&mut block as *mut IpcRegisterBlockV2) as usize, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0];
        let ret = unsafe { crate::arch::raw_syscall(SYSCALL_IPC_CALL_V2_NR, args) };
        #[cfg(target_arch = "x86_64")]
        if ret.error != 0 { return Err(decode_syscall_error(ret.error)); }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        if ret.ret0 != 0 { return Err(decode_syscall_error(ret.ret0)); }
        let len = usize::try_from(block.ret_len).map_err(|_| SyscallError::Internal)?;
        let transfer_cap = if block.ret_transfer_cap == IPC_V2_NO_TRANSFER_CAP {
            None
        } else {
            Some(block.ret_transfer_cap)
        };
        Ok((block.ret_status, len, transfer_cap))
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
    use yarm_ipc_abi::process_abi::ServiceStartupCapsV1;
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
    /// Primary meaning: process-manager restart-control SEND cap.
    ///
    /// Staged/service-conditional meaning: initramfs server request RECV cap
    /// during FS IPC-loop bring-up (slot count/layout unchanged).
    pub const STARTUP_SLOT_PROCESS_MANAGER_RESTART_CONTROL_SEND_CAP: usize = 11;
    pub const STARTUP_SLOT_INIT_ORCH_VERSION_AND_RESERVED: usize = 12;
    pub const STARTUP_SLOT_INITRAMFS_REQUEST_SEND_CAP: usize = 13;
    pub const STARTUP_SLOT_INITRAMFS_REQUEST_RECV_CAP_FOR_CHILD: usize = 14;
    pub const STARTUP_SLOT_INIT_ORCH_CONTROL0: usize = 15;
    pub const STARTUP_SLOT_INIT_ORCH_CONTROL1: usize = 16;
    const STARTUP_SLOT_COUNT: usize = 17;
    const _: () = assert!(STARTUP_SLOT_INIT_ORCH_CONTROL1 < STARTUP_SLOT_COUNT);
    const STARTUP_ARGS_BYTES: usize = STARTUP_SLOT_COUNT * core::mem::size_of::<u64>();

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
    }

    impl StartupContext {
        #[inline]
        pub fn process_manager_caps(self) -> Option<(u32, u32)> {
            let slot1_raw = STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_REQUEST_SEND_CAP]
                .load(Ordering::Relaxed);
            let slot2_raw = STARTUP_ARG_SLOTS[STARTUP_SLOT_PROCESS_MANAGER_REPLY_RECV_CAP]
                .load(Ordering::Relaxed);
            let mut msg = [0u8; 96];
            let mut len = 0usize;
            fn append(msg: &mut [u8], len: &mut usize, text: &str) {
                let bytes = text.as_bytes();
                let n = core::cmp::min(bytes.len(), msg.len().saturating_sub(*len));
                msg[*len..*len + n].copy_from_slice(&bytes[..n]);
                *len += n;
            }
            append(&mut msg, &mut len, "STARTUP_PM_CAPS_SLOT1 value=");
            append_u64_decimal(&mut msg, &mut len, slot1_raw);
            if len != 0 {
                let marker = unsafe { core::str::from_utf8_unchecked(&msg[..len]) };
                crate::serial_marker_line(marker);
            }
            len = 0;
            append(&mut msg, &mut len, "STARTUP_PM_CAPS_SLOT2 value=");
            append_u64_decimal(&mut msg, &mut len, slot2_raw);
            if len != 0 {
                let marker = unsafe { core::str::from_utf8_unchecked(&msg[..len]) };
                crate::serial_marker_line(marker);
            }
            match (self.process_manager_request_send_cap, self.process_manager_reply_recv_cap) {
                (Some(request_send), Some(reply_recv)) => {
                    crate::serial_marker_line("STARTUP_PM_CAPS_VALID");
                    Some((request_send, reply_recv))
                }
                _ => {
                    crate::serial_marker_line("STARTUP_PM_CAPS_INVALID reason=slot_zero_or_out_of_range");
                    None
                }
            }
        }

        /// Staged, service-scoped interpretation for initramfs FS server startup.
        ///
        /// This reuses startup slot 11 (`process_manager_restart_control_send_cap`)
        /// as a request receive endpoint capability *only* for initramfs server
        /// launch wiring during the staged FS IPC-loop rollout.
        /// Other services must continue to interpret slot 11 by its primary
        /// process-manager restart-control meaning.
        #[inline]
        pub const fn initramfs_request_recv_cap_from_slot11(self) -> Option<u32> {
            self.process_manager_restart_control_send_cap
        }

        #[inline]
        pub fn initramfs_startup_caps_v1_from_startup_args(self) -> Option<ServiceStartupCapsV1> {
            let sig = STARTUP_ARG_SLOTS[0].load(Ordering::Relaxed);
            if (sig & 0xFFFF_FFFF) != 0x5354_4350 {
                return None;
            }
            let version = ((sig >> 48) & 0xFFFF) as u16;
            let role = ((sig >> 32) & 0xFFFF) as u16;
            let request_recv_cap = STARTUP_ARG_SLOTS[1].load(Ordering::Relaxed);
            let control_send_cap = STARTUP_ARG_SLOTS[2].load(Ordering::Relaxed);
            let control_recv_cap = STARTUP_ARG_SLOTS[3].load(Ordering::Relaxed);
            let reserved0 = STARTUP_ARG_SLOTS[4].load(Ordering::Relaxed);
            Some(ServiceStartupCapsV1 {
                version,
                role,
                request_recv_cap,
                control_send_cap,
                control_recv_cap,
                reserved0,
            })
        }

        #[inline]
        pub fn init_orchestration_caps_v1(self) -> Option<yarm_ipc_abi::process_abi::InitOrchestrationCapsV1> {
            let hdr = STARTUP_ARG_SLOTS[STARTUP_SLOT_INIT_ORCH_VERSION_AND_RESERVED].load(Ordering::Relaxed);
            let version = (hdr & 0xFFFF) as u16;
            if version != yarm_ipc_abi::process_abi::InitOrchestrationCapsV1::VERSION {
                return None;
            }
            Some(yarm_ipc_abi::process_abi::InitOrchestrationCapsV1 {
                version,
                reserved: ((hdr >> 16) & 0xFFFF) as u16,
                initramfs_request_send_cap: STARTUP_ARG_SLOTS[STARTUP_SLOT_INITRAMFS_REQUEST_SEND_CAP].load(Ordering::Relaxed),
                initramfs_request_recv_cap_for_child: STARTUP_ARG_SLOTS[STARTUP_SLOT_INITRAMFS_REQUEST_RECV_CAP_FOR_CHILD].load(Ordering::Relaxed),
                control0: STARTUP_ARG_SLOTS[STARTUP_SLOT_INIT_ORCH_CONTROL0].load(Ordering::Relaxed),
                control1: STARTUP_ARG_SLOTS[STARTUP_SLOT_INIT_ORCH_CONTROL1].load(Ordering::Relaxed),
            })
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

    #[inline]
    fn append_u64_decimal(buf: &mut [u8], len: &mut usize, mut value: u64) {
        let mut digits = [0u8; 20];
        let mut dlen = 0usize;
        if value == 0 {
            digits[0] = b'0';
            dlen = 1;
        } else {
            while value != 0 && dlen < digits.len() {
                digits[dlen] = b'0' + (value % 10) as u8;
                value /= 10;
                dlen += 1;
            }
            digits[..dlen].reverse();
        }
        let copy = core::cmp::min(dlen, buf.len().saturating_sub(*len));
        buf[*len..*len + copy].copy_from_slice(&digits[..copy]);
        *len += copy;
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
    pub fn install_startup_arg_slots<const N: usize>(slots: [u64; N]) {
        let mut index = 0usize;
        while index < STARTUP_SLOT_COUNT {
            let value = if index < N { slots[index] } else { 0 };
            STARTUP_ARG_SLOTS[index].store(value, Ordering::Relaxed);
            index += 1;
        }
    }


    #[inline]
    fn emit_startup_install_arg_marker(label: &str, value: usize) {
        let mut line = [0u8; 128];
        let mut len = 0usize;
        let bytes = label.as_bytes();
        let copy = core::cmp::min(bytes.len(), line.len());
        line[..copy].copy_from_slice(&bytes[..copy]);
        len += copy;
        append_u64_decimal(&mut line, &mut len, value as u64);
        let msg = unsafe { core::str::from_utf8_unchecked(&line[..len]) };
        crate::serial_marker_line(msg);
    }

    #[inline]
    fn emit_startup_install_slots_marker(label: &str, slot1: u64, slot2: u64) {
        let mut line = [0u8; 160];
        let mut len = 0usize;
        let prefix = label.as_bytes();
        let copy = core::cmp::min(prefix.len(), line.len());
        line[..copy].copy_from_slice(&prefix[..copy]);
        len += copy;
        append_u64_decimal(&mut line, &mut len, slot1);
        if len < line.len() { line[len]=b' '; len += 1; }
        const SLOT2: &[u8] = b"slot2=";
        let copy2 = core::cmp::min(SLOT2.len(), line.len().saturating_sub(len));
        line[len..len+copy2].copy_from_slice(&SLOT2[..copy2]);
        len += copy2;
        append_u64_decimal(&mut line, &mut len, slot2);
        let msg = unsafe { core::str::from_utf8_unchecked(&line[..len]) };
        crate::serial_marker_line(msg);
    }

    #[inline]
    fn install_startup_args_from_abi(
        startup_task_id: u64,
        startup_proc_mgr_request_send_cap: u64,
        startup_proc_mgr_reply_recv_cap: u64,
        startup_slots_ptr: usize,
        startup_slots_len: usize,
    ) {
        emit_startup_install_arg_marker("STARTUP_INSTALL_ARG0 value=", startup_task_id as usize);
        emit_startup_install_arg_marker("STARTUP_INSTALL_ARG1 value=", startup_proc_mgr_request_send_cap as usize);
        emit_startup_install_arg_marker("STARTUP_INSTALL_ARG2 value=", startup_proc_mgr_reply_recv_cap as usize);
        emit_startup_install_arg_marker("STARTUP_INSTALL_ARG3 value=", startup_slots_ptr);
        emit_startup_install_arg_marker("STARTUP_INSTALL_ARG4 value=", startup_slots_len);
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
        ];
        debug_assert_eq!(STARTUP_ARGS_BYTES, slots.len() * core::mem::size_of::<u64>());
        emit_startup_install_slots_marker("STARTUP_INSTALL_AFTER_DIRECT slot1=", slots[1], slots[2]);
        if startup_slots_ptr != 0 && startup_slots_len >= slots.len() {
            let src = startup_slots_ptr as *const u64;
            let mut index = 0usize;
            while index < slots.len() {
                // SAFETY: bounded by `slots.len()` and guarded by non-zero pointer
                // + contract length check above.
                slots[index] = unsafe { core::ptr::read(src.add(index)) };
                index += 1;
            }
            emit_startup_install_slots_marker("STARTUP_INSTALL_AFTER_BLOCK slot1=", slots[1], slots[2]);
        }
        emit_startup_install_slots_marker("STARTUP_INSTALL_FINAL slot1=", slots[1], slots[2]);
        install_startup_arg_slots(slots);
    }

    #[cfg(test)]
    pub(crate) fn install_startup_args_from_abi_for_test(
        startup_task_id: u64,
        startup_proc_mgr_request_send_cap: u64,
        startup_proc_mgr_reply_recv_cap: u64,
        startup_slots_ptr: usize,
        startup_slots_len: usize,
    ) {
        install_startup_args_from_abi(
            startup_task_id,
            startup_proc_mgr_request_send_cap,
            startup_proc_mgr_reply_recv_cap,
            startup_slots_ptr,
            startup_slots_len,
        );
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
    use crate::syscall::{IpcTransportV2, IpcV2Response, SyscallError};
    use yarm_ipc_abi::ipc_v2::{
        IpcV2SharedReplyMeta, IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
        IPC_V2_SHARED_REPLY_META_VERSION, encode_shared_reply_meta,
    };
    use yarm_ipc_abi::process_abi::ServiceStartupCapsV1;

    #[test]
    fn ipc_v2_inline_encoder_roundtrip() {
        let mut block = yarm_ipc_abi::ipc_v2::IpcRegisterBlockV2::new_v2(yarm_ipc_abi::ipc_v2::IPC_V2_OP_SEND);
        super::syscall::fill_v2_payload_block(&mut block, b"hello").expect("encode");
        assert_ne!(block.flags & yarm_ipc_abi::ipc_v2::IPC_V2_FLAG_INLINE_PAYLOAD, 0);
        block.ret_len = 5;
        let decoded = super::syscall::decode_v2_response(&block).expect("decode");
        assert_eq!(&decoded.payload[..5], b"hello");
    }

    #[test]
    fn ipc_v2_large_payload_uses_pointer_mode() {
        let payload = [7u8; 80];
        let mut block = yarm_ipc_abi::ipc_v2::IpcRegisterBlockV2::new_v2(yarm_ipc_abi::ipc_v2::IPC_V2_OP_SEND);
        super::syscall::fill_v2_payload_block(&mut block, &payload).expect("encode");
        assert_eq!(block.flags & yarm_ipc_abi::ipc_v2::IPC_V2_FLAG_INLINE_PAYLOAD, 0);
        assert_ne!(block.ptr_or_offset, 0);
    }

    #[test]
    fn startup_process_manager_caps_require_both_slots() {
        let original = startup_context();

        install_startup_arg_slots([42, 11, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), Some((11, 12)));

        install_startup_arg_slots([42, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), None);

        install_startup_arg_slots([42, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
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
        ]);
    }

    #[test]
    fn startup_canonical_17_slot_layout_preserves_pm_caps_slots() {
        let original = startup_context();
        install_startup_arg_slots([42, 5001, 5002, 9, 10, 11, 12, 13, 14, 15, 16, 17, 0, 18, 19, 20, 21]);
        assert_eq!(startup_context().process_manager_caps(), Some((5001, 5002)));
        assert_eq!(startup_context().init_orchestration_caps_v1(), None);
        install_startup_arg_slots([
            original.task_id,
            original.process_manager_request_send_cap.map(u64::from).unwrap_or(0),
            original.process_manager_reply_recv_cap.map(u64::from).unwrap_or(0),
            original.supervisor_fault_recv_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_send_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_recv_ep.map(u64::from).unwrap_or(0),
            original.init_alert_send_ep.map(u64::from).unwrap_or(0),
            original.init_alert_recv_ep.map(u64::from).unwrap_or(0),
            original.init_tid.unwrap_or(0),
            original.supervisor_tid.unwrap_or(0),
            original.supervisor_restart_window_ticks.unwrap_or(0),
            original.process_manager_restart_control_send_cap.map(|v| v as u64).unwrap_or(0),
            original
                .init_orchestration_caps_v1()
                .map(|c| ((c.version as u64) << 48) | ((c.reserved as u64) << 32))
                .unwrap_or(0),
            original.init_orchestration_caps_v1().map(|c| c.initramfs_request_send_cap).unwrap_or(0),
            original
                .init_orchestration_caps_v1()
                .map(|c| c.initramfs_request_recv_cap_for_child)
                .unwrap_or(0),
            original.init_orchestration_caps_v1().map(|c| c.control0).unwrap_or(0),
            original.init_orchestration_caps_v1().map(|c| c.control1).unwrap_or(0),
        ]);
    }

    #[test]
    fn startup_process_manager_caps_accept_dynamic_cap_ids() {
        let original = startup_context();
        install_startup_arg_slots([1, 65536, 65537, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
        install_startup_arg_slots([
            original.task_id,
            original.process_manager_request_send_cap.map(u64::from).unwrap_or(0),
            original.process_manager_reply_recv_cap.map(u64::from).unwrap_or(0),
            original.supervisor_fault_recv_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_send_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_recv_ep.map(u64::from).unwrap_or(0),
            original.init_alert_send_ep.map(u64::from).unwrap_or(0),
            original.init_alert_recv_ep.map(u64::from).unwrap_or(0),
            original.init_tid.unwrap_or(0),
            original.supervisor_tid.unwrap_or(0),
            original.supervisor_restart_window_ticks.unwrap_or(0),
            original.process_manager_restart_control_send_cap.map(|v| v as u64).unwrap_or(0),
            0,0,0,0,0,
        ]);
    }

    #[test]
    fn startup_abi_direct_args_and_block_path_match_for_pm_caps() {
        let original = startup_context();
        let mut slots = [0u64; 17];
        slots[0] = 99;
        slots[1] = 65536;
        slots[2] = 65537;
        super::runtime::install_startup_args_from_abi_for_test(99, 65536, 65537, slots.as_ptr() as usize, slots.len());
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
        super::runtime::install_startup_args_from_abi_for_test(99, 65536, 65537, 0, 0);
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
        install_startup_arg_slots([
            original.task_id,
            original.process_manager_request_send_cap.map(u64::from).unwrap_or(0),
            original.process_manager_reply_recv_cap.map(u64::from).unwrap_or(0),
            original.supervisor_fault_recv_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_send_ep.map(u64::from).unwrap_or(0),
            original.supervisor_control_recv_ep.map(u64::from).unwrap_or(0),
            original.init_alert_send_ep.map(u64::from).unwrap_or(0),
            original.init_alert_recv_ep.map(u64::from).unwrap_or(0),
            original.init_tid.unwrap_or(0),
            original.supervisor_tid.unwrap_or(0),
            original.supervisor_restart_window_ticks.unwrap_or(0),
            original.process_manager_restart_control_send_cap.map(|v| v as u64).unwrap_or(0),
            0,0,0,0,0,
        ]);
    }

    #[test]
    fn startup_initramfs_recv_cap_uses_slot11_staged_mapping() {
        let original = startup_context();
        install_startup_arg_slots([42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 77]);
        let ctx = startup_context();
        assert_eq!(ctx.process_manager_restart_control_send_cap, Some(77));
        assert_eq!(ctx.initramfs_request_recv_cap_from_slot11(), Some(77));

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
        ]);
    }

    struct MockTransportV2 {
        response_payload: [u8; 4],
    }

    impl IpcTransportV2 for MockTransportV2 {
        fn send_v2(
            &mut self,
            _endpoint_cap: u32,
            _payload: &[u8],
            _transfer_cap: Option<u64>,
        ) -> Result<(), SyscallError> {
            panic!("not used");
        }
        fn recv_v2(&mut self, _recv_cap: u32) -> Result<Option<IpcV2Response>, SyscallError> {
            panic!("not used");
        }
        fn recv_v2_with_deadline(
            &mut self,
            _recv_cap: u32,
            _timeout_ticks: u64,
        ) -> Result<Option<IpcV2Response>, SyscallError> {
            panic!("not used");
        }
        fn reply_v2(
            &mut self,
            _reply_cap: u32,
            _payload: &[u8],
            _transfer_cap: Option<u64>,
        ) -> Result<(), SyscallError> {
            panic!("not used");
        }
        fn call_v2(
            &mut self,
            _send_cap: u32,
            _reply_recv_cap: u32,
            _payload: &[u8],
        ) -> Result<IpcV2Response, SyscallError> {
            let mut payload = [0u8; crate::ipc::Message::MAX_PAYLOAD];
            payload[..4].copy_from_slice(&self.response_payload);
            Ok(IpcV2Response {
                status: 0xCAFE,
                len: 4,
                transfer_cap: None,
                payload,
            })
        }
        fn request_reply_v2<T>(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
            decode_reply: impl FnOnce(&[u8]) -> Option<T>,
        ) -> Result<T, SyscallError> {
            crate::syscall::request_reply_v2(self, send_cap, reply_recv_cap, payload, decode_reply)
        }
    }

    #[test]
    fn request_reply_v2_decodes_typed_reply() {
        let mut transport = MockTransportV2 {
            response_payload: *b"pong",
        };
        let decoded = crate::syscall::request_reply_v2(&mut transport, 1, 2, b"ping", |reply| {
            (reply == b"pong").then_some(7u8)
        })
        .expect("typed reply");
        // request_reply_v2 decodes payload only; it must not depend on opcode/status lane.
        assert_eq!(decoded, 7);
    }

    #[test]
    fn decode_v2_response_accepts_ret_copyout_mode() {
        let mut block = yarm_ipc_abi::ipc_v2::IpcRegisterBlockV2::new_v2(
            yarm_ipc_abi::ipc_v2::IPC_V2_OP_RECV,
        );
        block.flags = yarm_ipc_abi::ipc_v2::IPC_V2_FLAG_RET_COPYOUT;
        block.ret_len = 65;
        let decoded = super::syscall::decode_v2_response(&block).expect("decode");
        assert_eq!(decoded.len, 65);
    }

    #[test]
    fn decode_shared_reply_response_succeeds_with_transfer_cap_and_valid_meta() {
        let meta = IpcV2SharedReplyMeta {
            version: IPC_V2_SHARED_REPLY_META_VERSION,
            flags: IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0x3000,
            len: 0x1000,
        };
        let meta_bytes = encode_shared_reply_meta(meta).expect("encode");
        let mut payload = [0u8; crate::ipc::Message::MAX_PAYLOAD];
        payload[..meta_bytes.len()].copy_from_slice(&meta_bytes);
        let response = IpcV2Response {
            status: 99,
            len: meta_bytes.len(),
            transfer_cap: Some(55),
            payload,
        };
        let decoded = crate::syscall::decode_shared_reply_response(&response).expect("decode");
        assert_eq!(decoded.status, 99);
        assert_eq!(decoded.opcode(), 99);
        assert_eq!(decoded.transfer_cap, 55);
        assert_eq!(decoded.offset, 0x3000);
        assert_eq!(decoded.len, 0x1000);
        assert_eq!(decoded.flags, IPC_V2_SHARED_REPLY_FLAG_READ_ONLY);
    }

    #[test]
    fn decode_shared_reply_response_fails_without_transfer_cap() {
        let mut payload = [0u8; crate::ipc::Message::MAX_PAYLOAD];
        payload[0] = 1;
        let response = IpcV2Response {
            status: 0,
            len: 24,
            transfer_cap: None,
            payload,
        };
        assert_eq!(
            crate::syscall::decode_shared_reply_response(&response),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn decode_shared_reply_response_fails_on_bad_metadata() {
        let mut payload = [0u8; crate::ipc::Message::MAX_PAYLOAD];
        let bad_version = (IPC_V2_SHARED_REPLY_META_VERSION + 1).to_le_bytes();
        payload[0..2].copy_from_slice(&bad_version);
        payload[16..24].copy_from_slice(&1u64.to_le_bytes());
        let response = IpcV2Response {
            status: 0,
            len: 24,
            transfer_cap: Some(7),
            payload,
        };
        assert_eq!(
            crate::syscall::decode_shared_reply_response(&response),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn shared_helpers_are_metadata_only_and_do_not_map() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("ipc_reply_v2_shared")
                && src.contains("ipc_call_v2_expect_shared")
                && src.contains("unsafe { ipc_reply_v2_msg(reply_cap, IPC_V2_DEFAULT_OPCODE, &payload, Some(mem_cap)) }")
                && src.contains("decode_shared_reply_response(&response)"),
            "shared reply helpers must remain metadata/transfer-cap wrappers without automatic mapping",
        );
    }

    #[test]
    fn vm_anon_map_wrapper_is_exposed_with_expected_syscall_number() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("const SYSCALL_VM_ANON_MAP_NR: usize = 13;")
                && src.contains("const SYSCALL_VM_UNMAP_NR: usize = 19;")
                && src.contains("const SYSCALL_CAP_RELEASE_NR: usize = 20;")
                && src.contains("pub struct AnonMapResult")
                && src.contains("pub unsafe fn vm_anon_map(")
                && src.contains("pub unsafe fn vm_unmap(")
                && src.contains("pub unsafe fn cap_release("),
            "user runtime must expose staged vm_anon_map/vm_unmap/cap_release wrappers and result type",
        );
    }

    #[test]
    fn serial_marker_line_uses_single_buffered_syscall_path() {
        let arch_src = include_str!("arch/mod.rs");
        let lib_src = include_str!("lib.rs");
        assert!(
            arch_src.contains("const SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR: usize = 22;")
                && arch_src.contains("raw_syscall(\n                    SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR,")
                && !arch_src.contains("[byte as usize, 0, 0, 0, 0, 0]"),
            "marker transport must use buffered debug syscall rather than per-byte loops",
        );
        assert!(
            lib_src.contains("crate::arch::serial_write_bytes(&line[..len + 1]);"),
            "serial_marker_line must submit the full marker line in one buffered call",
        );
    }

    #[test]
    fn startup_abi_install_preserves_slot2_with_valid_block() {
        let mut slots = [0u64; 17];
        slots[0] = 1;
        slots[1] = 65536;
        slots[2] = 65537;
        super::runtime::install_startup_args_from_abi_for_test(1, 65536, 65537, slots.as_ptr() as usize, 17);
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
    }

    #[test]
    fn startup_abi_direct_args_only_preserve_slot2() {
        super::runtime::install_startup_args_from_abi_for_test(1, 65536, 65537, 0, 0);
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
    }

    #[test]
    fn startup_abi_block_path_wins_but_keeps_pm_reply_cap() {
        let mut slots = [0u64; 17];
        slots[0] = 7;
        slots[1] = 65536;
        slots[2] = 65537;
        super::runtime::install_startup_args_from_abi_for_test(1, 2, 3, slots.as_ptr() as usize, 17);
        assert_eq!(startup_context().process_manager_caps(), Some((65536, 65537)));
    }

    #[test]
    fn startup_args_structured_caps_decode_roundtrip() {
        install_startup_arg_slots([((1u64) << 48) | ((2u64) << 32) | 0x5354_4350, 41, 42, 43, 44, 0, 0, 0, 0, 0, 0, 0]);
        let caps = startup_context().initramfs_startup_caps_v1_from_startup_args().expect("caps");
        assert_eq!(caps, ServiceStartupCapsV1 { version: 1, role: 2, request_recv_cap: 41, control_send_cap: 42, control_recv_cap: 43, reserved0: 44 });
    }

    #[test]
    fn startup_args_structured_caps_malformed_signature_rejected() {
        install_startup_arg_slots([0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(startup_context().initramfs_startup_caps_v1_from_startup_args(), None);
    }
}
