// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::boot::{KernelError, KernelState, TransferSharedRegion};
use super::capabilities::{CapId, CapObject, CapRights};
use super::ipc::{
    IPC_REGISTER_BYTES, Message, SharedMemoryRegion, pack_register_payload, unpack_register_payload,
};
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;
use super::vm::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::arch::syscall_abi;

pub const SYSCALL_ABI_VERSION: u16 = 9;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_VM_MAP_NR: usize = 3;
pub const SYSCALL_TRANSFER_RELEASE_NR: usize = 4;
pub const SYSCALL_IPC_RECV_TIMEOUT_NR: usize = 5;
pub const SYSCALL_IPC_CALL_NR: usize = 6;
pub const SYSCALL_IPC_REPLY_NR: usize = 7;
pub const SYSCALL_COUNT: usize = 8;
const _: [(); SYSCALL_COUNT] = [(); 8];
pub const SYSCALL_ARG_CAP: usize = 0;
pub const SYSCALL_ARG_PTR: usize = 1;
pub const SYSCALL_ARG_LEN: usize = 2;
/// First inline IPC payload register lane in the stable cross-arch syscall ABI.
pub const SYSCALL_ARG_INLINE_PAYLOAD0: usize = 3;
/// Second inline IPC payload register lane in the stable cross-arch syscall ABI.
pub const SYSCALL_ARG_INLINE_PAYLOAD1: usize = 4;
/// Transfer-cap send requires a known waiting receiver; otherwise send returns `WouldBlock`.
pub const SYSCALL_ARG_TRANSFER_CAP: usize = syscall_abi::TRAPFRAME_ARG_REGS - 1;
pub const SYSCALL_RET_STATUS: usize = 0;
pub const SYSCALL_RET_AUX: usize = 1;
pub const SYSCALL_RET_TRANSFER_CAP: usize = 2;
pub const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
pub const SYSCALL_VM_MAP_PROT_READ: usize = 0x1;
pub const SYSCALL_VM_MAP_PROT_WRITE: usize = 0x2;
pub const SYSCALL_VM_MAP_PROT_EXEC: usize = 0x4;
pub const OPCODE_INLINE: u16 = 0;
pub const OPCODE_SHARED_MEM: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Syscall {
    Yield = SYSCALL_YIELD_NR,
    IpcSend = SYSCALL_IPC_SEND_NR,
    IpcRecv = SYSCALL_IPC_RECV_NR,
    VmMap = SYSCALL_VM_MAP_NR,
    TransferRelease = SYSCALL_TRANSFER_RELEASE_NR,
    IpcRecvTimeout = SYSCALL_IPC_RECV_TIMEOUT_NR,
    IpcCall = SYSCALL_IPC_CALL_NR,
    IpcReply = SYSCALL_IPC_REPLY_NR,
}

impl Syscall {
    pub const VARIANT_COUNT: usize = 8;
    pub const fn number(self) -> usize {
        self as usize
    }

    pub fn decode(raw: usize) -> Result<Self, SyscallError> {
        match raw {
            SYSCALL_YIELD_NR => Ok(Self::Yield),
            SYSCALL_IPC_SEND_NR => Ok(Self::IpcSend),
            SYSCALL_IPC_RECV_NR => Ok(Self::IpcRecv),
            SYSCALL_VM_MAP_NR => Ok(Self::VmMap),
            SYSCALL_TRANSFER_RELEASE_NR => Ok(Self::TransferRelease),
            SYSCALL_IPC_RECV_TIMEOUT_NR => Ok(Self::IpcRecvTimeout),
            SYSCALL_IPC_CALL_NR => Ok(Self::IpcCall),
            SYSCALL_IPC_REPLY_NR => Ok(Self::IpcReply),
            _ => Err(SyscallError::InvalidNumber),
        }
    }
}

const _: () = assert!(SYSCALL_COUNT == Syscall::VARIANT_COUNT);
const _: [(); syscall_abi::TRAPFRAME_ARG_REGS] = [(); 6];
const _: () = assert!(SYSCALL_ARG_TRANSFER_CAP < syscall_abi::TRAPFRAME_ARG_REGS);
const _: () = assert!(syscall_abi::TRAPFRAME_ARG_REGS > SYSCALL_ARG_INLINE_PAYLOAD1);

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

impl SyscallError {
    pub const fn code(self) -> usize {
        self as usize
    }
}

impl From<KernelError> for SyscallError {
    fn from(value: KernelError) -> Self {
        match value {
            KernelError::VmFull
            | KernelError::SchedulerFull
            | KernelError::CapabilityFull
            | KernelError::EndpointFull
            | KernelError::TaskTableFull
            | KernelError::TaskMissing
            | KernelError::MemoryObjectFull
            | KernelError::MemoryObjectMissing
            | KernelError::Vm(_) => Self::Internal,
            KernelError::InvalidCapability => Self::InvalidCapability,
            KernelError::MissingRight => Self::MissingRight,
            KernelError::WrongObject | KernelError::StaleCapability => Self::WrongObject,
            KernelError::EndpointQueueFull => Self::QueueFull,
            KernelError::UserMemoryFault => Self::PageFault,
            KernelError::WouldBlock => Self::WouldBlock,
        }
    }
}

fn current_tid(kernel: &KernelState) -> Result<u64, SyscallError> {
    kernel.current_tid().ok_or(SyscallError::Internal)
}

fn current_task_has_user_asid(kernel: &KernelState) -> Result<bool, SyscallError> {
    Ok(kernel.task_asid(current_tid(kernel)?).is_some())
}

fn record_user_fault(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    addr: usize,
    access: FaultAccess,
) {
    kernel.record_fault(FaultInfo {
        addr: VirtAddr(addr as u64),
        access,
    });
    frame.set_err(SyscallError::PageFault.code());
}

fn transfer_flag_bits(transfer_cap: Option<CapId>) -> u16 {
    if transfer_cap.is_some() {
        Message::FLAG_CAP_TRANSFER
    } else {
        0
    }
}

fn sender_tid_to_ret(tid: u64) -> Result<usize, SyscallError> {
    usize::try_from(tid).map_err(|_| SyscallError::Internal)
}

