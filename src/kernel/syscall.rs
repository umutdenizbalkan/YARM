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
use crate::kernel::boot::UserImageSpec;
use crate::kernel::task::TaskClass;
use yarm_srv_common::{cpio::CpioArchive, elf::ElfImageInfo};

pub const SYSCALL_ABI_VERSION: u16 = 10;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_VM_MAP_NR: usize = 3;
pub const SYSCALL_TRANSFER_RELEASE_NR: usize = 4;
pub const SYSCALL_IPC_RECV_TIMEOUT_NR: usize = 5;
pub const SYSCALL_IPC_CALL_NR: usize = 6;
pub const SYSCALL_IPC_REPLY_NR: usize = 7;
pub const SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR: usize = 8;
pub const SYSCALL_FUTEX_WAIT_NR: usize = 9;
pub const SYSCALL_FUTEX_WAKE_NR: usize = 10;
pub const SYSCALL_SPAWN_THREAD_NR: usize = 11;
pub const SYSCALL_FORK_NR: usize = 12;
pub const SYSCALL_VM_ANON_MAP_NR: usize = 13;
pub const SYSCALL_VM_BRK_NR: usize = 14;
pub const SYSCALL_DEBUG_LOG_NR: usize = 15;
pub const SYSCALL_SPAWN_PROCESS_NR: usize = 23;
pub const SYSCALL_COUNT: usize = 24;
const _: [(); SYSCALL_COUNT] = [(); 24];
pub const SYSCALL_ARG_CAP: usize = 0;
pub const SYSCALL_ARG_PTR: usize = 1;
pub const SYSCALL_ARG_LEN: usize = 2;
/// First inline IPC payload register lane in the stable cross-arch syscall ABI.
pub const SYSCALL_ARG_INLINE_PAYLOAD0: usize = 3;
/// Second inline IPC payload register lane in the stable cross-arch syscall ABI.
pub const SYSCALL_ARG_INLINE_PAYLOAD1: usize = 4;
/// Transfer-cap send may bind to a known waiting receiver when available, otherwise
/// envelope materialization is validated at receive time against endpoint and receiver.
pub const SYSCALL_ARG_TRANSFER_CAP: usize = syscall_abi::TRAPFRAME_ARG_REGS - 1;
pub const SYSCALL_RET_STATUS: usize = 0;
pub const SYSCALL_RET_AUX: usize = 1;
pub const SYSCALL_RET_TRANSFER_CAP: usize = 2;
pub const SYSCALL_NO_TRANSFER_CAP: u64 = Message::NO_TRANSFER_CAP;
pub const SYSCALL_VM_MAP_PROT_READ: usize = 0x1;
pub const SYSCALL_VM_MAP_PROT_WRITE: usize = 0x2;
pub const SYSCALL_VM_MAP_PROT_EXEC: usize = 0x4;
pub const SYSCALL_RECV_MAP_INTENT_READ: usize = 0x1;
pub const SYSCALL_RECV_MAP_INTENT_WRITE: usize = 0x2;
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
    ControlPlaneSetCnodeSlots = SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR,
    FutexWait = SYSCALL_FUTEX_WAIT_NR,
    FutexWake = SYSCALL_FUTEX_WAKE_NR,
    SpawnThread = SYSCALL_SPAWN_THREAD_NR,
    Fork = SYSCALL_FORK_NR,
    VmAnonMap = SYSCALL_VM_ANON_MAP_NR,
    VmBrk = SYSCALL_VM_BRK_NR,
    DebugLog = SYSCALL_DEBUG_LOG_NR,
    SpawnProcess = SYSCALL_SPAWN_PROCESS_NR,
}

impl Syscall {
    pub const VARIANT_COUNT: usize = 17;
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
            SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR => Ok(Self::ControlPlaneSetCnodeSlots),
            SYSCALL_FUTEX_WAIT_NR => Ok(Self::FutexWait),
            SYSCALL_FUTEX_WAKE_NR => Ok(Self::FutexWake),
            SYSCALL_SPAWN_THREAD_NR => Ok(Self::SpawnThread),
            SYSCALL_FORK_NR => Ok(Self::Fork),
            SYSCALL_VM_ANON_MAP_NR => Ok(Self::VmAnonMap),
            SYSCALL_VM_BRK_NR => Ok(Self::VmBrk),
            SYSCALL_DEBUG_LOG_NR => Ok(Self::DebugLog),
            SYSCALL_SPAWN_PROCESS_NR => Ok(Self::SpawnProcess),
            _ => Err(SyscallError::InvalidNumber),
        }
    }
}

const _: () = assert!(SYSCALL_SPAWN_PROCESS_NR < SYSCALL_COUNT);
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

