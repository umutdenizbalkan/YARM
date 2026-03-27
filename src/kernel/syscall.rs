use super::boot::{KernelError, KernelState};
use super::capabilities::{CapId, CapObject, CapRights};
use super::ipc::{
    IPC_REGISTER_BYTES, Message, SharedMemoryRegion, pack_register_payload, unpack_register_payload,
};
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;
use super::vm::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::arch::syscall_abi;

pub const SYSCALL_ABI_VERSION: u16 = 4;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_VM_MAP_NR: usize = 3;
pub const SYSCALL_COUNT: usize = 4;
const _: [(); SYSCALL_COUNT] = [(); 4];
pub const SYSCALL_ARG_CAP: usize = 0;
pub const SYSCALL_ARG_PTR: usize = 1;
pub const SYSCALL_ARG_LEN: usize = 2;
pub const SYSCALL_ARG_INLINE_PAYLOAD0: usize = 3;
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
}

impl Syscall {
    pub const VARIANT_COUNT: usize = 4;
    pub const fn number(self) -> usize {
        self as usize
    }

    pub fn decode(raw: usize) -> Result<Self, SyscallError> {
        match raw {
            SYSCALL_YIELD_NR => Ok(Self::Yield),
            SYSCALL_IPC_SEND_NR => Ok(Self::IpcSend),
            SYSCALL_IPC_RECV_NR => Ok(Self::IpcRecv),
            SYSCALL_VM_MAP_NR => Ok(Self::VmMap),
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
    let derived = kernel
        .grant_capability_task_to_task(envelope.source_tid.0, envelope.source_cap, receiver_tid)
        .map_err(SyscallError::from)?;
    Ok(Some(derived.0))
}

fn validate_user_region(offset: u64, len: u64) -> Result<(), SyscallError> {
    const USER_ADDR_MAX: u64 = crate::arch::vm_layout::KERNEL_SPACE_BASE - 1;
    let end = offset.checked_add(len).ok_or(SyscallError::InvalidArgs)?;
    if end > USER_ADDR_MAX {
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
        .current_task_capability(cap)
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
    if kernel.current_task_capability(cap).is_none() {
        return Err(SyscallError::InvalidCapability);
    }
    Ok(())
}

fn stash_transfer_handle(
    kernel: &mut KernelState,
    transfer_cap: Option<CapId>,
    endpoint: CapObject,
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

fn handle_ipc_send(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.arg(SYSCALL_ARG_CAP) as u64);
    validate_endpoint_right(kernel, cap, CapRights::SEND)?;
    let endpoint = kernel
        .current_task_capability(cap)
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
                .current_task_capability(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            validate_user_region(user_ptr_or_offset as u64, len as u64)?;
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint)?;
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

            let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint)?;
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
        let payload = inline_payload_from_frame(frame, len)?;
        let transfer_handle = stash_transfer_handle(kernel, transfer_cap, endpoint)?;
        Message::with_header(
            sender_tid,
            OPCODE_INLINE,
            transfer_flag_bits(transfer_cap),
            transfer_handle,
            &payload[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)
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
        .current_task_capability(cap)
        .ok_or(SyscallError::InvalidCapability)?
        .object;
    let user_ptr = frame.arg(SYSCALL_ARG_PTR);
    let user_len = frame.arg(SYSCALL_ARG_LEN);
    let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;

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
                    frame.set_ok(sender, region_len, frame.ret2());
                    frame.set_arg(
                        SYSCALL_ARG_INLINE_PAYLOAD0,
                        usize::try_from(desc.offset).map_err(|_| SyscallError::InvalidArgs)?,
                    );
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
            frame.set_err(SyscallError::WouldBlock.code());
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

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    match Syscall::decode(frame.syscall_num())? {
        Syscall::Yield => {
            kernel.yield_current().map_err(SyscallError::from)?;
            frame.set_ok(0, 0, 0);
            Ok(())
        }
        Syscall::IpcSend => handle_ipc_send(kernel, frame),
        Syscall::IpcRecv => handle_ipc_recv(kernel, frame),
        Syscall::VmMap => handle_vm_map(kernel, frame),
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
        assert_eq!(SYSCALL_ABI_VERSION, 4);
        assert_eq!(SYSCALL_ARG_TRANSFER_CAP, 5);
        assert_eq!(SYSCALL_RET_TRANSFER_CAP, 2);
        assert_eq!(IPC_REGISTER_WORDS, 2);
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
            .duplicate_global_capability_to_task(1, recv_cap_global)
            .expect("dup recv cap");
        let (_mem_id, mem_cap) = state
            .create_memory_object(crate::kernel::vm::PhysAddr(0x7000))
            .expect("mem");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert!(
            state.current_task_capability_has_right(recv_cap, CapRights::RECEIVE),
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
            .current_task_capability(recv_local)
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
            .duplicate_global_capability_to_task(1, recv1)
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
            .duplicate_global_capability_to_task(1, recv_cap)
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