fn transfer_cap_arg(
    _kernel: &KernelState,
    frame: &TrapFrame,
) -> Result<Option<CapId>, SyscallError> {
    let raw = frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64;
    if raw == SYSCALL_NO_TRANSFER_CAP {
        return Ok(None);
    }
    Ok(Some(CapId(raw)))
}

fn encode_transfer_cap_ret(frame: &mut TrapFrame, cap: Option<u64>) -> Result<(), SyscallError> {
    let value = cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP);
    frame.set_ret2(usize::try_from(value).map_err(|_| SyscallError::Internal)?);
    Ok(())
}

fn materialize_received_transfer_cap(
    kernel: &mut KernelState,
    transfer_handle: Option<u64>,
    endpoint: CapObject,
    receiver_tid: u64,
) -> Result<Option<u64>, SyscallError> {
    let Some(handle) = transfer_handle else {
        return Ok(None);
    };
    let envelope = kernel
        .take_transfer_envelope(handle, endpoint, crate::kernel::ipc::ThreadId(receiver_tid))
        .ok_or(SyscallError::InvalidCapability)?;
    let source_capability = kernel
        .resolve_capability_for_task(envelope.source_tid.0, envelope.source_cap)
        .map_err(SyscallError::from)?;
    let derived = kernel
        .capability_service_mut()
        .grant_task_to_task_with_rights(
            envelope.source_tid.0,
            envelope.source_cap,
            receiver_tid,
            source_capability.rights(),
        )
        .map_err(SyscallError::from)?;
    Ok(Some(derived.0))
}

fn validate_user_region(offset: u64, len: u64) -> Result<(), SyscallError> {
    let user_end_exclusive = crate::arch::vm_layout::KERNEL_SPACE_BASE;
    if offset >= user_end_exclusive {
        return Err(SyscallError::InvalidArgs);
    }
    let end_exclusive = offset.checked_add(len).ok_or(SyscallError::InvalidArgs)?;
    if end_exclusive > user_end_exclusive {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(())
}

fn validate_endpoint_right(
    kernel: &KernelState,
    cap: CapId,
    right: CapRights,
) -> Result<(), SyscallError> {
    let endpoint_cap = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?;
    if !matches!(endpoint_cap.object, CapObject::Endpoint { .. }) {
        return Err(SyscallError::WrongObject);
    }
    if !endpoint_cap.has_right(right) {
        return Err(SyscallError::MissingRight);
    }
    Ok(())
}

fn validate_transfer_cap(kernel: &KernelState, cap: CapId) -> Result<(), SyscallError> {
    if kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .is_none()
    {
        return Err(SyscallError::InvalidCapability);
    }
    Ok(())
}

fn validate_shared_mem_transfer_rights(
    capability: &crate::kernel::capabilities::Capability,
) -> Result<(), SyscallError> {
    if !capability.has_right(CapRights::READ)
        || !capability.has_right(CapRights::WRITE)
        || !capability.has_right(CapRights::MAP)
    {
        return Err(SyscallError::MissingRight);
    }
    Ok(())
}

fn stash_transfer_handle(
    kernel: &mut KernelState,
    transfer_cap: Option<CapId>,
    endpoint: CapObject,
    shared_region: Option<TransferSharedRegion>,
) -> Result<Option<u64>, SyscallError> {
    let Some(source_cap_id) = transfer_cap else {
        return Ok(None);
    };
    let sender_tid = current_tid(kernel)?;
    let _ = kernel
        .resolve_capability_for_task(sender_tid, source_cap_id)
        .map_err(SyscallError::from)?;
    let receiver_tid = kernel
        .endpoint_waiter_tid(endpoint)
        .ok_or(SyscallError::WouldBlock)?;
    Ok(Some(
        kernel
            .stash_transfer_envelope(
                crate::kernel::ipc::ThreadId(sender_tid),
                source_cap_id,
                endpoint,
                Some(receiver_tid),
                shared_region,
            )
            .ok_or(SyscallError::QueueFull)?,
    ))
}

fn inline_payload_from_frame(
    frame: &TrapFrame,
    len: usize,
) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    if len > IPC_REGISTER_BYTES || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let words = [
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
    ];
    let regs = unpack_register_payload(words, len).ok_or(SyscallError::InvalidArgs)?;
    let mut payload = [0u8; Message::MAX_PAYLOAD];
    payload[..len].copy_from_slice(&regs[..len]);
    Ok(payload)
}

fn vm_map_page_flags(prot: usize) -> Result<PageFlags, SyscallError> {
    let unknown =
        prot & !(SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE | SYSCALL_VM_MAP_PROT_EXEC);
    if unknown != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(PageFlags {
        read: (prot & SYSCALL_VM_MAP_PROT_READ) != 0,
        write: (prot & SYSCALL_VM_MAP_PROT_WRITE) != 0,
        execute: (prot & SYSCALL_VM_MAP_PROT_EXEC) != 0,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    })
}

fn round_up_page(value: usize) -> Result<usize, SyscallError> {
    if value.is_multiple_of(PAGE_SIZE) {
        Ok(value)
    } else {
        let rounded = value
            .checked_add(PAGE_SIZE - 1)
            .ok_or(SyscallError::InvalidArgs)?;
        Ok(rounded & !(PAGE_SIZE - 1))
    }
}

fn map_shared_region_into_receiver(
    kernel: &mut KernelState,
    receiver_mem_cap: CapId,
    requested_va: usize,
    region_len: usize,
) -> Result<(usize, usize), SyscallError> {
    if requested_va == 0 || region_len == 0 || !requested_va.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let mapped_len = round_up_page(region_len)?;
    let mut va = requested_va;
    let end = requested_va
        .checked_add(mapped_len)
        .ok_or(SyscallError::InvalidArgs)?;
    let map_flags = PageFlags {
        read: true,
        write: true,
        execute: false,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    };

    while va < end {
        if let Err(err) = kernel.map_user_page_in_current_asid_with_caps(
            receiver_mem_cap,
            VirtAddr(va as u64),
            map_flags,
        ) {
            let mut rollback = requested_va;
            while rollback < va {
                let _ = kernel.unmap_user_page_in_current_asid(VirtAddr(rollback as u64));
                rollback += PAGE_SIZE;
            }
            return Err(SyscallError::from(err));
        }
        va += PAGE_SIZE;
    }
    Ok((requested_va, mapped_len))
}