fn decode_ipc_send_timeout_ticks(frame: &TrapFrame) -> u64 {
    frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1) as u64
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
    let tid = kernel.current_tid().unwrap_or(0);
    let cnode = kernel.current_task_cnode();
    let slot_result = cnode.and_then(|cn| kernel.capability_for_cnode_local(cn, cap));
    let live_result = slot_result.and_then(|c| kernel.capability_object_live(c.object).map(|_| c));
    crate::yarm_log!(
        "CAP_LOOKUP tid={} cap={} cnode={} slot_found={} object_live={} type={:?} rights={:?}",
        tid,
        cap.0,
        cnode.map(|c| c.0).unwrap_or(u64::MAX),
        slot_result.is_some(),
        live_result.is_some(),
        live_result.map(|c| c.object),
        live_result.map(|c| c.rights()),
    );
    let endpoint_cap = live_result.ok_or(SyscallError::InvalidCapability)?;
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
    if !capability.has_right(CapRights::READ) || !capability.has_right(CapRights::MAP) {
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
    let receiver_tid = kernel.endpoint_waiter_tid(endpoint);
    Ok(Some(
        kernel
            .stash_transfer_envelope(
                crate::kernel::ipc::ThreadId(sender_tid),
                source_cap_id,
                endpoint,
                receiver_tid,
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
    map_flags: PageFlags,
) -> Result<(usize, usize), SyscallError> {
    if requested_va == 0 || region_len == 0 || !requested_va.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    let mapped_len = round_up_page(region_len)?;
    let mut va = requested_va;
    let end = requested_va
        .checked_add(mapped_len)
        .ok_or(SyscallError::InvalidArgs)?;
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

fn attenuate_transfer_cap_for_recv_intent(
    kernel: &mut KernelState,
    transfer_cap: CapId,
    allow_write: bool,
) -> Result<CapId, SyscallError> {
    if allow_write {
        return Ok(transfer_cap);
    }
    let capability = kernel
        .capability_service()
        .resolve_current_task_capability(transfer_cap)
        .ok_or(SyscallError::InvalidCapability)?;
    let desired = CapRights::READ | CapRights::MAP;
    if capability.rights().contains(desired) && !capability.rights().contains(CapRights::WRITE) {
        return Ok(transfer_cap);
    }
    let attenuated_rights = capability.rights().intersect(desired);
    let derived = kernel
        .mint_capability_for_current_context(crate::kernel::capabilities::Capability::new(
            capability.object,
            attenuated_rights,
        ))
        .map_err(SyscallError::from)?;
    revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
    Ok(derived)
}

fn recv_shared_mem_map_intent_flags(frame: &TrapFrame) -> Result<PageFlags, SyscallError> {
    let raw = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1);
    if raw == 0 {
        return Ok(PageFlags {
            read: true,
            write: true,
            execute: false,
            user: true,
            cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
        });
    }
    let unknown = raw & !(SYSCALL_RECV_MAP_INTENT_READ | SYSCALL_RECV_MAP_INTENT_WRITE);
    if unknown != 0 || (raw & SYSCALL_RECV_MAP_INTENT_READ) == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(PageFlags {
        read: true,
        write: (raw & SYSCALL_RECV_MAP_INTENT_WRITE) != 0,
        execute: false,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    })
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

    let sender_has_user_asid = current_task_has_user_asid(kernel)?;
    let send_timeout_ticks = if sender_has_user_asid || len == 0 {
        decode_ipc_send_timeout_ticks(frame)
    } else {
        0
    };

    let msg_result = if sender_has_user_asid {
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

    let send_result = if send_timeout_ticks == 0 {
        kernel.ipc_send(cap, msg)
    } else {
        kernel.ipc_send_with_deadline(cap, msg, send_timeout_ticks)
    };
    if let Err(err) = send_result {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {
            let _ = kernel.take_transfer_envelope(
                handle,
                endpoint,
                crate::kernel::ipc::ThreadId(current_tid(kernel)?),
            );
        }
        if err == KernelError::WouldBlock && send_timeout_ticks != 0 {
            let timed_out = kernel
                .consume_ipc_timeout_fired_for_tid(sender_tid)
                .map_err(SyscallError::from)?;
            if timed_out {
                return Err(SyscallError::TimedOut);
            }
            let still_blocked = matches!(
                kernel.task_status(sender_tid),
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                ))
            );
            if !still_blocked {
                frame.set_ok(0, 0, 0);
                encode_transfer_cap_ret(frame, None)?;
                return Ok(());
            }
        }
        return Err(SyscallError::from(err));
    }
    frame.set_ok(0, 0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_recv(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let recv_tid = kernel.current_tid().unwrap_or(0);
    crate::yarm_log!("IPC_RECV_ENTER tid={} cap={}", recv_tid, cap.0);
    if let Err(e) = validate_endpoint_right(kernel, cap, CapRights::RECEIVE) {
        crate::yarm_log!(
            "IPC_RECV_CAP_LOOKUP_FAIL tid={} cap={} reason={:?}",
            recv_tid,
            cap.0,
            e
        );
        return Err(e);
    }
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;
    if received.is_none() {
        return Err(SyscallError::WouldBlock);
    }
    crate::yarm_log!(
        "IPC_RECV_GOT_MSG tid={} cap={} transfer_cap={}",
        recv_tid,
        cap.0,
        received.as_ref().and_then(|m| m.transferred_cap()).map(|c| c.0).unwrap_or(u64::MAX)
    );
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
    let sender_tid = current_tid(kernel)?;
    crate::yarm_log!(
        "IPC_CALL_BEGIN tid={} send_cap={} reply_cap={}",
        sender_tid,
        cap.0,
        reply_recv_cap.0
    );
    let responder_tid = kernel.endpoint_waiter_tid(endpoint);
    let endpoint_idx = kernel
        .resolve_endpoint_index(endpoint)
        .map_err(SyscallError::from)?;
    if let Some(waiter_tid) = responder_tid {
        crate::yarm_log!(
            "IPC_CALL_WAKE_RECEIVER endpoint={} tid={}",
            endpoint_idx,
            waiter_tid.0
        );
    }

    let reply_cap = kernel
        .create_reply_cap_for_caller(
            crate::kernel::ipc::ThreadId(sender_tid),
            reply_recv_cap,
            responder_tid,
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
    crate::yarm_log!(
        "IPC_CALL_SENT_OR_QUEUED tid={} endpoint={}",
        sender_tid,
        endpoint_idx
    );
    let received = kernel
        .ipc_recv(reply_recv_cap)
        .map_err(SyscallError::from)?;
    if let Some(reply) = received {
        let reply_sender = sender_tid_to_ret(reply.sender_tid.0)?;
        frame.set_ok(reply_sender, reply.len as usize, 0);
        let words =
            pack_register_payload(reply.as_slice()).map_err(|_| SyscallError::InvalidArgs)?;
        frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD0, words[0]);
        frame.set_arg(SYSCALL_ARG_INLINE_PAYLOAD1, words[1]);
        encode_transfer_cap_ret(frame, None)?;
    } else {
        crate::yarm_log!(
            "IPC_CALL_BLOCK_ON_REPLY tid={} reply_endpoint={} saved_elr=na",
            sender_tid,
            reply_recv_cap.0
        );
        return Err(SyscallError::WouldBlock);
    }
    Ok(())
}

fn handle_ipc_reply(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let reply_cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let len = frame.arg(SYSCALL_ARG_LEN);
    let sender_tid = current_tid(kernel)?;
    crate::yarm_log!(
        "IPC_REPLY_ENTER tid={} reply_cap={} len={}",
        sender_tid,
        reply_cap.0,
        len
    );
    let cnode = kernel.current_task_cnode();
    let slot_result = cnode.and_then(|cn| kernel.capability_for_cnode_local(cn, reply_cap));
    let live_result = slot_result.and_then(|c| kernel.capability_object_live(c.object).map(|_| c));
    crate::yarm_log!(
        "IPC_REPLY_CAP_PROBE tid={} cap={} cnode={} slot_found={} object_live={} object={:?} rights={:?}",
        sender_tid,
        reply_cap.0,
        cnode.map(|c| c.0).unwrap_or(u64::MAX),
        slot_result.is_some(),
        live_result.is_some(),
        live_result.map(|c| c.object),
        live_result.map(|c| c.rights()),
    );
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
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
    if let Err(err) = kernel.ipc_reply(reply_cap, msg) {
        let mapped = SyscallError::from(err);
        crate::yarm_log!(
            "IPC_REPLY_FAIL tid={} reply_cap={} err={:?}",
            sender_tid,
            reply_cap.0,
            mapped
        );
        return Err(mapped);
    }
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
                    let recv_map_flags = match recv_shared_mem_map_intent_flags(frame) {
                        Ok(flags) => flags,
                        Err(err) => {
                            revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                            encode_transfer_cap_ret(frame, None)?;
                            return Err(err);
                        }
                    };
                    let transfer_capability = kernel
                        .capability_service()
                        .resolve_current_task_capability(transfer_cap)
                        .ok_or(SyscallError::InvalidCapability)?;
                    if recv_map_flags.write && !transfer_capability.has_right(CapRights::WRITE) {
                        revoke_current_transfer_cap_best_effort(kernel, transfer_cap);
                        encode_transfer_cap_ret(frame, None)?;
                        return Err(SyscallError::MissingRight);
                    }
                    let transfer_cap = attenuate_transfer_cap_for_recv_intent(
                        kernel,
                        transfer_cap,
                        recv_map_flags.write,
                    )?;
                    if transfer_cap.0 != transfer_cap_raw {
                        encode_transfer_cap_ret(frame, Some(transfer_cap.0))?;
                    }
                    let (mapped_va, mapped_len) = match map_shared_region_into_receiver(
                        kernel,
                        transfer_cap,
                        user_ptr,
                        region_len,
                        recv_map_flags,
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
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_RECV_COPY_TO_USER tid={} dst=0x{:x} len={} result=ok",
                            receiver_tid,
                            user_ptr,
                            msg.len
                        );
                        frame.set_ok(sender, msg.len as usize, frame.ret2());
                    }
                    Err(KernelError::UserMemoryFault) => {
                        crate::yarm_log!(
                            "IPC_RECV_COPY_TO_USER tid={} dst=0x{:x} len={} result=err",
                            receiver_tid,
                            user_ptr,
                            msg.len
                        );
                        record_user_fault(kernel, frame, user_ptr, FaultAccess::Write);
                        return Ok(());
                    }
                    Err(other) => return Err(SyscallError::from(other)),
                };
            } else {
                frame.set_ok(sender, msg.len as usize, frame.ret2());
                crate::yarm_log!(
                    "IPC_RECV_WAKE_RETURN_REGS tid={} x0={} x1={} x2={} elr=na",
                    receiver_tid,
                    sender,
                    msg.len,
                    frame.ret2()
                );
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

    // Reserve one unmapped page immediately below writable non-executable
    // mappings so downward-growing task stacks can fault deterministically
    // on overflow instead of silently corrupting adjacent pages.
    if flags.write
        && !flags.execute
        && let Some(guard_page) = addr.checked_sub(PAGE_SIZE)
        && kernel
            .is_user_page_mapped_in_current_asid(VirtAddr(guard_page as u64))
            .map_err(SyscallError::from)?
    {
        return Err(SyscallError::InvalidArgs);
    }

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

fn handle_control_plane_set_cnode_slots(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let requester_tid = current_tid(kernel)?;
    let target_pid = frame.arg(SYSCALL_ARG_CAP) as u64;
    let slot_capacity = frame.arg(SYSCALL_ARG_PTR);
    if target_pid == 0 || slot_capacity == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    kernel
        .control_plane_set_process_cnode_slots(requester_tid, target_pid, slot_capacity)
        .map_err(SyscallError::from)?;
    frame.set_ok(slot_capacity, target_pid as usize, 0);
    Ok(())
}

fn handle_futex_wait(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let expected =
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let observed =
        u32::try_from(frame.arg(SYSCALL_ARG_LEN)).map_err(|_| SyscallError::InvalidArgs)?;
    let blocked = kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(SyscallError::from)?;
    frame.set_ok(usize::from(blocked), 0, 0);
    Ok(())
}

fn handle_futex_wake(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let addr = frame.arg(SYSCALL_ARG_CAP);
    let max_wake =
        u32::try_from(frame.arg(SYSCALL_ARG_PTR)).map_err(|_| SyscallError::InvalidArgs)?;
    let woke = kernel
        .futex_wake(addr, max_wake)
        .map_err(SyscallError::from)?;
    frame.set_ok(woke as usize, 0, 0);
    Ok(())
}

fn handle_spawn_thread(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let parent_tid = current_tid(kernel)?;
    let tls_base = frame.arg(SYSCALL_ARG_CAP);
    let user_stack_top = frame.arg(SYSCALL_ARG_PTR);
    let user_entry = frame.arg(SYSCALL_ARG_LEN);
    let tid = kernel
        .spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)
        .map_err(SyscallError::from)?;
    frame.set_ok(
        usize::try_from(tid).map_err(|_| SyscallError::Internal)?,
        0,
        0,
    );
    Ok(())
}

fn handle_fork(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let parent_tid = current_tid(kernel)?;
    let child_tid = kernel
        .fork_user_process_cow(parent_tid)
        .map_err(SyscallError::from)?;
    frame.set_ok(
        usize::try_from(child_tid).map_err(|_| SyscallError::Internal)?,
        0,
        0,
    );
    Ok(())
}

fn handle_spawn_process(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let image_id = frame.arg(SYSCALL_ARG_CAP) as u64;
    let parent_pid = frame.arg(SYSCALL_ARG_PTR) as u64;
    let startup_args_ptr = frame.arg(SYSCALL_ARG_LEN);
    let startup_args_count = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    crate::yarm_log!(
        "KSPAWN_ENTER image_id={} parent_pid={} args_count={}",
        image_id,
        parent_pid,
        startup_args_count
    );
    let startup_args = copy_spawn_startup_args(kernel, startup_args_ptr, startup_args_count)?;
    let image_path = spawn_image_path_for_image_id(image_id).ok_or(SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_PATH path={}", image_path);
    let initrd = crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = CpioArchive::new(initrd)
        .find(image_path)
        .map_err(|_| SyscallError::InvalidArgs)?
        .ok_or(SyscallError::InvalidArgs)?;
    let elf_bytes = entry.file_data();
    crate::yarm_log!("KSPAWN_ELF_FOUND size={}", elf_bytes.len());
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_ELF_PARSED entry={}", elf.entry);
    let tid = kernel.allocate_thread_id().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=allocate_tid err={:?}", err);
        SyscallError::from(err)
    })?;
    let (asid, _aspace_cap) = kernel.create_user_address_space().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=create_asid err={:?}", err);
        SyscallError::from(err)
    })?;
    crate::yarm_log!("KSPAWN_ASID_OK tid={} asid={}", tid, asid.0);
    kernel.load_elf_pt_load_segments(asid, elf_bytes).map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=load_elf err={:?}", err);
        SyscallError::from(err)
    })?;
    crate::yarm_log!("KSPAWN_LOAD_OK tid={}", tid);
    let spawned = kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid,
            entry: elf.entry as usize,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args,
        })
        .map_err(|err| {
            crate::yarm_log!("KSPAWN_FAIL phase=spawn_task err={:?}", err);
            SyscallError::from(err)
        })?;
    crate::yarm_log!("KSPAWN_TASK_READY tid={}", spawned.tid);
    frame.set_ok(
        0,
        usize::try_from(spawned.tid).map_err(|_| SyscallError::Internal)?,
        0,
    );
    Ok(())
}

