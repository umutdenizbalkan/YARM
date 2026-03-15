use super::bootstrap::{KernelError, KernelState};
use super::capabilities::{CapId, CapObject};
use super::ipc::{
    IPC_REGISTER_BYTES, Message, SharedMemoryRegion, pack_register_payload, unpack_register_payload,
};
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;
use super::vm::VirtAddr;
use crate::arch::syscall_abi;

pub const SYSCALL_ABI_VERSION: u16 = 2;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_COUNT: usize = 3;
const _: [(); SYSCALL_COUNT] = [(); 3];
pub const SYSCALL_ARG_CAP: usize = 0;
pub const SYSCALL_ARG_PTR: usize = 1;
pub const SYSCALL_ARG_LEN: usize = 2;
pub const SYSCALL_ARG_INLINE_PAYLOAD0: usize = 3;
pub const SYSCALL_ARG_INLINE_PAYLOAD1: usize = 4;
pub const SYSCALL_ARG_TRANSFER_CAP: usize = syscall_abi::TRAPFRAME_ARG_REGS - 1;
pub const SYSCALL_RET_STATUS: usize = 0;
pub const SYSCALL_RET_AUX: usize = 1;
pub const SYSCALL_RET_TRANSFER_CAP: usize = 2;
pub const SYSCALL_NO_TRANSFER_CAP: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Syscall {
    Yield = SYSCALL_YIELD_NR,
    IpcSend = SYSCALL_IPC_SEND_NR,
    IpcRecv = SYSCALL_IPC_RECV_NR,
}

impl Syscall {
    pub const VARIANT_COUNT: usize = 3;
    pub const fn number(self) -> usize {
        self as usize
    }

    pub fn decode(raw: usize) -> Result<Self, SyscallError> {
        match raw {
            SYSCALL_YIELD_NR => Ok(Self::Yield),
            SYSCALL_IPC_SEND_NR => Ok(Self::IpcSend),
            SYSCALL_IPC_RECV_NR => Ok(Self::IpcRecv),
            _ => Err(SyscallError::InvalidNumber),
        }
    }
}

const _: [(); SYSCALL_COUNT] = [(); Syscall::VARIANT_COUNT];
const _: [(); syscall_abi::TRAPFRAME_ARG_REGS] = [(); 6];

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

fn sender_tid_to_ret(tid: u64) -> Result<usize, SyscallError> {
    usize::try_from(tid).map_err(|_| SyscallError::Internal)
}

fn transfer_cap_arg(frame: &TrapFrame) -> Result<Option<CapId>, SyscallError> {
    let raw = frame.args[SYSCALL_ARG_TRANSFER_CAP] as u64;
    if raw == SYSCALL_NO_TRANSFER_CAP || raw == 0 {
        return Ok(None);
    }
    let cap = CapId(raw);
    Ok(Some(cap))
}

fn encode_transfer_cap_ret(frame: &mut TrapFrame, cap: Option<u64>) -> Result<(), SyscallError> {
    let value = cap.unwrap_or(SYSCALL_NO_TRANSFER_CAP);
    frame.ret2 = usize::try_from(value).map_err(|_| SyscallError::Internal)?;
    Ok(())
}

fn validate_transfer_cap(kernel: &KernelState, cap: CapId) -> Result<(), SyscallError> {
    if kernel.cspace.get(cap).is_none() {
        return Err(SyscallError::InvalidCapability);
    }
    Ok(())
}

fn inline_payload_from_frame(
    frame: &TrapFrame,
    len: usize,
) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    if len > IPC_REGISTER_BYTES || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }
    let words = [
        frame.args[SYSCALL_ARG_INLINE_PAYLOAD0],
        frame.args[SYSCALL_ARG_INLINE_PAYLOAD1],
    ];
    let regs = unpack_register_payload(words, len).ok_or(SyscallError::InvalidArgs)?;
    let mut payload = [0u8; Message::MAX_PAYLOAD];
    payload[..len].copy_from_slice(&regs[..len]);
    Ok(payload)
}