fn revoke_current_transfer_cap_best_effort(kernel: &mut KernelState, transfer_cap: CapId) {
    if let Some(cnode) = kernel.current_task_cnode() {
        let _ = kernel.revoke_capability_in_cnode(cnode, transfer_cap);
    }
}

fn handle_ipc_send(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let user_ptr_or_offset = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let transfer_cap = transfer_cap_arg(kernel, frame)?;
    if let Some(c) = transfer_cap {
        validate_transfer_cap(kernel, c)?;
    }
    let sender_tid = current_tid(kernel)?;

    let msg_result = if current_task_has_user_asid(kernel)? {
        if len > Message::MAX_PAYLOAD {
            let grant_cap = transfer_cap.ok_or(SyscallError::InvalidArgs)?;
            let grant = kernel
                .capability_service()
                .resolve_current_task_capability(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            validate_shared_mem_transfer_rights(&grant)?;
            validate_user_region(user_ptr_or_offset as u64, len as u64)?;
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let transfer_handle = stash_transfer_handle(
                kernel,
                transfer_cap,
                endpoint,
                Some(TransferSharedRegion {
                    offset: region.offset,
                    len: region.len,
                }),
            )?;
            Message::with_header(
                sender_tid,
                OPCODE_SHARED_MEM,
                Message::FLAG_CAP_TRANSFER,
                transfer_handle,
                &region.encode(),
            )
            .map_err(|_| SyscallError::InvalidArgs)
        } else {
            let payload = match kernel.copy_from_current_user(user_ptr_or_offset, len) {
                Ok(payload) => payload,
                Err(KernelError::UserMemoryFault) => {
                    record_user_fault(kernel, frame, user_ptr_or_offset, FaultAccess::Read);
                    return Ok(());
                }
                Err(other) => return Err(SyscallError::from(other)),
            };

            let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint, None)?;
            Message::with_header(
                sender_tid,
                OPCODE_INLINE,
                transfer_flag_bits(transfer_cap),
                transfer_handle,
                &payload[..len],
            )
            .map_err(|_| SyscallError::InvalidArgs)
        }
    } else {
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::InvalidArgs);
        }
        if len > IPC_REGISTER_BYTES {
            let grant_cap = transfer_cap.ok_or(SyscallError::InvalidArgs)?;
            let grant = kernel
                .capability_service()
                .resolve_current_task_capability(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            validate_shared_mem_transfer_rights(&grant)?;
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let transfer_handle = stash_transfer_handle(
                kernel,
                transfer_cap,
                endpoint,
                Some(TransferSharedRegion {
                    offset: region.offset,
                    len: region.len,
                }),
            )?;
            Message::with_header(
                sender_tid,
                OPCODE_SHARED_MEM,
                Message::FLAG_CAP_TRANSFER,
                transfer_handle,
                &region.encode(),
            )
            .map_err(|_| SyscallError::InvalidArgs)
        } else {
            let payload = inline_payload_from_frame(frame, len)?;
            let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint, None)?;
            Message::with_header(
                sender_tid,
                OPCODE_INLINE,
                transfer_flag_bits(transfer_cap),
                transfer_handle,
                &payload[..len],
            )
            .map_err(|_| SyscallError::InvalidArgs)
        }
    };
    let msg = match msg_result {
        Ok(msg) => msg,
        Err(err) => return Err(err),
    };

    if let Err(err) = kernel.ipc_send(cap, msg) {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
            let _ = kernel.take_transfer_envelope(
                handle,
                endpoint,
                crate::kernel::ipc::ThreadId(current_tid(kernel)?),
            );
        }
        return Err(SyscallError::from(err));
    }
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_recv(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::RECEIVE)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;
    handle_ipc_recv_result(
        kernel,
        frame,
        endpoint,
        frame.arg(SYSCALL_ARG_PTR),
        frame.arg(SYSCALL_ARG_LEN),
        received,
    )
}

fn handle_ipc_recv_timeout(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::RECEIVE)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let timeout_ticks = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0) as u64;
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let user_len = frame.arg(SYSCALL_ARG_LEN);
    let waiter_tid = current_tid(kernel)?;
    let received = if timeout_ticks == 0 {
        kernel.try_ipc_recv(cap).map_err(SyscallError::from)?
    } else {
        kernel
            .ipc_recv_with_deadline(cap, timeout_ticks)
            .map_err(SyscallError::from)?
    };
    let timed_out = if timeout_ticks == 0 {
        false
    } else {
        let fired = kernel
            .consume_ipc_timeout_fired_for_tid(waiter_tid)
            .map_err(SyscallError::from)?;
        fired || received.is_none()
    };
    handle_ipc_recv_result_with_empty_error(
        kernel,
        frame,
        endpoint,
        user_ptr,
        user_len,
        received,
        if timed_out {
            SyscallError::TimedOut
        } else {
            SyscallError::WouldBlock
        },
    )
}