fn spawn_image_path_for_image_id(image_id: u64) -> Option<&'static str> {
    match image_id {
        0 => Some("init"),
        1 => Some("sbin/supervisor"),
        2 => Some("sbin/process_manager"),
        3 => Some("sbin/init_server"),
        4 => Some("sbin/initramfs_srv"),
        _ => None,
    }
}

fn copy_spawn_startup_args(
    kernel: &KernelState,
    startup_args_ptr: usize,
    startup_args_count: usize,
) -> Result<[u64; UserImageSpec::DEFAULT_STARTUP_ARGS.len()], SyscallError> {
    let mut out = UserImageSpec::DEFAULT_STARTUP_ARGS;
    if startup_args_count == 0 {
        return Ok(out);
    }
    if startup_args_count > out.len() || startup_args_ptr == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let byte_len = startup_args_count
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(SyscallError::InvalidArgs)?;
    validate_user_region(startup_args_ptr as u64, byte_len as u64)?;
    let payload = kernel
        .copy_from_current_user(startup_args_ptr, byte_len)
        .map_err(SyscallError::from)?;
    for (idx, chunk) in payload[..byte_len].chunks_exact(core::mem::size_of::<u64>()).enumerate() {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        out[idx] = u64::from_le_bytes(word);
    }
    Ok(out)
}

