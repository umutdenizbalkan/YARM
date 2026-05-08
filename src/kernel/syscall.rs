// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::boot::{KernelError, KernelState, TransferSharedRegion};
use super::capabilities::{CapId, CapObject, CapRights};
use super::ipc::Message;
#[cfg(test)]
use super::ipc::SharedMemoryRegion;
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;
use super::vm::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::arch::syscall_abi;
use yarm_ipc_abi::ipc_v2::{
    IPC_ABI_V2_BLOCK_SIZE, IPC_ABI_V2_VERSION, IPC_V2_FLAG_INLINE_PAYLOAD,
    IPC_V2_FLAG_RECV_COPYOUT, IPC_V2_FLAG_RET_COPYOUT, IPC_V2_FLAG_TRANSFER_CAP,
    IPC_V2_NO_TRANSFER_CAP, IPC_V2_OP_CALL, IPC_V2_OP_RECV, IPC_V2_OP_REPLY, IPC_V2_OP_SEND,
    IpcRegisterBlockV2,
};

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
pub const SYSCALL_IPC_SEND_V2_NR: usize = 15;
pub const SYSCALL_IPC_RECV_V2_NR: usize = 16;
pub const SYSCALL_IPC_CALL_V2_NR: usize = 17;
pub const SYSCALL_IPC_REPLY_V2_NR: usize = 18;
pub const SYSCALL_VM_UNMAP_NR: usize = 19;
pub const SYSCALL_CAP_RELEASE_NR: usize = 20;
pub const SYSCALL_DEBUG_SERIAL_WRITE_NR: usize = 21;
pub const SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR: usize = 22;
pub const SYSCALL_COUNT: usize = 23;
const _: [(); SYSCALL_COUNT] = [(); 23];
pub const DEBUG_SERIAL_WRITE_BUF_MAX_LEN: usize = 256;
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
    IpcSendV2 = SYSCALL_IPC_SEND_V2_NR,
    IpcRecvV2 = SYSCALL_IPC_RECV_V2_NR,
    IpcCallV2 = SYSCALL_IPC_CALL_V2_NR,
    IpcReplyV2 = SYSCALL_IPC_REPLY_V2_NR,
    VmUnmap = SYSCALL_VM_UNMAP_NR,
    CapRelease = SYSCALL_CAP_RELEASE_NR,
    DebugSerialWrite = SYSCALL_DEBUG_SERIAL_WRITE_NR,
    DebugSerialWriteBuf = SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR,
}

impl Syscall {
    pub const VARIANT_COUNT: usize = 23;
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
            SYSCALL_IPC_SEND_V2_NR => Ok(Self::IpcSendV2),
            SYSCALL_IPC_RECV_V2_NR => Ok(Self::IpcRecvV2),
            SYSCALL_IPC_CALL_V2_NR => Ok(Self::IpcCallV2),
            SYSCALL_IPC_REPLY_V2_NR => Ok(Self::IpcReplyV2),
            SYSCALL_VM_UNMAP_NR => Ok(Self::VmUnmap),
            SYSCALL_CAP_RELEASE_NR => Ok(Self::CapRelease),
            SYSCALL_DEBUG_SERIAL_WRITE_NR => Ok(Self::DebugSerialWrite),
            SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR => Ok(Self::DebugSerialWriteBuf),
            _ => Err(SyscallError::InvalidNumber),
        }
    }
}

const _: () = assert!(SYSCALL_COUNT == Syscall::VARIANT_COUNT);
const _: [(); syscall_abi::TRAPFRAME_ARG_REGS] = [(); 6];
const _: () = assert!(SYSCALL_ARG_TRANSFER_CAP < syscall_abi::TRAPFRAME_ARG_REGS);
const _: () = assert!(syscall_abi::TRAPFRAME_ARG_REGS > SYSCALL_ARG_INLINE_PAYLOAD1);
const DEBUG_SERIAL_SYSCALL_ENABLED: bool =
    cfg!(debug_assertions) || cfg!(all(not(feature = "hosted-dev"), target_arch = "aarch64"));

const TRACE_SYSCALL_22: bool = cfg!(feature = "trace-syscall-22");

#[inline]
const fn should_log_syscall_trace(syscall_nr: usize) -> bool {
    syscall_nr != SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR || TRACE_SYSCALL_22
}

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

fn validate_shared_reply_transfer_cap_kind(
    kernel: &KernelState,
    cap: CapId,
) -> Result<u64, SyscallError> {
    let transfer = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?;
    let CapObject::MemoryObject { id } = transfer.object else {
        return Err(SyscallError::WrongObject);
    };
    let mem = kernel
        .with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|entry| entry.id == id)
                .copied()
        })
        .ok_or(SyscallError::WrongObject)?;
    let len = u64::try_from(mem.len).map_err(|_| SyscallError::WrongObject)?;
    Ok(len)
}