fn handle_ipc_call(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;

    let reply_recv_cap = CapId(frame.arg(SYSCALL_ARG_TRANSFER_CAP) as u64);
    validate_endpoint_right(kernel, reply_recv_cap, CapRights::RECEIVE)?;
    let responder_tid = kernel
        .endpoint_waiter_tid(endpoint)
        .ok_or(SyscallError::WouldBlock)?;

    let sender_tid = current_tid(kernel)?;
    let reply_cap = kernel
        .create_reply_cap_for_caller(
            crate::kernel::ipc::ThreadId(sender_tid),
            reply_recv_cap,
            Some(responder_tid),
        )
        .map_err(SyscallError::from)?;

    let user_ptr_or_offset = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    let msg = if current_task_has_user_asid(kernel)? {
        let payload = match kernel.copy_from_current_user(user_ptr_or_offset, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(kernel, frame, user_ptr_or_offset, FaultAccess::Read);
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        };
        let transfer_handle = stash_transfer_handle(kernel, Some(reply_cap), endpoint, None)?;
        Message::with_header(
            sender_tid,
            OPCODE_INLINE,
            Message::FLAG_CAP_TRANSFER,
            transfer_handle,
            &payload[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)?
    } else {
        let payload = inline_payload_from_frame(frame, len)?;
        let transfer_handle = stash_transfer_handle(kernel, Some(reply_cap), endpoint, None)?;
        Message::with_header(
            sender_tid,
            OPCODE_INLINE,
            Message::FLAG_CAP_TRANSFER,
            transfer_handle,
            &payload[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)?
    };

    if let Err(err) = kernel.ipc_send(cap, msg) {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
            let _ = kernel.take_transfer_envelope(
                handle,
                endpoint,
                crate::kernel::ipc::ThreadId(current_tid(kernel)?),
            );
        }
        return Err(SyscallError::from(err));
    }
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_reply(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let reply_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let sender_tid = current_tid(kernel)?;
    let msg = if current_task_has_user_asid(kernel)? {
        let payload = match kernel.copy_from_current_user(user_ptr, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(kernel, frame, user_ptr, FaultAccess::Read);
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        };
        Message::new(sender_tid, &payload[..len]).map_err(|_| SyscallError::InvalidArgs)?
    } else {
        let payload = inline_payload_from_frame(frame, len)?;
        Message::new(sender_tid, &payload[..len]).map_err(|_| SyscallError::InvalidArgs)?
    };
    kernel
        .ipc_reply(reply_cap, msg)
        .map_err(SyscallError::from)?;
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_recv_result(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    endpoint: CapObject,
    user_ptr: usize,
    user_len: usize,
    received: Option<Message>,
) -> Result<(), SyscallError> {
    handle_ipc_recv_result_with_empty_error(
        kernel,
        frame,
        endpoint,
        user_ptr,
        user_len,
        received,
        SyscallError::WouldBlock,
    )
}

fn handle_ipc_recv_result_with_empty_error(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
    endpoint: CapObject,
    user_ptr: usize,
    user_len: usize,
    received: Option<Message>,
    empty_error: SyscallError,
) -> Result<(), SyscallError> {
    match received {
        Some(msg) => {
            let sender = sender_tid_to_ret(msg.sender_tid.0)?;
            let receiver_tid = current_tid(kernel)?;
            let recv_local_transfer = materialize_received_transfer_cap(
                kernel,
                msg.transferred_cap().map(|c| c.0),
                endpoint,
                receiver_tid,
            )?;
            encode_transfer_cap_ret(frame, recv_local_transfer)?;

            if current_task_has_user_asid(kernel)? {
                if msg.opcode == OPCODE_SHARED_MEM {
                    let desc = SharedMemoryRegion::decode(msg.as_slice())
                        .ok_or(SyscallError::InvalidArgs)?;
                    let region_len =
                        usize::try_from(desc.len).map_err(|_| SyscallError::InvalidArgs)?;
                    if user_ptr == 0 || user_len < region_len {
                        if frame.ret2() as u64 != SYSCALL_NO_TRANSFER_CAP {
                            revoke_current_transfer_cap_best_effort(
                                kernel,
                                CapId(frame.ret2() as u64),
                            );
                            encode_transfer_cap_ret(frame, None)?;
                        }
                        return Err(SyscallError::InvalidArgs);
                    }
                    let transfer_cap_raw =
                        u64::try_from(frame.ret2()).map_err(|_| SyscallError::InvalidArgs)?;
                    if transfer_cap_raw == SYSCALL_NO_TRANSFER_CAP {
                        return Err(SyscallError::InvalidArgs);
                    }
                    let transfer_cap = CapId(transfer_cap_raw);
                    let (mapped_va, mapped_len) = match map_shared_region_into_receiver(
                        kernel,
                        transfer_cap,
                        user_ptr,
                        region_len,
                    ) {
                        Ok(mapped) => mapped,
                        Err(err) => {
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            encode_transfer_cap_ret(frame, None)?;
                            return Err(err);
                        }
                    };
                    kernel
                        .register_active_transfer_mapping(
                            crate::kernel::ipc::ThreadId(receiver_tid),
                            transfer_cap,
                            VirtAddr(mapped_va as u64),
                            mapped_len,
                        )
                        .map_err(|e| {
                            let mut rollback = mapped_va;
                            let end = mapped_va.saturating_add(mapped_len);
                            while rollback < end {
                                let _ = kernel
                                    .unmap_user_page_in_current_asid(VirtAddr(rollback as u64));
                                rollback += PAGE_SIZE;
                            }
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            let _ = encode_transfer_cap_ret(frame, None);
                            SyscallError::from(e)
                        })?;
                    kernel.note_shared_mem_mapped(mapped_len);
                    frame.set_ok(sender, mapped_len, frame.ret2());
                    frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, mapped_va);
                    frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, region_len);
                    return Ok(());
                }

                if user_len < msg.len as usize {
                    return Err(SyscallError::InvalidArgs);
                }
                match kernel.copy_to_current_user(user_ptr, msg.as_slice()) {
                    Ok(()) => frame.set_ok(sender, msg.len as usize, frame.ret2()),
                    Err(KernelError::UserMemoryFault) => {
                        record_user_fault(kernel, frame, user_ptr, FaultAccess::Write);
                        return Ok(());
                    }
                    Err(other) => return Err(SyscallError::from(other)),
                };
            } else {
                frame.set_ok(sender, msg.len as usize, frame.ret2());
                let words =
                    pack_register_payload(msg.as_slice()).map_err(|_| SyscallError::InvalidArgs)?;
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, words[0]);
                frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, words[1]);
            }
        }
        None => {
            frame.set_err(empty_error.code());
            encode_transfer_cap_ret(frame, None)?;
        }
    }
    Ok(())
}

fn handle_vm_map(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let aspace_map_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let addr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let prot = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let map_len = round_up_page(len)?;
    let end = addr.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    let flags = vm_map_page_flags(prot)?;
    let mut va = addr;
    while va < end {
        let (_, mem_cap) = kernel
            .alloc_anonymous_memory_object()
            .map_err(SyscallError::from)?;
        kernel
            .map_user_page_with_caps(aspace_map_cap, mem_cap, VirtAddr(va as u64), flags)
            .map_err(SyscallError::from)?;
        va += PAGE_SIZE;
    }
    frame.set_ok(addr, map_len, 0);
    Ok(())
}