fn handle_vm_anon_map(
    _kernel: &mut KernelState,
    _frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    Err(SyscallError::InvalidArgs)
}

fn handle_vm_brk(_kernel: &mut KernelState, _frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let tid = current_tid(_kernel)?;
    let leader_tid = _kernel
        .thread_group_id(tid)
        .map(|group| group.0)
        .ok_or(SyscallError::InvalidArgs)?;
    if tid != leader_tid {
        return Err(SyscallError::InvalidArgs);
    }

    let requested = _frame.arg(SYSCALL_ARG_CAP);
    if requested == 0 {
        let current_end = _kernel
            .task_brk_bounds(tid)
            .map(|(_, end)| end)
            .unwrap_or(0);
        _frame.set_ok(current_end, 0, 0);
        return Ok(());
    }

    validate_user_region(requested as u64, 1)?;
    let (base, current_end) = _kernel
        .task_brk_bounds(tid)
        .ok_or(SyscallError::InvalidArgs)?;
    if requested < base {
        return Err(SyscallError::InvalidArgs);
    }
    if requested < current_end {
        return Err(SyscallError::InvalidArgs);
    }

    // Staged VM_BRK behavior: query + grow only, tracked per-task. Grow
    // requires pre-initialized brk bounds to avoid creating an empty
    // [base,end) window from unset state. Heap pages are still allocated
    // lazily by demand-fault mapping in [base, end).
    _kernel
        .set_task_brk_bounds(tid, base, requested)
        .map_err(SyscallError::from)?;
    _frame.set_ok(requested, 0, 0);
    Ok(())
}