fn validate_shared_reply_meta_bounds(
    meta: yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta,
    object_len: u64,
) -> Result<(), SyscallError> {
    let end = meta
        .offset
        .checked_add(meta.len)
        .ok_or(SyscallError::WrongObject)?;
    if end > object_len {
        return Err(SyscallError::WrongObject);
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

fn handle_vm_anon_map(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let base = frame.arg(SYSCALL_ARG_CAP);
    let len = frame.arg(SYSCALL_ARG_PTR);
    let prot = frame.arg(SYSCALL_ARG_LEN);
    let reserved = [
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        frame.arg(SYSCALL_ARG_TRANSFER_CAP),
    ];
    if reserved.iter().any(|v| *v != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    if base == 0 || len == 0 || !base.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(base as u64, len as u64)?;
    let map_len = round_up_page(len)?;
    let end = base.checked_add(map_len).ok_or(SyscallError::InvalidArgs)?;
    let flags = vm_map_page_flags(prot)?;
    if !flags.read || !flags.write || flags.execute {
        return Err(SyscallError::InvalidArgs);
    }
    let mut check_va = base;
    while check_va < end {
        if kernel
            .is_user_page_mapped_in_current_asid(VirtAddr(check_va as u64))
            .map_err(SyscallError::from)?
        {
            return Err(SyscallError::WrongObject);
        }
        check_va += PAGE_SIZE;
    }

    let (_mem_id, mem_cap) = kernel
        .alloc_anonymous_memory_object_with_len(map_len)
        .map_err(SyscallError::from)?;

    let mut va = base;
    while va < end {
        if let Err(err) =
            kernel.map_user_page_in_current_asid_with_caps(mem_cap, VirtAddr(va as u64), flags)
        {
            let mut rollback_va = base;
            while rollback_va < va {
                let _ = kernel.unmap_user_page_in_current_asid(VirtAddr(rollback_va as u64));
                rollback_va += PAGE_SIZE;
            }
            let cnode = kernel.current_task_cnode().ok_or(SyscallError::Internal)?;
            kernel
                .revoke_capability_in_cnode(cnode, mem_cap)
                .map_err(SyscallError::from)?;
            return Err(SyscallError::from(err));
        }
        va += PAGE_SIZE;
    }

    frame.set_ok(base, map_len, mem_cap.0 as usize);
    Ok(())
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

fn handle_vm_unmap(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let base = frame.arg(SYSCALL_ARG_CAP);
    let len = frame.arg(SYSCALL_ARG_PTR);
    let reserved = [
        frame.arg(SYSCALL_ARG_LEN),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        frame.arg(SYSCALL_ARG_TRANSFER_CAP),
    ];
    if reserved.iter().any(|v| *v != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    if base == 0 || len == 0 || !base.is_multiple_of(PAGE_SIZE) {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(base as u64, len as u64)?;
    let map_len = round_up_page(len)?;
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
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn handle_cap_release(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    let reserved = [
        frame.arg(SYSCALL_ARG_PTR),
        frame.arg(SYSCALL_ARG_LEN),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        frame.arg(SYSCALL_ARG_TRANSFER_CAP),
    ];
    if reserved.iter().any(|v| *v != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    let cnode = kernel.current_task_cnode().ok_or(SyscallError::Internal)?;
    kernel
        .revoke_capability_in_cnode(cnode, cap)
        .map_err(SyscallError::from)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn read_user_bytes_exact(
    kernel: &KernelState,
    user_ptr: usize,
    out: &mut [u8],
) -> Result<(), SyscallError> {
    if out.is_empty() {
        return Ok(());
    }
    let mut offset = 0usize;
    while offset < out.len() {
        let chunk_len = core::cmp::min(Message::MAX_PAYLOAD, out.len() - offset);
        let chunk = kernel
            .copy_from_current_user(user_ptr + offset, chunk_len)
            .map_err(SyscallError::from)?;
        out[offset..offset + chunk_len].copy_from_slice(&chunk[..chunk_len]);
        offset += chunk_len;
    }
    Ok(())
}

fn write_user_bytes_exact(
    kernel: &mut KernelState,
    user_ptr: usize,
    bytes: &[u8],
) -> Result<(), SyscallError> {
    kernel
        .copy_to_current_user(user_ptr, bytes)
        .map_err(SyscallError::from)
}

fn read_ipc_v2_block_from_user(
    kernel: &KernelState,
    frame: &TrapFrame,
    expected_op: u16,
) -> Result<IpcRegisterBlockV2, SyscallError> {
    let user_ptr = frame.arg(SYSCALL_ARG_CAP);
    let block_size = frame.arg(SYSCALL_ARG_PTR);
    let reserved = [
        frame.arg(SYSCALL_ARG_LEN),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0),
        frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1),
        frame.arg(SYSCALL_ARG_TRANSFER_CAP),
    ];

    if user_ptr == 0 || block_size != IPC_ABI_V2_BLOCK_SIZE || reserved.iter().any(|v| *v != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(user_ptr as u64, block_size as u64)?;

    let mut raw = [0u8; IPC_ABI_V2_BLOCK_SIZE];
    read_user_bytes_exact(kernel, user_ptr, &mut raw)?;
    let block = unsafe { core::ptr::read_unaligned(raw.as_ptr() as *const IpcRegisterBlockV2) };

    if block.abi_version != IPC_ABI_V2_VERSION || block.op != expected_op {
        return Err(SyscallError::InvalidArgs);
    }
    let allowed_flags = IPC_V2_FLAG_INLINE_PAYLOAD
        | IPC_V2_FLAG_TRANSFER_CAP
        | IPC_V2_FLAG_RECV_COPYOUT
        | IPC_V2_FLAG_RET_COPYOUT;
    if (block.flags & !allowed_flags) != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0 {
        if block.len > 64 {
            return Err(SyscallError::InvalidArgs);
        }
    } else if block.inline_words.iter().any(|w| *w != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    if (block.flags & IPC_V2_FLAG_RET_COPYOUT) != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    Ok(block)
}

fn write_ipc_v2_block_to_user(
    kernel: &mut KernelState,
    frame: &TrapFrame,
    block: &IpcRegisterBlockV2,
) -> Result<(), SyscallError> {
    let user_ptr = frame.arg(SYSCALL_ARG_CAP);
    let block_size = frame.arg(SYSCALL_ARG_PTR);
    if user_ptr == 0 || block_size != IPC_ABI_V2_BLOCK_SIZE {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(user_ptr as u64, block_size as u64)?;
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (block as *const IpcRegisterBlockV2).cast::<u8>(),
            IPC_ABI_V2_BLOCK_SIZE,
        )
    };
    write_user_bytes_exact(kernel, user_ptr, bytes)
}

fn inline_payload_from_ipc_v2_block(
    block: &IpcRegisterBlockV2,
) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    let len = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
    if len > 64 || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let mut payload = [0u8; Message::MAX_PAYLOAD];
    let mut raw = [0u8; 64];
    for (idx, word) in block.inline_words.iter().enumerate() {
        let b = word.to_le_bytes();
        let start = idx * 8;
        raw[start..start + 8].copy_from_slice(&b);
    }
    payload[..len].copy_from_slice(&raw[..len]);
    Ok(payload)
}

fn handle_ipc_send_v2_stub(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let block = read_ipc_v2_block_from_user(kernel, frame, IPC_V2_OP_SEND)?;
    if block.aux1 != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let opcode = u16::try_from(block.aux0).map_err(|_| SyscallError::InvalidArgs)?;

    let cap = CapId(block.endpoint_cap);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;

    let transfer_cap = if (block.flags & IPC_V2_FLAG_TRANSFER_CAP) != 0 {
        if block.transfer_cap == IPC_V2_NO_TRANSFER_CAP {
            return Err(SyscallError::InvalidArgs);
        }
        let cap = CapId(block.transfer_cap);
        validate_transfer_cap(kernel, cap)?;
        Some(cap)
    } else {
        None
    };

    let sender_tid = current_tid(kernel)?;
    let len = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
    let payload = if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0 {
        inline_payload_from_ipc_v2_block(&block)?
    } else {
        if len > Message::MAX_PAYLOAD {
            return Err(SyscallError::InvalidArgs);
        }
        match kernel.copy_from_current_user(block.ptr_or_offset as usize, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(
                    kernel,
                    frame,
                    block.ptr_or_offset as usize,
                    FaultAccess::Read,
                );
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        }
    };

    let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint, None)?;
    let msg = Message::with_header(
        sender_tid,
        opcode,
        transfer_flag_bits(transfer_cap),
        transfer_handle,
        &payload[..len],
    )
    .map_err(|_| SyscallError::InvalidArgs)?;

    kernel.ipc_send(cap, msg).map_err(SyscallError::from)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn handle_ipc_recv_v2_stub(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let mut block = read_ipc_v2_block_from_user(kernel, frame, IPC_V2_OP_RECV)?;
    if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0
        || (block.flags & IPC_V2_FLAG_TRANSFER_CAP) != 0
        || (block.flags & IPC_V2_FLAG_RET_COPYOUT) != 0
    {
        return Err(SyscallError::InvalidArgs);
    }
    let recv_copyout = (block.flags & IPC_V2_FLAG_RECV_COPYOUT) != 0;
    if !recv_copyout && block.aux1 != 0 {
        return Err(SyscallError::InvalidArgs);
    }

    let cap = CapId(block.endpoint_cap);
    validate_endpoint_right(kernel, cap, CapRights::RECEIVE)?;
    let timeout_ticks = block.aux0;
    let waiter_tid = current_tid(kernel)?;
    let received = if timeout_ticks == 0 {
        kernel.try_ipc_recv(cap).map_err(SyscallError::from)?
    } else {
        kernel
            .ipc_recv_with_deadline(cap, timeout_ticks)
            .map_err(SyscallError::from)?
    };
    let Some(msg) = received else {
        if timeout_ticks == 0 {
            return Err(SyscallError::WouldBlock);
        }
        let fired = kernel
            .consume_ipc_timeout_fired_for_tid(waiter_tid)
            .map_err(SyscallError::from)?;
        return Err(if fired {
            SyscallError::TimedOut
        } else {
            SyscallError::WouldBlock
        });
    };

    let actual_len = msg.len as usize;

    block.flags &= !(
        IPC_V2_FLAG_INLINE_PAYLOAD
            | IPC_V2_FLAG_TRANSFER_CAP
            | IPC_V2_FLAG_RECV_COPYOUT
            | IPC_V2_FLAG_RET_COPYOUT
    );
    block.ret_status = msg.opcode as u64;
    block.ret_len = actual_len as u64;
    block.ret_transfer_cap = msg
        .transferred_cap()
        .map(|cap| cap.0)
        .unwrap_or(IPC_V2_NO_TRANSFER_CAP);
    if msg.transferred_cap().is_some() {
        block.flags |= IPC_V2_FLAG_TRANSFER_CAP;
    }

    if actual_len <= 64 && !recv_copyout {
        block.flags |= IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = actual_len as u64;
        block.inline_words = [0; 8];
        for (idx, chunk) in msg.as_slice().chunks(8).enumerate() {
            let mut lane = [0u8; 8];
            lane[..chunk.len()].copy_from_slice(chunk);
            block.inline_words[idx] = u64::from_le_bytes(lane);
        }
    } else {
        if !recv_copyout {
            return Err(SyscallError::BufferTooSmall);
        }
        let capacity = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
        if actual_len > capacity {
            return Err(SyscallError::BufferTooSmall);
        }
        if block.aux1 == 0 {
            return Err(SyscallError::InvalidArgs);
        }
        validate_user_region(block.aux1, actual_len as u64)?;
        write_user_bytes_exact(kernel, block.aux1 as usize, msg.as_slice())?;
        block.flags |= IPC_V2_FLAG_RET_COPYOUT;
        block.len = capacity as u64;
        block.inline_words = [0; 8];
    }

    write_ipc_v2_block_to_user(kernel, frame, &block)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn handle_ipc_call_v2_stub(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let mut block = read_ipc_v2_block_from_user(kernel, frame, IPC_V2_OP_CALL)?;
    if (block.flags & IPC_V2_FLAG_RET_COPYOUT) != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let recv_copyout = (block.flags & IPC_V2_FLAG_RECV_COPYOUT) != 0;

    let cap = CapId(block.endpoint_cap);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .capability_service()
        .resolve_current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;

    let reply_recv_cap = CapId(block.aux0);
    let request_opcode = u16::try_from(block.aux1).map_err(|_| SyscallError::InvalidArgs)?;
    validate_endpoint_right(kernel, reply_recv_cap, CapRights::RECEIVE)?;
    let Some(responder_tid) = kernel.endpoint_waiter_tid(endpoint) else {
        #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
        crate::yarm_log!(
            "IPC_CALL_V2_WOULD_BLOCK cap={} endpoint={} reason=no_receiver_or_no_reply",
            cap.0,
            endpoint.id
        );
        return Err(SyscallError::WouldBlock);
    };

    if (block.flags & IPC_V2_FLAG_TRANSFER_CAP) != 0 {
        return Err(SyscallError::InvalidArgs);
    }

    let sender_tid = current_tid(kernel)?;
    let reply_cap = kernel
        .create_reply_cap_for_caller(
            crate::kernel::ipc::ThreadId(sender_tid),
            reply_recv_cap,
            Some(responder_tid),
        )
        .map_err(SyscallError::from)?;

    let len = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    let payload = if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0 {
        inline_payload_from_ipc_v2_block(&block)?
    } else {
        match kernel.copy_from_current_user(block.ptr_or_offset as usize, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(
                    kernel,
                    frame,
                    block.ptr_or_offset as usize,
                    FaultAccess::Read,
                );
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        }
    };

    let transfer_handle = stash_transfer_handle(kernel, Some(reply_cap), endpoint, None)?;
    let msg = Message::with_header(
        sender_tid,
        request_opcode,
        Message::FLAG_CAP_TRANSFER,
        transfer_handle,
        &payload[..len],
    )
    .map_err(|_| SyscallError::InvalidArgs)?;

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

    let received = kernel
        .ipc_recv(reply_recv_cap)
        .map_err(SyscallError::from)?;
    let Some(reply) = received else {
        return Err(SyscallError::WouldBlock);
    };
    let reply_len = reply.len as usize;
    let reply_copyout_ptr = block.aux1;
    let reply_copyout_capacity = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
    block.flags &= !(
        IPC_V2_FLAG_INLINE_PAYLOAD
            | IPC_V2_FLAG_TRANSFER_CAP
            | IPC_V2_FLAG_RECV_COPYOUT
            | IPC_V2_FLAG_RET_COPYOUT
    );
    block.ret_status = reply.opcode as u64;
    block.ret_len = reply_len as u64;
    block.ret_transfer_cap = reply
        .transferred_cap()
        .map(|cap| cap.0)
        .unwrap_or(IPC_V2_NO_TRANSFER_CAP);
    if reply.transferred_cap().is_some() {
        block.flags |= IPC_V2_FLAG_TRANSFER_CAP;
    }
    if reply_len <= 64 && !recv_copyout {
        block.flags |= IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = reply_len as u64;
        block.inline_words = [0; 8];
        for (idx, chunk) in reply.as_slice().chunks(8).enumerate() {
            let mut lane = [0u8; 8];
            lane[..chunk.len()].copy_from_slice(chunk);
            block.inline_words[idx] = u64::from_le_bytes(lane);
        }
    } else {
        if !recv_copyout {
            return Err(SyscallError::BufferTooSmall);
        }
        if reply_copyout_ptr == 0 {
            return Err(SyscallError::InvalidArgs);
        }
        if reply_len > reply_copyout_capacity {
            return Err(SyscallError::BufferTooSmall);
        }
        validate_user_region(reply_copyout_ptr, reply_len as u64)?;
        write_user_bytes_exact(kernel, reply_copyout_ptr as usize, reply.as_slice())?;
        block.flags |= IPC_V2_FLAG_RET_COPYOUT;
        block.len = reply_copyout_capacity as u64;
        block.inline_words = [0; 8];
    }

    write_ipc_v2_block_to_user(kernel, frame, &block)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn handle_ipc_reply_v2_stub(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let block = read_ipc_v2_block_from_user(kernel, frame, IPC_V2_OP_REPLY)?;
    if block.aux1 != 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let opcode = u16::try_from(block.aux0).map_err(|_| SyscallError::InvalidArgs)?;

    let reply_cap = CapId(block.endpoint_cap);
    let len = usize::try_from(block.len).map_err(|_| SyscallError::InvalidArgs)?;
    if len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    let payload = if (block.flags & IPC_V2_FLAG_INLINE_PAYLOAD) != 0 {
        inline_payload_from_ipc_v2_block(&block)?
    } else {
        match kernel.copy_from_current_user(block.ptr_or_offset as usize, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                record_user_fault(
                    kernel,
                    frame,
                    block.ptr_or_offset as usize,
                    FaultAccess::Read,
                );
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        }
    };

    let (flags, transfer_cap) = if (block.flags & IPC_V2_FLAG_TRANSFER_CAP) != 0 {
        if block.transfer_cap == IPC_V2_NO_TRANSFER_CAP {
            return Err(SyscallError::InvalidArgs);
        }
        let cap = CapId(block.transfer_cap);
        validate_transfer_cap(kernel, cap)?;
        if let Ok(meta) = yarm_ipc_abi::ipc_v2::decode_shared_reply_meta(&payload[..len]) {
            let object_len = validate_shared_reply_transfer_cap_kind(kernel, cap)?;
            validate_shared_reply_meta_bounds(meta, object_len)?;
        }
        (Message::FLAG_CAP_TRANSFER, Some(block.transfer_cap))
    } else {
        (0, None)
    };

    let sender_tid = current_tid(kernel)?;
    let msg = Message::with_header(sender_tid, opcode, flags, transfer_cap, &payload[..len])
        .map_err(|_| SyscallError::InvalidArgs)?;
    kernel
        .ipc_reply(reply_cap, msg)
        .map_err(SyscallError::from)?;
    frame.set_ok(0, 0, 0);
    Ok(())
}

fn handle_debug_serial_write(frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let reserved = [frame.arg(1), frame.arg(2), frame.arg(3), frame.arg(4), frame.arg(5)];
    if reserved.iter().any(|value| *value != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    let byte = (frame.arg(0) & 0xff) as u8;
    let mut emitted = 0usize;
    if DEBUG_SERIAL_SYSCALL_ENABLED && crate::arch::console::try_write_byte(byte) {
        emitted = 1;
    }
    frame.set_ok(emitted, 0, 0);
    Ok(())
}

fn handle_debug_serial_write_buf(
    kernel: &KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let user_ptr = frame.arg(0);
    let len = frame.arg(1);
    let reserved = [frame.arg(2), frame.arg(3), frame.arg(4), frame.arg(5)];
    if reserved.iter().any(|value| *value != 0) {
        return Err(SyscallError::InvalidArgs);
    }
    if user_ptr == 0 || len == 0 || len > DEBUG_SERIAL_WRITE_BUF_MAX_LEN {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(user_ptr as u64, len as u64)?;
    let mut buf = [0u8; DEBUG_SERIAL_WRITE_BUF_MAX_LEN];
    read_user_bytes_exact(kernel, user_ptr, &mut buf[..len])?;
    let mut emitted = 0usize;
    if DEBUG_SERIAL_SYSCALL_ENABLED && crate::arch::console::try_write_bytes(&buf[..len]) {
        emitted = 1;
    }
    frame.set_ok(emitted, 0, 0);
    Ok(())
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        let tid = kernel.current_tid().unwrap_or(0);
        if should_log_syscall_trace(frame.syscall_num()) {
            crate::yarm_log!(
                "YARM_SYSCALL_ENTER tid={} nr={} x0={} x1={} x2={} x3={} x4={} x5={}",
                tid,
                frame.syscall_num(),
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
                frame.arg(5)
            );
        }
    }
    let syscall = Syscall::decode(frame.syscall_num())?;
    let caller_tid = kernel.current_tid();
    let result = match syscall {
        Syscall::Yield => {
            kernel.yield_current().map_err(SyscallError::from)?;
            frame.set_ok(0, 0, 0);
            Ok(())
        }
        // Legacy IPC v1 syscall slots are intentionally reserved for ABI stability.
        Syscall::IpcSend
        | Syscall::IpcRecv
        | Syscall::IpcRecvTimeout
        | Syscall::IpcCall
        | Syscall::IpcReply => Err(SyscallError::InvalidNumber),
        Syscall::ControlPlaneSetCnodeSlots => handle_control_plane_set_cnode_slots(kernel, frame),
        Syscall::VmMap => handle_vm_map(kernel, frame),
        Syscall::TransferRelease => handle_transfer_release(kernel, frame),
        Syscall::FutexWait => handle_futex_wait(kernel, frame),
        Syscall::FutexWake => handle_futex_wake(kernel, frame),
        Syscall::SpawnThread => handle_spawn_thread(kernel, frame),
        Syscall::Fork => handle_fork(kernel, frame),
        Syscall::VmAnonMap => handle_vm_anon_map(kernel, frame),
        Syscall::VmBrk => handle_vm_brk(kernel, frame),
        Syscall::VmUnmap => handle_vm_unmap(kernel, frame),
        Syscall::CapRelease => handle_cap_release(kernel, frame),
        Syscall::IpcSendV2 => handle_ipc_send_v2_stub(kernel, frame),
        Syscall::IpcRecvV2 => handle_ipc_recv_v2_stub(kernel, frame),
        Syscall::IpcCallV2 => handle_ipc_call_v2_stub(kernel, frame),
        Syscall::IpcReplyV2 => handle_ipc_reply_v2_stub(kernel, frame),
        Syscall::DebugSerialWrite => handle_debug_serial_write(frame),
        Syscall::DebugSerialWriteBuf => handle_debug_serial_write_buf(kernel, frame),
    };
    if result == Err(SyscallError::WouldBlock) {
        let caller_blocked = caller_tid.is_some_and(|tid| {
            matches!(
                kernel.task_status(tid),
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                        | crate::kernel::task::WaitReason::EndpointReceive(_)
                ))
            )
        });
        let blocking_syscall = false;
        if blocking_syscall && caller_blocked {
            return Ok(());
        }
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        let tid = kernel.current_tid().unwrap_or(0);
        if should_log_syscall_trace(frame.syscall_num()) {
            if let Some(code) = frame.error_code() {
                crate::yarm_log!(
                    "YARM_SYSCALL_EXIT tid={} nr={} result=err code={}",
                    tid,
                    frame.syscall_num(),
                    code
                );
            } else {
                crate::yarm_log!(
                    "YARM_SYSCALL_EXIT tid={} nr={} result=ok r0={} r1={} r2={}",
                    tid,
                    frame.syscall_num(),
                    frame.ret0(),
                    frame.ret1(),
                    frame.ret2()
                );
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::capabilities::Capability;
    use crate::kernel::ipc::EndpointMode;
    use crate::kernel::scheduler_timer::Timer;
    use crate::kernel::trapframe::TrapFrame;
    use yarm_ipc_abi::process_abi::{ExecuteRestartReply, ExecuteRestartRequest};

    fn write_v2_block_to_user_for_test(
        state: &mut KernelState,
        ptr: usize,
        block: &IpcRegisterBlockV2,
    ) {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (block as *const IpcRegisterBlockV2).cast::<u8>(),
                IPC_ABI_V2_BLOCK_SIZE,
            )
        };
        state
            .copy_to_current_user(ptr, bytes)
            .expect("write v2 block");
    }

    fn legacy_inline_payload_from_recv_frame(frame: &TrapFrame, len: usize) -> [u8; 16] {
        let mut out = [0u8; 16];
        let lane0 = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0).to_le_bytes();
        let lane1 = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD1).to_le_bytes();
        out[..8].copy_from_slice(&lane0);
        out[8..16].copy_from_slice(&lane1);
        let _ = len;
        out
    }

    fn memory_object_len_for_cap(state: &KernelState, cap: CapId) -> u64 {
        let obj = state
            .capability_service()
            .resolve_current_task_capability(cap)
            .expect("cap")
            .object;
        let CapObject::MemoryObject { id } = obj else {
            panic!("expected memory object cap");
        };
        state
            .with_memory_state(|memory| {
                memory
                    .memory_objects
                    .iter()
                    .flatten()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.len as u64)
            })
            .expect("memory object len")
    }

    #[test]
    fn syscall_ipc_v2_validation_accepts_valid_send_block() {
        let mut state = Bootstrap::init().expect("kernel");
        let ptr = 0x2000usize;
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = 64;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert!(read_ipc_v2_block_from_user(&state, &frame, IPC_V2_OP_SEND).is_ok());
    }

    #[test]
    fn syscall_ipc_v2_validation_rejects_wrong_size_version_op_reserved_flags_and_len() {
        let mut state = Bootstrap::init().expect("kernel");
        let ptr = 0x3000usize;
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        write_v2_block_to_user_for_test(&mut state, ptr, &block);

        let bad_size = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE - 1, 0, 0, 0, 0],
        );
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &bad_size, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        block.abi_version = 9;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let ok_frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &ok_frame, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &ok_frame, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        let reserved = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 1, 0, 0, 0],
        );
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &reserved, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.flags = 1 << 31;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &ok_frame, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = 65;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &ok_frame, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );

        block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.inline_words[0] = 1;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        assert_eq!(
            read_ipc_v2_block_from_user(&state, &ok_frame, IPC_V2_OP_SEND),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn syscall_ipc_send_v2_inline_payload_reaches_receiver_queue() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let ptr = 0x4000usize;
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.endpoint_cap = send_cap.0;
        block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = 5;
        let mut lane = [0u8; 8];
        lane[..5].copy_from_slice(b"hello");
        block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, ptr, &block);

        let mut send_frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut send_frame).expect("send v2 inline");

        let mut recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv_frame).expect("recv");
        assert_eq!(recv_frame.ret1(), 5);
        let got = legacy_inline_payload_from_recv_frame(&recv_frame, 5);
        assert_eq!(&got[..5], b"hello");
    }

    #[test]
    fn syscall_ipc_send_v2_pointer_payload_and_aux_validation() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let ptr = 0x5000usize;
        state
            .copy_to_current_user(0x6000, b"world")
            .expect("user write");

        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        block.endpoint_cap = send_cap.0;
        block.ptr_or_offset = 0x6000;
        block.len = 5;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);

        let mut send_frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut send_frame).expect("send v2 ptr");

        let mut recv_frame = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv_frame).expect("recv");
        let got = legacy_inline_payload_from_recv_frame(&recv_frame, 5);
        assert_eq!(&got[..5], b"world");

        block.aux1 = 1;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let mut bad_aux = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut bad_aux),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn syscall_ipc_send_v2_then_recv_v2_roundtrip_inline() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");

        let send_ptr = 0x7000usize;
        let recv_ptr = 0x7100usize;

        let mut send_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        send_block.endpoint_cap = send_cap.0;
        send_block.aux0 = 0x44;
        send_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        send_block.len = 6;
        let mut lane = [0u8; 8];
        lane[..6].copy_from_slice(b"recvv2");
        send_block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut send_frame).expect("send v2");

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv_frame).expect("recv v2");

        let mut bytes = [0u8; IPC_ABI_V2_BLOCK_SIZE];
        read_user_bytes_exact(&state, recv_ptr, &mut bytes).expect("read recv block");
        let out = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const IpcRegisterBlockV2) };
        assert_eq!(out.ret_len, 6);
        assert_eq!(out.ret_status, 0x44);
        assert_eq!(out.ret_transfer_cap, IPC_V2_NO_TRANSFER_CAP);
        assert_ne!(out.flags & IPC_V2_FLAG_INLINE_PAYLOAD, 0);
        let lane0 = out.inline_words[0].to_le_bytes();
        assert_eq!(&lane0[..6], b"recvv2");
    }

    #[test]
    fn syscall_recv_v2_empty_endpoint_would_block_and_aux_validation() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let recv_ptr = 0x7200usize;

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut recv_frame),
            Err(SyscallError::WouldBlock)
        );

        recv_block.aux0 = 1;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut timed = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(dispatch(&mut state, &mut timed), Err(SyscallError::TimedOut));

        recv_block.aux1 = 1;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut bad_aux = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut bad_aux),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn syscall_recv_v2_with_timeout_aux0_receives_available_message() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let send_ptr = 0x7210usize;
        let recv_ptr = 0x7220usize;

        let mut send_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        send_block.endpoint_cap = send_cap.0;
        send_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        send_block.len = 4;
        let mut lane = [0u8; 8];
        lane[..4].copy_from_slice(b"pong");
        send_block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame = TrapFrame::new(
            SYSCALL_IPC_SEND_V2_NR,
            [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut send_frame).expect("send v2");

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        recv_block.aux0 = 5;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut recv_frame).expect("recv v2 timed");
    }

    #[test]
    fn syscall_recv_v2_inline_fastpath_exact_64_bytes() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let send_ptr = 0x7220usize;
        let recv_ptr = 0x7230usize;

        let payload = [0xABu8; 64];
        state.copy_to_current_user(0x9000, &payload).expect("user write");
        let mut send_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        send_block.endpoint_cap = send_cap.0;
        send_block.ptr_or_offset = 0x9000;
        send_block.len = payload.len() as u64;
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_V2_NR, [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut send_frame).expect("send");

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(SYSCALL_IPC_RECV_V2_NR, [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut recv_frame).expect("recv");

        let mut bytes = [0u8; IPC_ABI_V2_BLOCK_SIZE];
        read_user_bytes_exact(&state, recv_ptr, &mut bytes).expect("read");
        let out = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const IpcRegisterBlockV2) };
        assert_eq!(out.ret_len, 64);
        assert_ne!(out.flags & IPC_V2_FLAG_INLINE_PAYLOAD, 0);
    }

    #[test]
    fn syscall_recv_v2_large_reply_requires_copyout_flag() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let send_ptr = 0x7240usize;
        let recv_ptr = 0x7250usize;
        let payload = [0xCDu8; 65];
        state.copy_to_current_user(0x9100, &payload).expect("user write");
        let mut send_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        send_block.endpoint_cap = send_cap.0;
        send_block.ptr_or_offset = 0x9100;
        send_block.len = payload.len() as u64;
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_V2_NR, [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut send_frame).expect("send");

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(SYSCALL_IPC_RECV_V2_NR, [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut recv_frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_recv_v2_large_reply_copyout_success_and_small_capacity_error() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let send_ptr = 0x7260usize;
        let recv_ptr = 0x7270usize;
        let payload = [0xEEu8; 65];

        // Success case.
        state.copy_to_current_user(0x9200, &payload).expect("user write payload");
        let mut send_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_SEND);
        send_block.endpoint_cap = send_cap.0;
        send_block.ptr_or_offset = 0x9200;
        send_block.len = payload.len() as u64;
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_V2_NR, [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut send_frame).expect("send");

        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        recv_block.flags = IPC_V2_FLAG_RECV_COPYOUT;
        recv_block.aux1 = 0x9300;
        recv_block.len = 65;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame = TrapFrame::new(SYSCALL_IPC_RECV_V2_NR, [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut recv_frame).expect("recv copyout");
        let copied = state.copy_from_current_user(0x9300, 65).expect("copied");
        assert_eq!(&copied[..], &payload);
        let mut bytes = [0u8; IPC_ABI_V2_BLOCK_SIZE];
        read_user_bytes_exact(&state, recv_ptr, &mut bytes).expect("read");
        let out = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const IpcRegisterBlockV2) };
        assert_eq!(out.ret_len, 65);
        assert_ne!(out.flags & IPC_V2_FLAG_RET_COPYOUT, 0);
        assert_eq!(out.inline_words, [0; 8]);

        // Small-capacity case (current behavior: BufferTooSmall and block is not written back).
        state.copy_to_current_user(0x9400, &payload).expect("user write payload2");
        send_block.ptr_or_offset = 0x9400;
        write_v2_block_to_user_for_test(&mut state, send_ptr, &send_block);
        let mut send_frame2 = TrapFrame::new(SYSCALL_IPC_SEND_V2_NR, [send_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut send_frame2).expect("send2");
        recv_block.len = 64;
        write_v2_block_to_user_for_test(&mut state, recv_ptr, &recv_block);
        let mut recv_frame2 = TrapFrame::new(SYSCALL_IPC_RECV_V2_NR, [recv_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut recv_frame2), Err(SyscallError::BufferTooSmall));
    }

    #[test]
    fn syscall_ipc_reply_v2_inline_routes_and_consumes_reply_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let reply_cap = state
            .create_reply_cap_for_caller(crate::kernel::ipc::ThreadId(0), recv_cap, None)
            .expect("reply cap");

        let ptr = 0x7300usize;
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        block.endpoint_cap = reply_cap.0;
        block.aux0 = 0x55;
        block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        block.len = 5;
        let mut lane = [0u8; 8];
        lane[..5].copy_from_slice(b"reply");
        block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, ptr, &block);

        let mut frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("reply v2");

        let received = state.ipc_recv(recv_cap).expect("recv").expect("message");
        assert_eq!(received.as_slice(), b"reply");
        assert_eq!(received.opcode, 0x55);
    }

    #[test]
    fn syscall_ipc_reply_v2_pointer_and_aux_validation() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        let reply_cap = state
            .create_reply_cap_for_caller(crate::kernel::ipc::ThreadId(0), recv_cap, None)
            .expect("reply cap");

        state
            .copy_to_current_user(0x7400, b"ptrok")
            .expect("write user");
        let ptr = 0x7410usize;
        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        block.endpoint_cap = reply_cap.0;
        block.ptr_or_offset = 0x7400;
        block.len = 5;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let mut frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut frame).expect("reply ptr");

        let received = state.ipc_recv(recv_cap).expect("recv").expect("message");
        assert_eq!(received.as_slice(), b"ptrok");

        block.aux1 = 1;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let mut bad = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut bad),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn syscall_ipc_call_v2_inline_and_reply_v2_roundtrip() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");

        let req_ptr = 0x7500usize;
        let reply_ptr = 0x7600usize;

        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.aux1 = 0x66;
        let mut req_lane = [0u8; 8];
        req_lane[..4].copy_from_slice(b"call");
        call_block.inline_words[0] = u64::from_le_bytes(req_lane);
        write_v2_block_to_user_for_test(&mut state, req_ptr, &call_block);

        let mut waiter = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let mut recv_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV);
        recv_block.endpoint_cap = recv_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &recv_block);

        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut block_recv);

        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [req_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);

        let received = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(received.opcode, 0x66);
        let reply_cap = received.transferred_cap().expect("reply cap");

        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.aux0 = 0x77;
        reply_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        reply_block.len = 2;
        let mut lane = [0u8; 8];
        lane[..2].copy_from_slice(b"ok");
        reply_block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");

        let mut recv_reply = TrapFrame::new(
            SYSCALL_IPC_RECV_V2_NR,
            [req_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        write_v2_block_to_user_for_test(
            &mut state,
            req_ptr,
            &IpcRegisterBlockV2::new_v2(IPC_V2_OP_RECV),
        );
        let _ = dispatch(&mut state, &mut recv_reply);
    }

    #[test]
    fn syscall_ipc_call_v2_restart_control_process_abi_roundtrip_inline_reply() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");

        let req_ptr = 0x7800usize;
        let reply_ptr = 0x7900usize;

        let req_bytes = ExecuteRestartRequest::new(42, 0xBEEF).encode();
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = req_bytes.len() as u64;
        call_block.aux0 = recv_cap.0;
        let mut lane0 = [0u8; 8];
        lane0.copy_from_slice(&req_bytes[..8]);
        call_block.inline_words[0] = u64::from_le_bytes(lane0);
        let mut lane1 = [0u8; 8];
        lane1.copy_from_slice(&req_bytes[8..16]);
        call_block.inline_words[1] = u64::from_le_bytes(lane1);
        write_v2_block_to_user_for_test(&mut state, req_ptr, &call_block);

        let mut block_recv = TrapFrame::new(
            Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, Message::MAX_PAYLOAD, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut block_recv);

        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [req_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);

        let received = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(received.as_slice(), req_bytes);
        let reply_cap = received.transferred_cap().expect("reply cap");

        let reply_bytes = ExecuteRestartReply::new(ExecuteRestartReply::STATUS_OK).encode();
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        reply_block.len = reply_bytes.len() as u64;
        let mut lane = [0u8; 8];
        lane[0] = reply_bytes[0];
        reply_block.inline_words[0] = u64::from_le_bytes(lane);
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");

        let out_block_bytes = state
            .copy_from_current_user(req_ptr, IPC_ABI_V2_BLOCK_SIZE)
            .expect("read call result block");
        let mut out = IpcRegisterBlockV2::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                out_block_bytes.as_ptr(),
                (&mut out as *mut IpcRegisterBlockV2).cast::<u8>(),
                IPC_ABI_V2_BLOCK_SIZE,
            );
        }
        assert_eq!(out.ret_len, 1);
        let decoded = ExecuteRestartReply::decode(&[out.inline_words[0].to_le_bytes()[0]])
            .expect("decode restart reply");
        assert_eq!(decoded.status, ExecuteRestartReply::STATUS_OK);
    }

    #[test]
    fn syscall_ipc_call_v2_aux1_and_pointer_validation() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let ptr = 0x7700usize;
        state
            .copy_to_current_user(0x7710, b"data")
            .expect("user write");

        let mut block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        block.endpoint_cap = send_cap.0;
        block.ptr_or_offset = 0x7710;
        block.len = 4;
        block.aux0 = recv_cap.0;
        block.aux1 = 1;
        write_v2_block_to_user_for_test(&mut state, ptr, &block);
        let mut frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut frame),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn syscall_ipc_call_v2_large_reply_copyout_and_no_copyout_error() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let req_ptr = 0xA000usize;
        let reply_ptr = 0xA100usize;
        let call_ptr = 0xA200usize;
        let reply_bytes = [0x5Au8; 65];

        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD | IPC_V2_FLAG_RECV_COPYOUT;
        call_block.len = 65;
        call_block.aux0 = recv_cap.0;
        call_block.aux1 = 0xA300;
        call_block.inline_words[0] = u64::from_le_bytes(*b"rqst0000");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(SYSCALL_IPC_CALL_V2_NR, [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        let _ = dispatch(&mut state, &mut call_frame);
        let received = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        let reply_cap = received.transferred_cap().expect("reply cap");
        state.copy_to_current_user(req_ptr, &reply_bytes).expect("user write");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = req_ptr as u64;
        reply_block.len = 65;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame = TrapFrame::new(SYSCALL_IPC_REPLY_V2_NR, [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut reply_frame).expect("reply");
        let out_block = state.copy_from_current_user(call_ptr, IPC_ABI_V2_BLOCK_SIZE).expect("read call");
        let out = unsafe { core::ptr::read_unaligned(out_block.as_ptr() as *const IpcRegisterBlockV2) };
        assert_eq!(out.ret_len, 65);
        assert_ne!(out.flags & IPC_V2_FLAG_RET_COPYOUT, 0);
        let copied = state.copy_from_current_user(0xA300, 65).expect("copied");
        assert_eq!(&copied[..], &reply_bytes);

        // Copyout requested but capacity too small should fail with BufferTooSmall.
        let mut small_copy_call = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        small_copy_call.endpoint_cap = send_cap.0;
        small_copy_call.flags = IPC_V2_FLAG_INLINE_PAYLOAD | IPC_V2_FLAG_RECV_COPYOUT;
        small_copy_call.len = 64;
        small_copy_call.aux0 = recv_cap.0;
        small_copy_call.aux1 = 0xA400;
        small_copy_call.inline_words[0] = u64::from_le_bytes(*b"rqst0002");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &small_copy_call);
        let mut small_copy_frame = TrapFrame::new(SYSCALL_IPC_CALL_V2_NR, [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        let _ = dispatch(&mut state, &mut small_copy_frame);
        let received_small = state.ipc_recv(recv_cap).expect("recv small").expect("msg small");
        let reply_cap_small = received_small.transferred_cap().expect("reply cap small");
        reply_block.endpoint_cap = reply_cap_small.0;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame_small = TrapFrame::new(SYSCALL_IPC_REPLY_V2_NR, [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut reply_frame_small).expect("reply small");
        assert_eq!(dispatch(&mut state, &mut small_copy_frame), Err(SyscallError::BufferTooSmall));

        // Same reply size without copyout flag should fail.
        let mut no_copy_call = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        no_copy_call.endpoint_cap = send_cap.0;
        no_copy_call.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        no_copy_call.len = 8;
        no_copy_call.aux0 = recv_cap.0;
        no_copy_call.inline_words[0] = u64::from_le_bytes(*b"rqst0001");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &no_copy_call);
        let mut no_copy_frame = TrapFrame::new(SYSCALL_IPC_CALL_V2_NR, [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        let _ = dispatch(&mut state, &mut no_copy_frame);
        let received2 = state.ipc_recv(recv_cap).expect("recv2").expect("msg2");
        let reply_cap2 = received2.transferred_cap().expect("reply cap2");
        reply_block.endpoint_cap = reply_cap2.0;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame2 = TrapFrame::new(SYSCALL_IPC_REPLY_V2_NR, [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0]);
        dispatch(&mut state, &mut reply_frame2).expect("reply2");
        assert_eq!(dispatch(&mut state, &mut no_copy_frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_ipc_reply_v2_transfer_cap_memory_object_sets_ret_transfer_cap_baseline() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        let call_ptr = 0xB000usize;
        let reply_ptr = 0xB100usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"ping\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0x1000,
            len: 0x2000,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        state
            .copy_to_current_user(reply_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = mem_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_ptr + 0x80, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr + 0x80, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");

        dispatch(&mut state, &mut call_frame).expect("call completion");
        let out_block = state
            .copy_from_current_user(call_ptr, IPC_ABI_V2_BLOCK_SIZE)
            .expect("call out");
        let out = unsafe {
            core::ptr::read_unaligned(out_block.as_ptr() as *const IpcRegisterBlockV2)
        };
        assert_eq!(out.ret_transfer_cap, mem_cap.0);
        assert_ne!(out.flags & IPC_V2_FLAG_TRANSFER_CAP, 0);
    }

    #[test]
    fn syscall_ipc_reply_v2_shared_meta_transfer_cap_rejects_non_memory_object() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");

        let call_ptr = 0xB200usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"ping\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0x1000,
            len: 0x2000,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        let reply_payload_ptr = 0xB260usize;
        state
            .copy_to_current_user(reply_payload_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_payload_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = send_cap.0; // endpoint cap (non-MemoryObject)
        let reply_ptr = 0xB280usize;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut reply_frame),
            Err(SyscallError::WrongObject)
        );
    }

    #[test]
    fn syscall_ipc_reply_v2_shared_meta_exact_object_len_succeeds() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let obj_len = memory_object_len_for_cap(&state, mem_cap);

        let call_ptr = 0xB2C0usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"size\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0,
            len: obj_len,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        let reply_payload_ptr = 0xB2E0usize;
        state
            .copy_to_current_user(reply_payload_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_payload_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = mem_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_payload_ptr + 0x80, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_payload_ptr + 0x80, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");
    }

    #[test]
    fn syscall_ipc_reply_v2_shared_meta_over_object_len_rejects_wrong_object() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let obj_len = memory_object_len_for_cap(&state, mem_cap);

        let call_ptr = 0xB300usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"obnd\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: obj_len - 1,
            len: 2,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        let reply_payload_ptr = 0xB320usize;
        state
            .copy_to_current_user(reply_payload_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_payload_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = mem_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_payload_ptr + 0x80, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_payload_ptr + 0x80, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut reply_frame),
            Err(SyscallError::WrongObject)
        );
    }

    #[test]
    fn syscall_ipc_reply_v2_shared_meta_offset_beyond_object_len_rejects_wrong_object() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let obj_len = memory_object_len_for_cap(&state, mem_cap);

        let call_ptr = 0xB340usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"ofst\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: obj_len + 1,
            len: 1,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        let reply_payload_ptr = 0xB360usize;
        state
            .copy_to_current_user(reply_payload_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_payload_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = mem_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_payload_ptr + 0x80, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_payload_ptr + 0x80, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        assert_eq!(
            dispatch(&mut state, &mut reply_frame),
            Err(SyscallError::WrongObject)
        );
    }

    #[test]
    fn syscall_ipc_reply_v2_non_shared_payload_preserves_generic_transfer_cap_behavior() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");

        let call_ptr = 0xB220usize;
        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"pong\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        // Non-shared payload: transfer-kind policy should remain generic.
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD | IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = send_cap.0; // endpoint cap
        reply_block.len = 2;
        let mut lane = [0u8; 8];
        lane[..2].copy_from_slice(b"ok");
        reply_block.inline_words[0] = u64::from_le_bytes(lane);
        let reply_ptr = 0xB2A0usize;
        write_v2_block_to_user_for_test(&mut state, reply_ptr, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");

        dispatch(&mut state, &mut call_frame).expect("call completion");
        let out_block = state
            .copy_from_current_user(call_ptr, IPC_ABI_V2_BLOCK_SIZE)
            .expect("call out");
        let out = unsafe {
            core::ptr::read_unaligned(out_block.as_ptr() as *const IpcRegisterBlockV2)
        };
        assert_eq!(out.ret_transfer_cap, send_cap.0);
        assert_ne!(out.flags & IPC_V2_FLAG_TRANSFER_CAP, 0);
    }

    #[test]
    fn syscall_ipc_call_v2_reply_transfer_with_shared_reply_meta_payload_baseline() {
        let mut state = Bootstrap::init().expect("kernel");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(8).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let call_ptr = 0xB300usize;
        let reply_ptr = 0xB380usize;

        let mut call_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_CALL);
        call_block.endpoint_cap = send_cap.0;
        call_block.flags = IPC_V2_FLAG_INLINE_PAYLOAD;
        call_block.len = 4;
        call_block.aux0 = recv_cap.0;
        call_block.inline_words[0] = u64::from_le_bytes(*b"meta\0\0\0\0");
        write_v2_block_to_user_for_test(&mut state, call_ptr, &call_block);
        let mut call_frame = TrapFrame::new(
            SYSCALL_IPC_CALL_V2_NR,
            [call_ptr, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        let _ = dispatch(&mut state, &mut call_frame);
        let req = state.ipc_recv(recv_cap).expect("recv").expect("req");
        let reply_cap = req.transferred_cap().expect("reply cap");

        let meta = yarm_ipc_abi::ipc_v2::IpcV2SharedReplyMeta {
            version: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_META_VERSION,
            flags: yarm_ipc_abi::ipc_v2::IPC_V2_SHARED_REPLY_FLAG_READ_ONLY,
            reserved: 0,
            offset: 0x4000,
            len: 0x1000,
        };
        let payload = yarm_ipc_abi::ipc_v2::encode_shared_reply_meta(meta).expect("meta");
        state
            .copy_to_current_user(reply_ptr, &payload)
            .expect("meta bytes");
        let mut reply_block = IpcRegisterBlockV2::new_v2(IPC_V2_OP_REPLY);
        reply_block.endpoint_cap = reply_cap.0;
        reply_block.ptr_or_offset = reply_ptr as u64;
        reply_block.len = payload.len() as u64;
        reply_block.flags = IPC_V2_FLAG_TRANSFER_CAP;
        reply_block.transfer_cap = mem_cap.0;
        write_v2_block_to_user_for_test(&mut state, reply_ptr + 0x80, &reply_block);
        let mut reply_frame = TrapFrame::new(
            SYSCALL_IPC_REPLY_V2_NR,
            [reply_ptr + 0x80, IPC_ABI_V2_BLOCK_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut reply_frame).expect("reply");

        dispatch(&mut state, &mut call_frame).expect("call completion");
        let out_block = state
            .copy_from_current_user(call_ptr, IPC_ABI_V2_BLOCK_SIZE)
            .expect("call out");
        let out = unsafe {
            core::ptr::read_unaligned(out_block.as_ptr() as *const IpcRegisterBlockV2)
        };
        assert_eq!(out.ret_transfer_cap, mem_cap.0);
        let decoded_meta = yarm_ipc_abi::ipc_v2::decode_shared_reply_meta(&payload)
            .expect("decode shared meta");
        assert_eq!(out.ret_transfer_cap, mem_cap.0);
        assert_eq!(decoded_meta.offset, 0x4000);
        assert_eq!(decoded_meta.len, 0x1000);
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
        assert_eq!(SYSCALL_IPC_SEND_V2_NR, 15);
        assert_eq!(SYSCALL_IPC_RECV_V2_NR, 16);
        assert_eq!(SYSCALL_IPC_CALL_V2_NR, 17);
        assert_eq!(SYSCALL_IPC_REPLY_V2_NR, 18);
        assert_eq!(SYSCALL_VM_UNMAP_NR, 19);
        assert_eq!(SYSCALL_CAP_RELEASE_NR, 20);
        assert_eq!(SYSCALL_DEBUG_SERIAL_WRITE_NR, 21);
        assert_eq!(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR, 22);
        assert_eq!(SYSCALL_COUNT, 23);
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
    fn legacy_ipc_v1_syscall_slots_return_invalid_number() {
        let mut state = Bootstrap::init().expect("kernel");
        for syscall_num in [
            SYSCALL_IPC_SEND_NR,
            SYSCALL_IPC_RECV_NR,
            SYSCALL_IPC_RECV_TIMEOUT_NR,
            SYSCALL_IPC_CALL_NR,
            SYSCALL_IPC_REPLY_NR,
        ] {
            let mut frame = TrapFrame::new(syscall_num, [0; 6]);
            assert_eq!(
                dispatch(&mut state, &mut frame),
                Err(SyscallError::InvalidNumber)
            );
        }
    }

    #[test]
    fn syscall_control_plane_set_cnode_slots_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR).expect("decode"),
            Syscall::ControlPlaneSetCnodeSlots
        );
    }

    #[test]
    fn syscall_ipc_v2_decode_is_stable() {
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_SEND_V2_NR).expect("decode"),
            Syscall::IpcSendV2
        );
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_RECV_V2_NR).expect("decode"),
            Syscall::IpcRecvV2
        );
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_CALL_V2_NR).expect("decode"),
            Syscall::IpcCallV2
        );
        assert_eq!(
            Syscall::decode(SYSCALL_IPC_REPLY_V2_NR).expect("decode"),
            Syscall::IpcReplyV2
        );
        assert_eq!(
            Syscall::decode(SYSCALL_DEBUG_SERIAL_WRITE_NR).expect("decode"),
            Syscall::DebugSerialWrite
        );
        assert_eq!(
            Syscall::decode(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR).expect("decode"),
            Syscall::DebugSerialWriteBuf
        );
    }

    #[test]
    fn syscall_debug_serial_rejects_nonzero_reserved_args() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_NR, [b'A' as usize, 1, 0, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_debug_serial_truncates_arg0_to_low_byte_when_enabled() {
        if !DEBUG_SERIAL_SYSCALL_ENABLED {
            return;
        }
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_NR, [0x1ff, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("debug serial write");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 1);
    }

    #[test]
    fn syscall_debug_serial_is_noop_success_outside_debug_builds() {
        if DEBUG_SERIAL_SYSCALL_ENABLED {
            return;
        }
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_NR, [b'A' as usize, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("no-op success");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0);
    }

    #[test]
    fn syscall_debug_serial_buf_rejects_nonzero_reserved_args() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR, [0x6000, 2, 1, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_debug_serial_buf_rejects_zero_len() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR, [0x6000, 0, 0, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_debug_serial_buf_rejects_oversize_len() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(
            SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR,
            [0x6000, DEBUG_SERIAL_WRITE_BUF_MAX_LEN + 1, 0, 0, 0, 0],
        );
        assert_eq!(dispatch(&mut state, &mut frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_debug_serial_buf_rejects_invalid_user_pointer() {
        let mut state = Bootstrap::init().expect("kernel");
        let kernel_ptr = crate::arch::vm_layout::KERNEL_SPACE_BASE as usize;
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR, [kernel_ptr, 1, 0, 0, 0, 0]);
        assert_eq!(dispatch(&mut state, &mut frame), Err(SyscallError::InvalidArgs));
    }

    #[test]
    fn syscall_debug_serial_buf_accepts_valid_user_buffer() {
        let mut state = Bootstrap::init().expect("kernel");
        state
            .copy_to_current_user(0x6500, b"ok")
            .expect("write user bytes");
        let mut frame = TrapFrame::new(SYSCALL_DEBUG_SERIAL_WRITE_BUF_NR, [0x6500, 2, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("buffer write");
        assert_eq!(frame.error_code(), None);
        if DEBUG_SERIAL_SYSCALL_ENABLED {
            assert_eq!(frame.ret0(), 1);
        } else {
            assert_eq!(frame.ret0(), 0);
        }
    }

    #[test]
    fn syscall_ipc_v2_stubs_return_invalid_args() {
        let mut state = Bootstrap::init().expect("kernel");
        for nr in [
            SYSCALL_IPC_SEND_V2_NR,
            SYSCALL_IPC_RECV_V2_NR,
            SYSCALL_IPC_CALL_V2_NR,
            SYSCALL_IPC_REPLY_V2_NR,
        ] {
            let mut frame = TrapFrame::new(nr, [0, 0, 0, 0, 0, 0]);
            let err = dispatch(&mut state, &mut frame).expect_err("v2 stub error");
            assert_eq!(err, SyscallError::InvalidArgs);
        }
    }

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
        let bytes = legacy_inline_payload_from_recv_frame(&recv, recv.ret1());
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
        let bytes = legacy_inline_payload_from_recv_frame(&recv, recv.ret1());
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
    fn vm_anon_map_syscall_maps_region_and_returns_memory_object_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");
        let mut frame = TrapFrame::new(
            Syscall::VmAnonMap as usize,
            [
                0x8000,
                PAGE_SIZE + 1,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut frame).expect("vm_anon_map");
        assert_eq!(frame.error_code(), None);
        assert_eq!(frame.ret0(), 0x8000);
        assert_eq!(frame.ret1(), PAGE_SIZE * 2);
        let cap = CapId(frame.ret2() as u64);
        let resolved = state
            .capability_service()
            .resolve_current_task_capability(cap)
            .expect("resolved cap");
        assert!(matches!(resolved.object, CapObject::MemoryObject { .. }));
    }

    #[test]
    fn vm_anon_map_rejects_invalid_args() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");

        let cases = [
            [0, PAGE_SIZE, SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE, 0, 0, 0],
            [0x8101, PAGE_SIZE, SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE, 0, 0, 0],
            [0x9000, 0, SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE, 0, 0, 0],
            [0xA000, PAGE_SIZE, SYSCALL_VM_MAP_PROT_READ, 0, 0, 0],
            [0xB000, PAGE_SIZE, SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE | SYSCALL_VM_MAP_PROT_EXEC, 0, 0, 0],
            [0xC000, PAGE_SIZE, SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE, 1, 0, 0],
        ];
        for args in cases {
            let mut frame = TrapFrame::new(Syscall::VmAnonMap as usize, args);
            let err = dispatch(&mut state, &mut frame).expect_err("invalid");
            assert_eq!(err, SyscallError::InvalidArgs);
        }
    }

    #[test]
    fn vm_anon_map_rejects_overlap_before_allocation() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");
        let mut occupy = TrapFrame::new(
            Syscall::VmMap as usize,
            [
                aspace_map_cap.0 as usize,
                0xD000,
                PAGE_SIZE,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut occupy).expect("occupy");
        let mem_before = state.with_memory_state(|memory| memory.memory_objects.iter().flatten().count());

        let mut frame = TrapFrame::new(
            Syscall::VmAnonMap as usize,
            [
                0xD000,
                PAGE_SIZE * 2,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
                0,
            ],
        );
        let err = dispatch(&mut state, &mut frame).expect_err("overlap");
        assert_eq!(err, SyscallError::WrongObject);
        let mem_after = state.with_memory_state(|memory| memory.memory_objects.iter().flatten().count());
        assert_eq!(mem_before, mem_after, "overlap reject must happen before allocation");
    }

    #[test]
    fn vm_unmap_after_vm_anon_map_allows_reuse_of_same_base() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");
        let mut map = TrapFrame::new(
            Syscall::VmAnonMap as usize,
            [
                0x20000,
                PAGE_SIZE,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut map).expect("map");
        let mut unmap = TrapFrame::new(
            Syscall::VmUnmap as usize,
            [0x20000, PAGE_SIZE, 0, 0, 0, 0],
        );
        dispatch(&mut state, &mut unmap).expect("unmap");
        let mut map_again = TrapFrame::new(
            Syscall::VmAnonMap as usize,
            [
                0x20000,
                PAGE_SIZE,
                SYSCALL_VM_MAP_PROT_READ | SYSCALL_VM_MAP_PROT_WRITE,
                0,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut map_again).expect("map again");
    }

    #[test]
    fn vm_unmap_rejects_invalid_args() {
        let mut state = Bootstrap::init().expect("kernel");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");
        let cases = [
            [0, PAGE_SIZE, 0, 0, 0, 0],
            [0x20001, PAGE_SIZE, 0, 0, 0, 0],
            [0x21000, 0, 0, 0, 0, 0],
            [0x22000, PAGE_SIZE, 1, 0, 0, 0],
        ];
        for args in cases {
            let mut frame = TrapFrame::new(Syscall::VmUnmap as usize, args);
            let err = dispatch(&mut state, &mut frame).expect_err("invalid");
            assert_eq!(err, SyscallError::InvalidArgs);
        }
    }

    #[test]
    fn cap_release_revokes_current_task_cap() {
        let mut state = Bootstrap::init().expect("kernel");
        let cap = state
            .mint_capability_for_current_context(Capability::new(CapObject::Kernel, CapRights::READ))
            .expect("mint");
        let resolved_before = state
            .capability_service()
            .resolve_current_task_capability(cap);
        assert!(resolved_before.is_some());
        let mut frame = TrapFrame::new(Syscall::CapRelease as usize, [cap.0 as usize, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut frame).expect("cap_release");
        let resolved_after = state
            .capability_service()
            .resolve_current_task_capability(cap);
        assert!(resolved_after.is_none());
    }

    #[test]
    fn cap_release_invalid_cap_returns_invalid_capability() {
        let mut state = Bootstrap::init().expect("kernel");
        let mut frame = TrapFrame::new(Syscall::CapRelease as usize, [99999, 0, 0, 0, 0, 0]);
        let err = dispatch(&mut state, &mut frame).expect_err("invalid");
        assert_eq!(err, SyscallError::InvalidCapability);
    }

    #[test]
    fn syscall_error_codes_are_stable() {
        assert_eq!(SyscallError::InvalidNumber.code(), 1);
        assert_eq!(SyscallError::InvalidArgs.code(), 2);
        assert_eq!(SyscallError::BufferTooSmall.code(), 10);
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
                17,
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
        assert_eq!(region.len as usize, 17);
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