fn handle_transfer_release(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    if !current_task_has_user_asid(kernel)? {
        return Err(SyscallError::InvalidArgs);
    }
    let transfer_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let owner = crate::kernel::ipc::ThreadId(current_tid(kernel)?);
    let (base, map_len) = {
        let base_arg = frame.arg(SYSCALL_ARG_PTR);
        let len_arg = frame.arg(SYSCALL_ARG_LEN);
        if base_arg == 0 && len_arg == 0 {
            kernel
                .active_transfer_mapping_for(owner, transfer_cap)
                .map(|(base, len)| (base.0 as usize, len))
                .ok_or(SyscallError::InvalidArgs)?
        } else {
            if len_arg == 0 || !base_arg.is_multiple_of(PAGE_SIZE) {
                return Err(SyscallError::InvalidArgs);
            }
            (base_arg, round_up_page(len_arg)?)
        }
    };
    let end = base.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    let mut va = base;
    while va < end {
        let unmapped = kernel
            .unmap_user_page_in_current_asid(VirtAddr(va as u64))
            .map_err(SyscallError::from)?;
        if unmapped.is_none() {
            return Err(SyscallError::InvalidArgs);
        }
        va += PAGE_SIZE;
    }

    let cnode = kernel.current_task_cnode().ok_or(SyscallError::Internal)?;
    kernel
        .revoke_capability_in_cnode(cnode, transfer_cap)
        .map_err(SyscallError::from)?;
    let _ = kernel.remove_active_transfer_mapping(owner, transfer_cap);
    kernel.note_shared_mem_released(map_len);
    frame.set_ok(map_len, 0, 0);
    Ok(())
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    match Syscall::decode(frame.syscall_num())? {
        Syscall::Yield => {
            kernel.yield_current().map_err(SyscallError::from)?;
            frame.set_ok(0, 0, 0);
            Ok(())
        }
        Syscall::IpcSend => handle_ipc_send(kernel, frame),
        Syscall::IpcRecv => handle_ipc_recv(kernel, frame),
        Syscall::IpcRecvTimeout => handle_ipc_recv_timeout(kernel, frame),
        Syscall::IpcCall => handle_ipc_call(kernel, frame),
        Syscall::IpcReply => handle_ipc_reply(kernel, frame),
        Syscall::VmMap => handle_vm_map(kernel, frame),
        Syscall::TransferRelease => handle_transfer_release(kernel, frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::ipc::IPC_REGISTER_WORDS;
    use crate::kernel::trapframe::TrapFrame;

    #[test]
    fn syscall_abi_numbers_are_frozen() {
        assert_eq!(SYSCALL_ABI_VERSION, 9);
        assert_eq!(SYSCALL_ARG_TRANSFER_CAP, 5);
        assert_eq!(SYSCALL_RET_TRANSFER_CAP, 2);
        assert_eq!(SYSCALL_TRANSFER_RELEASE_NR, 4);
        assert_eq!(SYSCALL_IPC_RECV_TIMEOUT_NR, 5);
        assert_eq!(SYSCALL_IPC_CALL_NR, 6);
        assert_eq!(SYSCALL_IPC_REPLY_NR, 7);
        assert_eq!(IPC_REGISTER_WORDS, 2);
    }

    #[test]
    fn syscall_recv_timeout_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_RECV_TIMEOUT_NR).expect("decode"),
            Syscall::IpcRecvTimeout
        );
    }

    #[test]
    fn syscall_ipc_call_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_CALL_NR).expect("decode"),
            Syscall::IpcCall
        );
    }

    #[test]
    fn syscall_ipc_reply_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_REPLY_NR).expect("decode"),
            Syscall::IpcReply
        );
    }

    #[test]
    fn syscall_recv_timeout_can_pull_queued_message() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let msg = Message::new(7, b"ok").expect("msg");
        state.ipc_send(send_cap, msg).expect("send");

        let mut frame = TrapFrame::new(
            Syscall::IpcRecvTimeout as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("recv timeout");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 7);
        assert_eq!(frame.ret1(), 2);
    }

    #[test]
    fn syscall_recv_timeout_zero_returns_would_block_when_empty() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");

        let mut frame = TrapFrame::new(
            Syscall::IpcRecvTimeout as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("recv timeout");
        assert_eq!(frame.error_code(), Some(SyscallError::WouldBlock.code()));
    }

    #[test]
    fn syscall_recv_timeout_nonzero_returns_timed_out_when_empty() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");

        let mut frame = TrapFrame::new(
            Syscall::IpcRecvTimeout as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 1, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("recv timeout");
        assert_eq!(frame.error_code(), Some(SyscallError::TimedOut.code()));
    }

    #[test]
    fn syscall_ipc_call_attaches_single_use_reply_cap_to_request() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");

        let (_call_eid, call_send_cap, call_recv_cap_global) =
            state.create_endpoint(4).expect("call ep");
        let call_recv_cap = state
            .grant_capability_task_to_task(0, call_recv_cap_global, 1)
            .expect("dup recv cap");
        let (_reply_eid, _reply_send, reply_recv_cap) = state.create_endpoint(4).expect("reply ep");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [call_recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let payload_word = usize::from_le_bytes(*b"call0000");
        let mut frame = TrapFrame::new(
            Syscall::IpcCall as usize,
            [
                call_send_cap.0 as usize,
                0,
                8,
                payload_word,
                0,
                reply_recv_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut frame).expect("ipc call");
        assert_eq!(frame.error_code(), None);

        state.yield_current().expect("switch receiver");
        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [call_recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv).expect("recv syscall");

        assert_eq!(recv.ret1(), 8);
        let bytes = unpack_register_payload(
            [
                recv.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
                recv.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            ],
            recv.ret1(),
        )
        .expect("payload");
        assert_eq!(&bytes[..8], b"call0000");
        assert_ne!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
    }

    #[test]
    fn syscall_ipc_reply_routes_message_and_consumes_reply_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let reply_cap = state
            .create_reply_cap_for_caller(crate::kernel::ipc::ThreadId(0), recv_cap, None)
            .expect("reply cap");
        let payload_word = usize::from_le_bytes(*b"reply000");
        let mut frame = TrapFrame::new(
            Syscall::IpcReply as usize,
            [reply_cap.0 as usize, 0, 8, payload_word, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("ipc reply");
        assert_eq!(frame.error_code(), None);

        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv).expect("recv syscall");
        let bytes = unpack_register_payload(
            [
                recv.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
                recv.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            ],
            recv.ret1(),
        )
        .expect("payload");
        assert_eq!(&bytes[..8], b"reply000");

        let mut replay = TrapFrame::new(
            Syscall::IpcReply as usize,
            [reply_cap.0 as usize, 0, 8, payload_word, 0, 0],
        );
        let err = dispatch(&mut state, &mut replay).expect_err("single use");
        assert_eq!(err, SyscallError::WrongObject);
    }

    #[test]
    fn vm_map_syscall_maps_aligned_region() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");
        let mut frame = TrapFrame::new(
            Syscall::VmMap as usize,
            [
                aspace_map_cap.0 as usize,
                0x4000,
                PAGE_SIZE * 2,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut frame).expect("vm_map");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0x4000);
        assert_eq!(frame.ret1(), PAGE_SIZE * 2);
    }

    #[test]
    fn syscall_error_codes_are_stable() {
        assert_eq!(SyscallError::InvalidNumber.code(), 1);
        assert_eq!(SyscallError::InvalidArgs.code(), 2);
        assert_eq!(SyscallError::InvalidCapability.code(), 3);
        assert_eq!(SyscallError::MissingRight.code(), 4);
        assert_eq!(SyscallError::WrongObject.code(), 5);
        assert_eq!(SyscallError::QueueFull.code(), 6);
        assert_eq!(SyscallError::WouldBlock.code(), 7);
        assert_eq!(SyscallError::PageFault.code(), 8);
        assert_eq!(SyscallError::TimedOut.code(), 9);
        assert_eq!(SyscallError::Internal.code(), 255);
    }

    #[test]
    fn transfer_cap_arg_zero_is_not_treated_as_none() {
        let state = Bootstrap::init().expect("kernel");

        let mut frame = TrapFrame::zeroed();
        frame.set_arg(SYSCALL_ARG_TRANSFER_CAP, 0);

        assert_eq!(
            transfer_cap_arg(&state, &frame).expect("decode transfer cap"),
            Some(CapId(0))
        );
    }

    #[test]
    fn syscall_recv_materializes_receiver_local_transfer_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0x7000))
            .expect("mem");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert!(
            state
                .capability_service()
                .current_task_capability_has_right(recv_cap, CapRights::RECEIVE),
            "receiver task must own receive cap"
        );
        let mut block_recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv_frame).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0,
                2,
                usize::from_le_bytes([b'o', b'k', 0, 0, 0, 0, 0, 0]),
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send_frame).expect("send syscall");
        assert_eq!(send_frame.error_code(), None);
        state.yield_current().expect("switch to receiver");
        assert_eq!(state.current_tid(), Some(1));

        let mut frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("recv syscall");

        assert_eq!(frame.error_code(), None);
        let recv_local = CapId(frame.ret2() as u64);
        assert_ne!(recv_local, mem_cap);
        let mapped = state
            .capability_service()
            .resolve_current_task_capability(recv_local)
            .expect("receiver-local transferred cap");
        assert!(matches!(mapped.object, CapObject::MemoryObject { .. }));
        state.yield_current().expect("switch back to sender");
        assert_eq!(state.current_tid(), Some(0));
        let sender_cnode = state.task_cnode(0).expect("sender cnode");
        if let Some(sender_cap) = state.capability_for_cnode_local(sender_cnode, recv_local) {
            assert_ne!(sender_cap.object, mapped.object);
        }
    }

    #[test]
    fn syscall_recv_shared_mem_can_auto_map_into_receiver_when_requested() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");

        state.yield_current().expect("switch receiver");
        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [
                recv_cap.0 as usize,
                0x8000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut recv).expect("recv");
        assert_eq!(recv.error_code(), None);
        assert_eq!(recv.arg(SYSCALL_ARG_INLINE_PAYLOAD0), 0x8000);
        assert_eq!(
            recv.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
            Message::MAX_PAYLOAD + 16
        );
        assert_eq!(
            recv.ret1(),
            round_up_page(Message::MAX_PAYLOAD + 16).expect("rounded")
        );
    }

    #[test]
    fn syscall_recv_shared_mem_auto_map_rejects_unaligned_target_va() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");
        state.yield_current().expect("switch receiver");

        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [
                recv_cap.0 as usize,
                0x8101,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                0,
            ],
        );
        let err = dispatch(&mut state, &mut recv).expect_err("unaligned target");
        assert_eq!(err, SyscallError::InvalidArgs);
    }

    #[test]
    fn syscall_recv_shared_mem_auto_map_requires_len_budget() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");
        state.yield_current().expect("switch receiver");

        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0x8000, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        let err = dispatch(&mut state, &mut recv).expect_err("len budget too small");
        assert_eq!(err, SyscallError::InvalidArgs);
        assert_eq!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
    }

    #[test]
    fn syscall_recv_shared_mem_requires_nonzero_map_target() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");
        state.yield_current().expect("switch receiver");

        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD + 16, 0, 0, 0],
        );
        let err = dispatch(&mut state, &mut recv).expect_err("zero map target");
        assert_eq!(err, SyscallError::InvalidArgs);
    }

    #[test]
    fn syscall_send_shared_mem_requires_map_right_on_transfer_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let readonly_object = state
            .current_task_capability(mem_cap)
            .expect("mem cap")
            .object;
        let readonly_cap = state
            .mint_capability_for_current_context(crate::kernel::capabilities::Capability::new(
                readonly_object,
                CapRights::READ,
            ))
            .expect("readonly cap");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                readonly_cap.0 as usize,
            ],
        );
        let err = dispatch(&mut state, &mut send).expect_err("missing map right");
        assert_eq!(err, SyscallError::MissingRight);
    }

    #[test]
    fn shared_mem_send_rights_rejection_does_not_create_transfer_envelopes() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        state.bind_task_asid(0, asid0).expect("bind0");
        let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let readonly_object = state
            .current_task_capability(mem_cap)
            .expect("mem cap")
            .object;
        let readonly_cap = state
            .mint_capability_for_current_context(crate::kernel::capabilities::Capability::new(
                readonly_object,
                CapRights::READ,
            ))
            .expect("readonly cap");

        for _ in 0..64 {
            let mut send = TrapFrame::new(
                Syscall::IpcSend as usize,
                [
                    send_cap.0 as usize,
                    0x2000,
                    Message::MAX_PAYLOAD + 16,
                    0,
                    0,
                    readonly_cap.0 as usize,
                ],
            );
            let err = dispatch(&mut state, &mut send).expect_err("missing map right");
            assert_eq!(err, SyscallError::MissingRight);
        }
        let t = state.ipc_path_telemetry();
        assert_eq!(t.transfer_records_created, 0);
    }

    #[test]
    fn syscall_send_shared_mem_requires_write_right_on_transfer_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let no_write_object = state
            .current_task_capability(mem_cap)
            .expect("mem cap")
            .object;
        let no_write_cap = state
            .mint_capability_for_current_context(crate::kernel::capabilities::Capability::new(
                no_write_object,
                CapRights::READ | CapRights::MAP,
            ))
            .expect("no-write cap");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                no_write_cap.0 as usize,
            ],
        );
        let err = dispatch(&mut state, &mut send).expect_err("missing write right");
        assert_eq!(err, SyscallError::MissingRight);
    }

    #[test]
    fn syscall_transfer_release_unmaps_receiver_range_and_revokes_transfer_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");

        state.yield_current().expect("switch receiver");
        let map_base = 0xA000usize;
        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [
                recv_cap.0 as usize,
                map_base,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut recv).expect("recv");
        let recv_local_transfer = CapId(recv.ret2() as u64);

        let mut release = TrapFrame::new(
            Syscall::TransferRelease as usize,
            [
                recv_local_transfer.0 as usize,
                map_base,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut release).expect("release");
        assert_eq!(
            state
                .capability_service()
                .resolve_current_task_capability(recv_local_transfer),
            None
        );
        assert_eq!(
            state.copy_to_current_user(map_base, b"x"),
            Err(KernelError::UserMemoryFault)
        );
        let t = state.ipc_path_telemetry();
        assert_eq!(t.transfer_records_revoked, 1);
        assert_eq!(t.transfer_release_calls, 1);
        assert_eq!(t.shared_mem_bytes_mapped, PAGE_SIZE as u64);
        assert_eq!(t.shared_mem_bytes_released, PAGE_SIZE as u64);
    }

    #[test]
    fn shared_mem_fastpath_throughput_smoke_tracks_volume_for_repeated_map_release() {
        let loops = 64usize;
        let mut total_mapped = 0u64;
        let mut total_released = 0u64;
        let mut total_release_calls = 0u64;
        for _ in 0..loops {
            let mut state = Bootstrap::init().expect("kernel");
            state.register_task(1).expect("task1");
            state.enqueue_current_cpu(1).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
            let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
            state.bind_task_asid(0, asid0).expect("bind0");
            state.bind_task_asid(1, asid1).expect("bind1");
            let (_eid, send_cap, recv_cap_global) = state.create_endpoint(8).expect("endpoint");
            let recv_cap = state
                .grant_capability_task_to_task(0, recv_cap_global, 1)
                .expect("dup recv cap");
            let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
            let map_base = 0xA000usize;
            state.yield_current().expect("switch receiver");
            let mut block_recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [recv_cap.0 as usize, 0, 0, 0, 0, 0],
            );
            dispatch(&mut state, &mut block_recv).expect("block recv");

            let mut send = TrapFrame::new(
                Syscall::IpcSend as usize,
                [
                    send_cap.0 as usize,
                    0x2000,
                    Message::MAX_PAYLOAD + 16,
                    0,
                    0,
                    mem_cap.0 as usize,
                ],
            );
            dispatch(&mut state, &mut send).expect("send");
            state.yield_current().expect("switch receiver");
            let mut recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [
                    recv_cap.0 as usize,
                    map_base,
                    Message::MAX_PAYLOAD + 16,
                    0,
                    0,
                    0,
                ],
            );
            dispatch(&mut state, &mut recv).expect("recv");
            let recv_local_transfer = CapId(recv.ret2() as u64);
            let mut release = TrapFrame::new(
                Syscall::TransferRelease as usize,
                [recv_local_transfer.0 as usize, 0, 0, 0, 0, 0],
            );
            dispatch(&mut state, &mut release).expect("release");
            let t = state.ipc_path_telemetry();
            total_mapped = total_mapped.saturating_add(t.shared_mem_bytes_mapped);
            total_released = total_released.saturating_add(t.shared_mem_bytes_released);
            total_release_calls = total_release_calls.saturating_add(t.transfer_release_calls);
        }
        let mapped_per_loop = PAGE_SIZE as u64;
        assert_eq!(total_release_calls, loops as u64);
        assert_eq!(total_mapped, loops as u64 * mapped_per_loop);
        assert_eq!(total_released, loops as u64 * mapped_per_loop);
    }

    #[test]
    fn shared_mem_canary_map_release_parity_under_repeated_load() {
        let loops = 32usize;
        let mut total_mapped = 0u64;
        let mut total_released = 0u64;
        for _ in 0..loops {
            let mut state = Bootstrap::init().expect("kernel");
            state.register_task(1).expect("task1");
            state.enqueue_current_cpu(1).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
            let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
            state.bind_task_asid(0, asid0).expect("bind0");
            state.bind_task_asid(1, asid1).expect("bind1");
            let (_eid, send_cap, recv_cap_global) = state.create_endpoint(8).expect("endpoint");
            let recv_cap = state
                .grant_capability_task_to_task(0, recv_cap_global, 1)
                .expect("dup recv cap");
            let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
            state.yield_current().expect("switch receiver");
            let mut block_recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [recv_cap.0 as usize, 0, 0, 0, 0, 0],
            );
            dispatch(&mut state, &mut block_recv).expect("block recv");
            let mut send = TrapFrame::new(
                Syscall::IpcSend as usize,
                [
                    send_cap.0 as usize,
                    0x2000,
                    Message::MAX_PAYLOAD + 16,
                    0,
                    0,
                    mem_cap.0 as usize,
                ],
            );
            dispatch(&mut state, &mut send).expect("send");
            state.yield_current().expect("switch receiver");
            let mut recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [
                    recv_cap.0 as usize,
                    0xA000,
                    Message::MAX_PAYLOAD + 16,
                    0,
                    0,
                    0,
                ],
            );
            dispatch(&mut state, &mut recv).expect("recv");
            let transfer_cap = CapId(recv.ret2() as u64);
            let mut release = TrapFrame::new(
                Syscall::TransferRelease as usize,
                [transfer_cap.0 as usize, 0, 0, 0, 0, 0],
            );
            dispatch(&mut state, &mut release).expect("release");
            let t = state.ipc_path_telemetry();
            total_mapped = total_mapped.saturating_add(t.shared_mem_bytes_mapped);
            total_released = total_released.saturating_add(t.shared_mem_bytes_released);
        }
        assert_eq!(total_mapped, total_released, "phase7 canary drift");
    }

    #[test]
    fn syscall_transfer_release_can_use_active_mapping_fast_path() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        let (asid1, _map_cap1) = state.create_user_address_space().expect("asid1");
        state.bind_task_asid(0, asid0).expect("bind0");
        state.bind_task_asid(1, asid1).expect("bind1");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        state.yield_current().expect("switch receiver");
        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv).expect("block recv");

        let mut send = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send).expect("send");

        state.yield_current().expect("switch receiver");
        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [
                recv_cap.0 as usize,
                0xA000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut recv).expect("recv");
        let recv_local_transfer = CapId(recv.ret2() as u64);

        let mut release = TrapFrame::new(
            Syscall::TransferRelease as usize,
            [recv_local_transfer.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut release).expect("release");
        assert_eq!(release.ret0(), PAGE_SIZE);
        assert_eq!(
            state
                .capability_service()
                .resolve_current_task_capability(recv_local_transfer),
            None
        );
    }

    #[test]
    fn syscall_transfer_release_rejects_unaligned_base() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid0, _map_cap0) = state.create_user_address_space().expect("asid0");
        state.bind_task_asid(0, asid0).expect("bind0");
        let mut release = TrapFrame::new(
            Syscall::TransferRelease as usize,
            [0, 0xA001, PAGE_SIZE, 0, 0, 0],
        );
        let err = dispatch(&mut state, &mut release).expect_err("unaligned");
        assert_eq!(err, SyscallError::InvalidArgs);
    }

    #[test]
    fn failed_send_does_not_leak_transfer_envelopes() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0x9000))
            .expect("mem");

        for _ in 0..256 {
            let mut send_frame = TrapFrame::new(
                Syscall::IpcSend as usize,
                [
                    send_cap.0 as usize,
                    0,
                    Message::MAX_PAYLOAD + 1,
                    0,
                    0,
                    mem_cap.0 as usize,
                ],
            );
            let err = dispatch(&mut state, &mut send_frame).expect_err("invalid inline send");
            assert_eq!(err, SyscallError::InvalidArgs);
        }
    }

    #[test]
    fn kernel_inline_send_can_fall_back_to_shared_region_for_larger_payloads() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap, recv_cap_global) = state.create_endpoint(2).expect("endpoint");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0xA000))
            .expect("mem");
        state.yield_current().expect("switch to task1");
        let mut block_recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut block_recv_frame).expect("block recv");
        assert_eq!(state.current_tid(), Some(0));

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x1200,
                IPC_REGISTER_BYTES + 1,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send_frame).expect("send syscall");
        assert_eq!(send_frame.error_code(), None);

        let msg = state.ipc_recv(recv_cap_global).expect("recv").expect("msg");
        assert_eq!(msg.opcode, OPCODE_SHARED_MEM);
        assert!(msg.flags & Message::FLAG_CAP_TRANSFER != 0);
        let region = SharedMemoryRegion::decode(msg.as_slice()).expect("region");
        assert_eq!(region.offset, 0x1200);
        assert_eq!(region.len as usize, IPC_REGISTER_BYTES + 1);
    }

    #[test]
    fn transfer_envelope_handle_is_bound_to_endpoint_context() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_e1, send1, recv1) = state.create_endpoint(2).expect("endpoint1");
        let (_e2, send2, recv2) = state.create_endpoint(2).expect("endpoint2");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0xA000))
            .expect("mem");
        let recv1_task1 = state
            .grant_capability_task_to_task(0, recv1, 1)
            .expect("dup recv1 to task1");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert_eq!(state.ipc_recv(recv1_task1).expect("block recv"), None);
        assert_eq!(state.current_tid(), Some(0));

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send1.0 as usize,
                0,
                2,
                usize::from_le_bytes([b'o', b'k', 0, 0, 0, 0, 0, 0]),
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send_frame).expect("send syscall");
        let staged = state.ipc_recv(recv1).expect("recv1").expect("msg1");
        let handle = staged.transferred_cap().expect("handle").0;

        let forged = Message::with_header(0, 0, Message::FLAG_CAP_TRANSFER, Some(handle), b"zz")
            .expect("forged");
        state.ipc_send(send2, forged).expect("queue forged");

        let mut recv_frame =
            TrapFrame::new(Syscall::IpcRecv as usize, [recv2.0 as usize, 0, 0, 0, 0, 0]);
        let err = dispatch(&mut state, &mut recv_frame).expect_err("endpoint mismatch");
        assert_eq!(err, SyscallError::InvalidCapability);
    }

    #[test]
    fn transfer_envelope_waiter_binding_rejects_wrong_receiver_task() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_e, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0xB000))
            .expect("mem");
        let recv_cap_task1 = state
            .grant_capability_task_to_task(0, recv_cap, 1)
            .expect("dup recv to task1");

        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert_eq!(state.ipc_recv(recv_cap_task1).expect("block recv"), None);
        assert_eq!(state.current_tid(), Some(0));

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0,
                2,
                usize::from_le_bytes([b'o', b'k', 0, 0, 0, 0, 0, 0]),
                0,
                mem_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut send_frame).expect("send");

        let mut wrong_recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        let err = dispatch(&mut state, &mut wrong_recv_frame).expect_err("wrong receiver");
        assert_eq!(err, SyscallError::InvalidCapability);
    }

    #[test]
    fn transfer_send_without_waiter_returns_would_block() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_e, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0xC000))
            .expect("mem");

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0,
                2,
                usize::from_le_bytes([b'o', b'k', 0, 0, 0, 0, 0, 0]),
                0,
                mem_cap.0 as usize,
            ],
        );
        let err = dispatch(&mut state, &mut send_frame).expect_err("receiver waiter required");
        assert_eq!(err, SyscallError::WouldBlock);
    }
}