fn handle_ipc_send(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
    let user_ptr_or_offset = frame.args[SYSCALL_ARG_PTR];
    let len = frame.args[SYSCALL_ARG_LEN];
    let transfer_cap = transfer_cap_arg(frame)?;
    if let Some(c) = transfer_cap {
        validate_transfer_cap(kernel, c)?;
    }

    let sender_tid = kernel
        .scheduler
        .current_tid()
        .ok_or(SyscallError::Internal)?;

    let msg = if kernel.task_asid(sender_tid).is_some() {
        if len > Message::MAX_PAYLOAD {
            let grant_cap = transfer_cap.ok_or(SyscallError::InvalidArgs)?;
            let grant = kernel
                .cspace
                .get(grant_cap)
                .ok_or(SyscallError::InvalidCapability)?;
            match grant.object {
                CapObject::MemoryObject { .. } | CapObject::DmaRegion { .. } => {}
                _ => return Err(SyscallError::WrongObject),
            }
            let region = SharedMemoryRegion {
                offset: user_ptr_or_offset as u64,
                len: len as u64,
            };
            Message::with_header(
                sender_tid,
                0,
                Message::FLAG_CAP_TRANSFER,
                Some(grant_cap.0),
                &region.encode(),
            )
            .map_err(|_| SyscallError::InvalidArgs)?
        } else {
            let payload = match kernel.copy_from_current_user(user_ptr_or_offset, len) {
                Ok(payload) => payload,
                Err(KernelError::UserMemoryFault) => {
                    kernel.record_fault(FaultInfo {
                        addr: VirtAddr(user_ptr_or_offset as u64),
                        access: FaultAccess::Read,
                    });
                    frame.set_err(SyscallError::PageFault.code());
                    return Ok(());
                }
                Err(other) => return Err(SyscallError::from(other)),
            };

            Message::with_header(
                sender_tid,
                0,
                if transfer_cap.is_some() {
                    Message::FLAG_CAP_TRANSFER
                } else {
                    0
                },
                transfer_cap.map(|c| c.0),
                &payload[..len],
            )
            .map_err(|_| SyscallError::InvalidArgs)?
        }
    } else {
        let payload = inline_payload_from_frame(frame, len)?;
        Message::with_header(
            sender_tid,
            0,
            if transfer_cap.is_some() {
                Message::FLAG_CAP_TRANSFER
            } else {
                0
            },
            transfer_cap.map(|c| c.0),
            &payload[..len],
        )
        .map_err(|_| SyscallError::InvalidArgs)?
    };

    kernel.ipc_send(cap, msg).map_err(SyscallError::from)?;
    frame.set_ok(0, 0);
    encode_transfer_cap_ret(frame, None)?;
    Ok(())
}

fn handle_ipc_recv(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
    let user_ptr = frame.args[SYSCALL_ARG_PTR];
    let user_len = frame.args[SYSCALL_ARG_LEN];
    let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;

    match received {
        Some(msg) => {
            let sender = sender_tid_to_ret(msg.sender_tid.0)?;
            encode_transfer_cap_ret(frame, msg.transferred_cap().map(|c| c.0))?;

            let current_tid = kernel
                .scheduler
                .current_tid()
                .ok_or(SyscallError::Internal)?;
            if kernel.task_asid(current_tid).is_some() {
                if msg.transferred_cap().is_some()
                    && msg.as_slice().len() == SharedMemoryRegion::ENCODED_LEN
                {
                    let desc = SharedMemoryRegion::decode(msg.as_slice())
                        .ok_or(SyscallError::InvalidArgs)?;
                    let region_len =
                        usize::try_from(desc.len).map_err(|_| SyscallError::InvalidArgs)?;
                    frame.set_ok(sender, region_len);
                    frame.args[SYSCALL_ARG_INLINE_PAYLOAD0] =
                        usize::try_from(desc.offset).map_err(|_| SyscallError::InvalidArgs)?;
                    frame.args[SYSCALL_ARG_INLINE_PAYLOAD1] = region_len;
                    return Ok(());
                }

                if user_len < msg.len as usize {
                    return Err(SyscallError::InvalidArgs);
                }
                match kernel.copy_to_current_user(user_ptr, msg.as_slice()) {
                    Ok(()) => frame.set_ok(sender, msg.len as usize),
                    Err(KernelError::UserMemoryFault) => {
                        kernel.record_fault(FaultInfo {
                            addr: VirtAddr(user_ptr as u64),
                            access: FaultAccess::Write,
                        });
                        frame.set_err(SyscallError::PageFault.code());
                        return Ok(());
                    }
                    Err(other) => return Err(SyscallError::from(other)),
                };
            } else {
                frame.set_ok(sender, msg.len as usize);
                let words = pack_register_payload(msg.as_slice());
                frame.args[SYSCALL_ARG_INLINE_PAYLOAD0] = words[0];
                frame.args[SYSCALL_ARG_INLINE_PAYLOAD1] = words[1];
            }
        }
        None => {
            frame.set_err(SyscallError::WouldBlock.code());
            encode_transfer_cap_ret(frame, None)?;
        }
    }
    Ok(())
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    match Syscall::decode(frame.syscall_num)? {
        Syscall::Yield => {
            kernel.yield_current().map_err(SyscallError::from)?;
            frame.set_ok(0, 0);
            Ok(())
        }
        Syscall::IpcSend => handle_ipc_send(kernel, frame),
        Syscall::IpcRecv => handle_ipc_recv(kernel, frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::IPC_REGISTER_WORDS;

    #[test]
    fn syscall_abi_numbers_are_frozen() {
        assert_eq!(SYSCALL_ABI_VERSION, 2);
        assert_eq!(SYSCALL_ARG_TRANSFER_CAP, 5);
        assert_eq!(SYSCALL_RET_TRANSFER_CAP, 2);
        assert_eq!(IPC_REGISTER_WORDS, 2);
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
}