fn handle_debug_log(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    // ABI: arg0=ptr, arg1=len (no cap slot; do not use SYSCALL_ARG_PTR/LEN here).
    let a0 = frame.arg(0);
    let a1 = frame.arg(1);
    let a2 = frame.arg(2);
    let tid = kernel.current_tid().unwrap_or(0);
    crate::yarm_log!("DEBUG_LOG_ARGS tid={} a0=0x{:x} a1=0x{:x} a2=0x{:x}", tid, a0, a1, a2);
    let user_ptr = a0;
    let raw_len = a1;
    let len = raw_len.min(Message::MAX_PAYLOAD);
    crate::yarm_log!("DEBUG_LOG_ENTER tid={} ptr=0x{:x} len={}", tid, user_ptr, raw_len);
    if user_ptr == 0 || len == 0 {
        frame.set_ok(0, 0, 0);
        return Ok(());
    }
    let payload = match kernel.copy_from_current_user(user_ptr, len) {
        Ok(data) => data,
        Err(e) => {
            crate::yarm_log!("DEBUG_LOG_COPY_FAIL tid={} err={:?}", tid, e);
            frame.set_ok(0, 0, 0);
            return Ok(());
        }
    };
    crate::yarm_log!("DEBUG_LOG_COPY_OK tid={} len={}", tid, len);
    let msg_str = core::str::from_utf8(&payload[..len]).unwrap_or("<utf8_err>");
    crate::yarm_log!("USER_LOG tid={} msg={}", tid, msg_str);
    frame.set_ok(0, 0, 0);
    Ok(())
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if frame.syscall_num() == SYSCALL_YIELD_NR {
        let tid = kernel.current_tid().unwrap_or(0);
        crate::yarm_log!(
            "YARM_SYSCALL0_ENTER tid={} nr={} x0={} x1={} x2={}",
            tid,
            frame.syscall_num(),
            frame.arg(0),
            frame.arg(1),
            frame.arg(2)
        );
    }
    let syscall = Syscall::decode(frame.syscall_num())?;
    let caller_tid = kernel.current_tid();
    let result = match syscall {
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
        Syscall::ControlPlaneSetCnodeSlots => handle_control_plane_set_cnode_slots(kernel, frame),
        Syscall::VmMap => handle_vm_map(kernel, frame),
        Syscall::TransferRelease => handle_transfer_release(kernel, frame),
        Syscall::FutexWait => handle_futex_wait(kernel, frame),
        Syscall::FutexWake => handle_futex_wake(kernel, frame),
        Syscall::SpawnThread => handle_spawn_thread(kernel, frame),
        Syscall::Fork => handle_fork(kernel, frame),
        Syscall::VmAnonMap => handle_vm_anon_map(kernel, frame),
        Syscall::VmBrk => handle_vm_brk(kernel, frame),
        Syscall::DebugLog => handle_debug_log(kernel, frame),
        Syscall::SpawnProcess => handle_spawn_process(kernel, frame),
    };
    if result == Err(SyscallError::WouldBlock) {
        let caller_status = caller_tid.and_then(|tid| kernel.task_status(tid));
        let caller_blocked = matches!(
            caller_status,
            Some(crate::kernel::task::TaskStatus::Blocked(
                crate::kernel::task::WaitReason::EndpointSend(_)
                    | crate::kernel::task::WaitReason::EndpointReceive(_)
            ))
        );
        let blocking_syscall = match syscall {
            Syscall::IpcRecv | Syscall::IpcCall => true,
            Syscall::IpcSend => decode_ipc_send_timeout_ticks(frame) == 0,
            _ => false,
        };
        crate::yarm_log!(
            "BLOCKED_WOULDBLOCK_CLASSIFY tid={} nr={} status={:?} nonfatal={}",
            caller_tid.unwrap_or(0),
            frame.syscall_num(),
            caller_status,
            blocking_syscall && caller_blocked
        );
        if blocking_syscall && caller_blocked {
            // For IpcRecv/IpcRecvTimeout: mark the frame with WouldBlock so the
            // ELR policy saves saved_pc = SVC (not SVC+4). When the task is woken
            // it will re-execute the SVC, find the message in the queue, and receive
            // the result correctly in x0/x1/x2. Without this, saved_pc = SVC+4 and
            // the task resumes past the SVC with the original args still in registers.
            if syscall == Syscall::IpcRecv {
                frame.set_err(SyscallError::WouldBlock.code());
                crate::yarm_log!(
                    "IPC_RECV_BLOCKED_RETRY_SAVE tid={} nr={}",
                    caller_tid.unwrap_or(0),
                    frame.syscall_num()
                );
            }
            if kernel.current_tid() == caller_tid {
                let _ = kernel.dispatch_next_task().map_err(SyscallError::from)?;
            }
            crate::yarm_log!(
                "AARCH64_BLOCKED_RETURN_DISPATCH trapped_tid={} next_tid={}",
                caller_tid.unwrap_or(0),
                kernel.current_tid().unwrap_or(0)
            );
            crate::yarm_log!(
                "AARCH64_SYSCALL_BLOCKED_OK tid={} nr={}",
                caller_tid.unwrap_or(0),
                frame.syscall_num()
            );
            crate::yarm_log!(
                "AARCH64_BLOCKED_SYSCALL_STAYS_BLOCKED tid={} nr={}",
                caller_tid.unwrap_or(0),
                frame.syscall_num()
            );
            crate::yarm_log!("AARCH64_TRAP_DISPATCH_RESULT blocked");
            return Ok(());
        }
        crate::yarm_log!(
            "BLOCKED_WOULDBLOCK_FATAL tid={} nr={} status={:?} reason={}",
            caller_tid.unwrap_or(0),
            frame.syscall_num(),
            caller_status,
            if !blocking_syscall {
                "non_blocking_syscall"
            } else {
                "caller_not_blocked"
            }
        );
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if frame.syscall_num() == SYSCALL_YIELD_NR {
        let trapped_tid = caller_tid.unwrap_or(0);
        let next_tid = kernel.current_tid().unwrap_or(0);
        if let Some(code) = frame.error_code() {
            crate::yarm_log!(
                "YARM_SYSCALL0_EXIT trapped_tid={} next_tid={} nr={} result=err code={}",
                trapped_tid,
                next_tid,
                frame.syscall_num(),
                code
            );
        } else {
            crate::yarm_log!(
                "YARM_SYSCALL0_EXIT trapped_tid={} next_tid={} nr={} result=ok r0={} r1={} r2={}",
                trapped_tid,
                next_tid,
                frame.syscall_num(),
                frame.ret0(),
                frame.ret1(),
                frame.ret2()
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{boxed::Box, format, vec::Vec};
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::ipc::{EndpointMode, IPC_REGISTER_WORDS};
    use crate::kernel::scheduler_timer::Timer;
    use crate::kernel::trapframe::TrapFrame;

    fn push_cpio_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let namesz = name.len() + 1;
        let mut h = [0u8; 110];
        h[0..6].copy_from_slice(b"070701");
        h[14..22].copy_from_slice(format!("{mode:08x}").as_bytes());
        h[54..62].copy_from_slice(format!("{:08x}", data.len()).as_bytes());
        h[94..102].copy_from_slice(format!("{namesz:08x}").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn synthetic_elf_image(image_id: u64) -> [u8; 128] {
        let mut image = [0u8; 128];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        image[16..18].copy_from_slice(&2u16.to_le_bytes());
        image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes());
        let entry = 0x400000u64.saturating_add(image_id.saturating_mul(0x1000));
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes());
        image[52..54].copy_from_slice(&(64u16).to_le_bytes());
        image[54..56].copy_from_slice(&(56u16).to_le_bytes());
        image[56..58].copy_from_slice(&(1u16).to_le_bytes());
        let ph = 64usize;
        image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
        image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes());
        image[ph + 8..ph + 16].copy_from_slice(&120u64.to_le_bytes());
        image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes());
        image[ph + 32..ph + 40].copy_from_slice(&8u64.to_le_bytes());
        image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes());
        image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        image[120..128].copy_from_slice(&[0x90; 8]);
        image
    }

    #[test]
    fn syscall_abi_numbers_are_frozen() {
        assert_eq!(SYSCALL_ABI_VERSION, 10);
        assert_eq!(SYSCALL_ARG_TRANSFER_CAP, 5);
        assert_eq!(SYSCALL_RET_TRANSFER_CAP, 2);
        assert_eq!(SYSCALL_TRANSFER_RELEASE_NR, 4);
        assert_eq!(SYSCALL_IPC_RECV_TIMEOUT_NR, 5);
        assert_eq!(SYSCALL_IPC_CALL_NR, 6);
        assert_eq!(SYSCALL_IPC_REPLY_NR, 7);
        assert_eq!(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR, 8);
        assert_eq!(SYSCALL_FUTEX_WAIT_NR, 9);
        assert_eq!(SYSCALL_FUTEX_WAKE_NR, 10);
        assert_eq!(SYSCALL_SPAWN_THREAD_NR, 11);
        assert_eq!(SYSCALL_FORK_NR, 12);
        assert_eq!(SYSCALL_VM_ANON_MAP_NR, 13);
        assert_eq!(SYSCALL_VM_BRK_NR, 14);
        assert_eq!(SYSCALL_SPAWN_PROCESS_NR, 23);
        assert_eq!(SYSCALL_COUNT, 24);
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
    fn spawn_process_rejects_startup_arg_count_overflow() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(
            Syscall::SpawnProcess as usize,
            [4, 1, 0, UserImageSpec::DEFAULT_STARTUP_ARGS.len() + 1, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect_err("reject overflow count");
    }

    #[test]
    fn spawn_process_rejects_missing_cpio_image() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut cpio = Vec::new();
        push_cpio_entry(&mut cpio, "init", 0o100755, &synthetic_elf_image(0));
        push_cpio_entry(&mut cpio, "TRAILER!!!", 0, &[]);
        let bytes: &'static [u8] = Box::leak(cpio.into_boxed_slice());
        crate::kernel::boot::Bootstrap::install_boot_initrd_bytes(bytes);
        let mut frame = TrapFrame::new(Syscall::SpawnProcess as usize, [4, 1, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect_err("missing image path");
    }

    #[test]
    fn syscall_control_plane_set_cnode_slots_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR).expect("decode"),
            Syscall::ControlPlaneSetCnodeSlots
        );
    }

    #[test]
    fn syscall_vm_brk_query_unset_returns_zero() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("vm brk query");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0);
    }

    #[test]
    fn syscall_vm_brk_query_returns_existing_end() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .set_task_brk_bounds(0, 0x4000, 0x8000)
            .expect("set brk");
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("vm brk query");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0x8000);
    }

    #[test]
    fn syscall_vm_brk_grow_unset_is_rejected() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0x9000, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect_err("vm brk grow unset rejected");
    }

    #[test]
    fn syscall_vm_brk_grow_updates_end() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .set_task_brk_bounds(0, 0x4000, 0x8000)
            .expect("set brk");
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0x9000, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("vm brk grow");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0x9000);
        assert_eq!(state.task_brk_bounds(0), Some((0x4000, 0x9000)));
    }

    #[test]
    fn syscall_vm_brk_shrink_is_rejected() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .set_task_brk_bounds(0, 0x4000, 0x8000)
            .expect("set brk");
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0x7000, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect_err("vm brk shrink rejected");
    }

    #[test]
    fn syscall_vm_brk_rejects_kernel_address() {
        let mut state = Bootstrap::init().expect("kernel");
        let kernel_addr = crate::kernel::vm::KERNEL_SPACE_BASE as usize;
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [kernel_addr, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect_err("vm brk kernel addr rejected");
    }

    #[test]
    fn syscall_vm_brk_rejects_non_leader_thread() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(crate::kernel::boot::UserImageSpec {
                tid: 41,
                entry: 0x4000,
                asid: Some(asid),
                class: crate::kernel::task::TaskClass::App,
                startup_args: crate::kernel::boot::UserImageSpec::DEFAULT_STARTUP_ARGS,
            })
            .expect("leader");
        state
            .set_task_brk_bounds(41, 0x4000, 0x8000)
            .expect("brk bounds");
        let child_tid = state
            .spawn_user_thread(41, 0xABCD_0000, 0x8800_0000, 0x4010)
            .expect("thread");
        state.yield_current().expect("switch");
        assert_eq!(state.current_tid(), Some(child_tid));
        let mut frame = TrapFrame::new(Syscall::VmBrk as usize, [0x9000, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect_err("non-leader rejected");
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
    fn syscall_send_timeout_marks_blocked_sender_after_deadline_tick() {
        let mut state = Bootstrap::init().expect("kernel");
        state.set_timer_for_test(Timer::new(1));
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap_global, _recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");
        let send_cap = state
            .grant_capability_task_to_task(0, send_cap_global, 1)
            .expect("dup send cap");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert!(
            state
                .capability_service()
                .current_task_capability_has_right(send_cap, CapRights::SEND),
            "task1 must hold send right"
        );

        let mut frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0,
                0,
                0,
                1,
                SYSCALL_NO_TRANSFER_CAP as usize,
            ],
        );
        let err = dispatch(&mut state, &mut frame).expect_err("blocked send");
        assert_eq!(err, SyscallError::WouldBlock);
        assert_eq!(
            state.task_status(1),
            Some(crate::kernel::task::TaskStatus::Blocked(
                crate::kernel::task::WaitReason::EndpointSend(send_cap)
            ))
        );
        assert!(
            !state
                .consume_ipc_timeout_fired_for_tid(1)
                .expect("pre-tick timeout marker"),
            "timeout marker must not fire before timer progression"
        );

        state
            .handle_trap(crate::kernel::trap::Trap::TimerInterrupt, None)
            .expect("timer trap");
        assert!(
            state
                .consume_ipc_timeout_fired_for_tid(1)
                .expect("consume timeout marker"),
            "send timeout marker should fire after deadline"
        );
        assert!(matches!(
            state.task_status(1),
            Some(
                crate::kernel::task::TaskStatus::Runnable
                    | crate::kernel::task::TaskStatus::Running
            )
        ));
    }

    #[test]
    fn syscall_send_with_timeout_succeeds_when_receiver_waiting() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("receiver");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap_global, recv_cap_global) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");
        state
            .grant_capability_task_to_task(0, send_cap_global, 1)
            .expect("dup send cap");
        let recv_cap = state
            .grant_capability_task_to_task(0, recv_cap_global, 1)
            .expect("dup recv cap");
        state.yield_current().expect("switch to receiver");
        assert_eq!(state.current_tid(), Some(1));

        let mut recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv_frame).expect("block receiver");
        assert_eq!(state.current_tid(), Some(0));

        let mut send_frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap_global.0 as usize,
                0,
                0,
                0,
                5,
                SYSCALL_NO_TRANSFER_CAP as usize,
            ],
        );
        dispatch(&mut state, &mut send_frame).expect("send before timeout");
        assert_eq!(send_frame.error_code(), None);
    }

    #[test]
    fn blocking_ipc_send_dispatch_switches_away_without_userspace_resume() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap_global, _recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");
        let send_cap = state
            .grant_capability_task_to_task(0, send_cap_global, 1)
            .expect("dup send cap");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));

        let mut frame = TrapFrame::new(
            Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0,
                0,
                0,
                0,
                SYSCALL_NO_TRANSFER_CAP as usize,
            ],
        );
        dispatch(&mut state, &mut frame).expect("blocking send consumed by dispatch");
        assert_eq!(
            state.task_status(1),
            Some(crate::kernel::task::TaskStatus::Blocked(
                crate::kernel::task::WaitReason::EndpointSend(send_cap)
            ))
        );
        assert_ne!(state.current_tid(), Some(1));
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
    fn blocking_ipc_call_dispatch_switches_away_while_waiting_for_reply() {
        let mut state = Bootstrap::init().expect("kernel");
        state.register_task(1).expect("server");
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

        let mut call = TrapFrame::new(
            Syscall::IpcCall as usize,
            [
                call_send_cap.0 as usize,
                0,
                0,
                0,
                0,
                reply_recv_cap.0 as usize,
            ],
        );
        dispatch(&mut state, &mut call).expect("blocking call consumed by dispatch");
        assert_eq!(
            state.task_status(0),
            Some(crate::kernel::task::TaskStatus::Blocked(
                crate::kernel::task::WaitReason::EndpointReceive(reply_recv_cap)
            ))
        );
        assert_ne!(state.current_tid(), Some(0));
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
    fn vm_map_writable_region_requires_unmapped_guard_page_below_base() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");

        let mut first = TrapFrame::new(
            Syscall::VmMap as usize,
            [
                aspace_map_cap.0 as usize,
                0x3000,
                PAGE_SIZE,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut first).expect("first map");
        assert_eq!(first.error_code(), None);

        let mut second = TrapFrame::new(
            Syscall::VmMap as usize,
            [
                aspace_map_cap.0 as usize,
                0x4000,
                PAGE_SIZE,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
            ],
        );
        let err = dispatch(&mut state, &mut second).expect_err("guard conflict");
        assert_eq!(err, SyscallError::InvalidArgs);
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
    fn syscall_recv_shared_mem_rejects_invalid_map_intent_flags() {
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
                0xA000,
                Message::MAX_PAYLOAD + 16,
                0,
                0x8,
                0,
            ],
        );
        let err = dispatch(&mut state, &mut recv).expect_err("invalid map intent");
        assert_eq!(err, SyscallError::InvalidArgs);
        assert_eq!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
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
    fn syscall_recv_shared_mem_write_intent_requires_write_right_on_transfer_cap() {
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
        dispatch(&mut state, &mut send).expect("send");
        state.yield_current().expect("switch receiver");

        let mut recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [
                recv_cap.0 as usize,
                0x9200,
                Message::MAX_PAYLOAD + 16,
                0,
                SYSCALL_RECV_MAP_INTENT_READ | SYSCALL_RECV_MAP_INTENT_WRITE,
                0,
            ],
        );
        let err = dispatch(&mut state, &mut recv).expect_err("missing write right");
        assert_eq!(err, SyscallError::MissingRight);
        assert_eq!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
    }

    #[test]
    fn shared_mem_recv_intent_failures_do_not_drift_map_release_telemetry() {
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

        for _ in 0..8 {
            if state.current_tid() != Some(1) {
                state.yield_current().expect("switch receiver");
            }
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
                    0x3000,
                    Message::MAX_PAYLOAD + 32,
                    0,
                    0,
                    mem_cap.0 as usize,
                ],
            );
            dispatch(&mut state, &mut send).expect("send");
            if state.current_tid() != Some(1) {
                state.yield_current().expect("switch receiver");
            }

            let mut recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [
                    recv_cap.0 as usize,
                    0xB000,
                    Message::MAX_PAYLOAD + 32,
                    0,
                    0x80,
                    0,
                ],
            );
            let err = dispatch(&mut state, &mut recv).expect_err("invalid map intent");
            assert_eq!(err, SyscallError::InvalidArgs);
            assert_eq!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
            assert_eq!(state.current_tid(), Some(1));
        }

        let t = state.ipc_path_telemetry();
        assert_eq!(t.shared_mem_bytes_mapped, 0);
        assert_eq!(t.shared_mem_bytes_released, 0);
    }

    #[test]
    fn shared_mem_recv_write_intent_failures_do_not_drift_map_release_telemetry() {
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

        for _ in 0..8 {
            if state.current_tid() != Some(1) {
                state.yield_current().expect("switch receiver");
            }
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
                    0x3000,
                    Message::MAX_PAYLOAD + 32,
                    0,
                    0,
                    no_write_cap.0 as usize,
                ],
            );
            dispatch(&mut state, &mut send).expect("send");
            if state.current_tid() != Some(1) {
                state.yield_current().expect("switch receiver");
            }

            let mut recv = TrapFrame::new(
                Syscall::IpcRecv as usize,
                [
                    recv_cap.0 as usize,
                    0xB000,
                    Message::MAX_PAYLOAD + 32,
                    0,
                    SYSCALL_RECV_MAP_INTENT_READ | SYSCALL_RECV_MAP_INTENT_WRITE,
                    0,
                ],
            );
            let err = dispatch(&mut state, &mut recv).expect_err("missing write right");
            assert_eq!(err, SyscallError::MissingRight);
            assert_eq!(recv.ret2() as u64, SYSCALL_NO_TRANSFER_CAP);
            assert_eq!(state.current_tid(), Some(1));
        }

        let t = state.ipc_path_telemetry();
        assert_eq!(t.shared_mem_bytes_mapped, 0);
        assert_eq!(t.shared_mem_bytes_released, 0);
    }

    #[test]
    fn shared_mem_recv_read_intent_attenuates_receiver_transfer_cap() {
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
                SYSCALL_RECV_MAP_INTENT_READ,
                0,
            ],
        );
        dispatch(&mut state, &mut recv).expect("recv");
        let recv_local = CapId(recv.ret2() as u64);
        let cap = state
            .capability_service()
            .resolve_current_task_capability(recv_local)
            .expect("recv transfer cap");
        assert!(cap.has_right(CapRights::READ));
        assert!(cap.has_right(CapRights::MAP));
        assert!(!cap.has_right(CapRights::WRITE));
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
    fn syscall_control_plane_set_cnode_slots_respects_policy() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .register_task_with_class(230, crate::kernel::task::TaskClass::App)
            .expect("register requester");
        state
            .register_task_with_class(231, crate::kernel::task::TaskClass::App)
            .expect("register target");
        state.enqueue_current_cpu(230).expect("enqueue requester");
        state.dispatch_next_task().expect("dispatch requester");
        if state.current_tid() != Some(230) {
            state.yield_current().expect("switch to requester");
        }

        let mut frame = TrapFrame::new(
            Syscall::ControlPlaneSetCnodeSlots as usize,
            [231, 16, 0, 0, 0, 0],
        );
        let err = dispatch(&mut state, &mut frame).expect_err("policy");
        assert_eq!(err, SyscallError::MissingRight);
    }

    #[test]
    fn syscall_control_plane_set_cnode_slots_allows_system_server_targeting_other_process() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .register_task_with_class(228, crate::kernel::task::TaskClass::SystemServer)
            .expect("register system server");
        state
            .register_task_with_class(229, crate::kernel::task::TaskClass::App)
            .expect("register app");
        let app_cnode = state.process_cnode_for_pid(229).expect("app cnode");
        let before = state.cnode_slot_capacity(app_cnode).expect("slot capacity");
        let requested = before.saturating_add(8);
        state
            .enqueue_current_cpu(228)
            .expect("enqueue system server");
        state.dispatch_next_task().expect("dispatch system server");
        if state.current_tid() != Some(228) {
            state.yield_current().expect("switch to system server");
        }

        let mut frame = TrapFrame::new(
            Syscall::ControlPlaneSetCnodeSlots as usize,
            [229, requested, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("syscall dispatch");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), requested);
        assert_eq!(frame.ret1(), 229);
        assert_eq!(state.cnode_slot_capacity(app_cnode), Some(requested));
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
