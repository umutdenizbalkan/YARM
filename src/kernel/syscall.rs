use super::bootstrap::{KernelError, KernelState};
use super::capabilities::CapId;
use super::ipc::Message;
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;
use super::vm::VirtAddr;

pub const SYSCALL_ABI_VERSION: u16 = 1;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_COUNT: usize = 3;
const _: [(); SYSCALL_COUNT] = [(); 3];
pub const SYSCALL_ARG_CAP: usize = 0;
pub const SYSCALL_ARG_PTR: usize = 1;
pub const SYSCALL_ARG_LEN: usize = 2;
pub const SYSCALL_ARG_INLINE_PAYLOAD: usize = 3;
pub const SYSCALL_RET_STATUS: usize = 0;
pub const SYSCALL_RET_AUX: usize = 1;

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

fn unpack_payload(word: usize, len: usize) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    if len > core::mem::size_of::<usize>() || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    let mut payload = [0u8; Message::MAX_PAYLOAD];
    let bytes = word.to_le_bytes();
    payload[..len].copy_from_slice(&bytes[..len]);
    Ok(payload)
}

fn pack_payload(bytes: &[u8]) -> usize {
    let mut word = [0u8; core::mem::size_of::<usize>()];
    let copy_len = if bytes.len() > word.len() {
        word.len()
    } else {
        bytes.len()
    };

    word[..copy_len].copy_from_slice(&bytes[..copy_len]);

    usize::from_le_bytes(word)
}

fn sender_tid_to_ret(tid: u64) -> Result<usize, SyscallError> {
    usize::try_from(tid).map_err(|_| SyscallError::Internal)
}

fn handle_ipc_send(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    // Untrusted userspace value: capability checks are enforced by lookup paths.
    let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
    let user_ptr = frame.args[SYSCALL_ARG_PTR];
    let len = frame.args[SYSCALL_ARG_LEN];

    // Sender identity is kernel-supplied and never taken from userspace args.
    let sender_tid = kernel
        .scheduler
        .current_tid()
        .ok_or(SyscallError::Internal)?;
    let payload = if kernel.task_asid(sender_tid).is_some() {
        match kernel.copy_from_current_user(user_ptr, len) {
            Ok(payload) => payload,
            Err(KernelError::UserMemoryFault) => {
                kernel.record_fault(FaultInfo {
                    addr: VirtAddr(user_ptr as u64),
                    access: FaultAccess::Read,
                });
                frame.set_err(SyscallError::PageFault.code());
                return Ok(());
            }
            Err(other) => return Err(SyscallError::from(other)),
        }
    } else {
        let payload_word = frame.args[SYSCALL_ARG_INLINE_PAYLOAD];
        unpack_payload(payload_word, len)?
    };

    let msg = Message::with_header(sender_tid, 0, 0, None, &payload[..len])
        .map_err(|_| SyscallError::InvalidArgs)?;

    kernel.ipc_send(cap, msg).map_err(SyscallError::from)?;
    frame.set_ok(0, 0);
    Ok(())
}

fn handle_ipc_recv(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    // Untrusted userspace value: capability checks are enforced by lookup paths.
    let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
    let user_ptr = frame.args[SYSCALL_ARG_PTR];
    let user_len = frame.args[SYSCALL_ARG_LEN];
    let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;

    match received {
        Some(msg) => {
            let sender = sender_tid_to_ret(msg.sender_tid.0)?;
            let current_tid = kernel
                .scheduler
                .current_tid()
                .ok_or(SyscallError::Internal)?;

            if kernel.task_asid(current_tid).is_some() {
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
                frame.ret1 = pack_payload(msg.as_slice());
            }
        }
        None => frame.set_err(SyscallError::WouldBlock.code()),
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

    #[test]
    fn syscall_decode_rejects_unknown_number() {
        assert_eq!(Syscall::decode(99), Err(SyscallError::InvalidNumber));
    }

    #[test]
    fn syscall_abi_numbers_are_frozen() {
        assert_eq!(SYSCALL_ABI_VERSION, 1);
        assert_eq!(SYSCALL_COUNT, 3);
        assert_eq!(Syscall::Yield.number(), SYSCALL_YIELD_NR);
        assert_eq!(Syscall::IpcSend.number(), SYSCALL_IPC_SEND_NR);
        assert_eq!(Syscall::IpcRecv.number(), SYSCALL_IPC_RECV_NR);
        assert_eq!(Syscall::decode(SYSCALL_YIELD_NR), Ok(Syscall::Yield));
        assert_eq!(Syscall::decode(SYSCALL_IPC_SEND_NR), Ok(Syscall::IpcSend));
        assert_eq!(Syscall::decode(SYSCALL_IPC_RECV_NR), Ok(Syscall::IpcRecv));
        assert_eq!(SYSCALL_ARG_CAP, 0);
        assert_eq!(SYSCALL_ARG_PTR, 1);
        assert_eq!(SYSCALL_ARG_LEN, 2);
        assert_eq!(SYSCALL_ARG_INLINE_PAYLOAD, 3);
        assert_eq!(SYSCALL_RET_STATUS, 0);
        assert_eq!(SYSCALL_RET_AUX, 1);
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
    fn unpack_payload_boundaries() {
        let p0 = unpack_payload(0xAA_BB_CC_DD, 0).expect("len 0");
        assert_eq!(&p0[..1], &[0]);

        let pmax = unpack_payload(0x0807_0605_0403_0201, core::mem::size_of::<usize>())
            .expect("register-sized");
        assert_eq!(pmax[0], 0x01);

        assert_eq!(
            unpack_payload(0, core::mem::size_of::<usize>() + 1),
            Err(SyscallError::InvalidArgs)
        );
    }

    #[test]
    fn pack_payload_boundaries() {
        assert_eq!(pack_payload(&[]), 0);
        assert_eq!(
            pack_payload(&[1, 2, 3, 4]),
            usize::from_le_bytes([1, 2, 3, 4, 0, 0, 0, 0])
        );
        let packed = pack_payload(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(packed, usize::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));
    }

    #[test]
    fn kernel_error_mapping_is_explicit() {
        assert_eq!(
            SyscallError::from(KernelError::InvalidCapability),
            SyscallError::InvalidCapability
        );
        assert_eq!(
            SyscallError::from(KernelError::MissingRight),
            SyscallError::MissingRight
        );
        assert_eq!(
            SyscallError::from(KernelError::WrongObject),
            SyscallError::WrongObject
        );
        assert_eq!(
            SyscallError::from(KernelError::StaleCapability),
            SyscallError::WrongObject
        );
        assert_eq!(
            SyscallError::from(KernelError::EndpointQueueFull),
            SyscallError::QueueFull
        );
        assert_eq!(
            SyscallError::from(KernelError::UserMemoryFault),
            SyscallError::PageFault
        );
        assert_eq!(
            SyscallError::from(KernelError::WouldBlock),
            SyscallError::WouldBlock
        );
    }
}
