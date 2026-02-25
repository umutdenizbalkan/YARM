use super::bootstrap::{KernelError, KernelState};
use super::capabilities::CapId;
use super::ipc::Message;
use super::trap::{FaultAccess, FaultInfo};
use super::trapframe::TrapFrame;

pub const SYSCALL_ABI_VERSION: u16 = 1;
pub const SYSCALL_YIELD_NR: usize = 0;
pub const SYSCALL_IPC_SEND_NR: usize = 1;
pub const SYSCALL_IPC_RECV_NR: usize = 2;
pub const SYSCALL_COUNT: usize = 3;
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
            KernelError::InvalidCapability => Self::InvalidCapability,
            KernelError::MissingRight => Self::MissingRight,
            KernelError::WrongObject | KernelError::StaleCapability => Self::WrongObject,
            KernelError::EndpointQueueFull => Self::QueueFull,
            KernelError::UserMemoryFault => Self::PageFault,
            KernelError::WouldBlock => Self::WouldBlock,
            _ => Self::Internal,
        }
    }
}

fn unpack_payload(word: usize, len: usize) -> Result<[u8; Message::MAX_PAYLOAD], SyscallError> {
    if len > 8 || len > Message::MAX_PAYLOAD {
        return Err(SyscallError::InvalidArgs);
    }

    let mut payload = [0u8; Message::MAX_PAYLOAD];
    let bytes = word.to_le_bytes();
    let mut i = 0;
    while i < len {
        payload[i] = bytes[i];
        i += 1;
    }
    Ok(payload)
}

fn pack_payload(bytes: &[u8]) -> usize {
    let mut word = [0u8; core::mem::size_of::<usize>()];
    let copy_len = if bytes.len() > word.len() {
        word.len()
    } else {
        bytes.len()
    };

    let mut i = 0;
    while i < copy_len {
        word[i] = bytes[i];
        i += 1;
    }

    usize::from_le_bytes(word)
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) -> Result<(), SyscallError> {
    match Syscall::decode(frame.syscall_num)? {
        Syscall::Yield => {
            kernel.yield_current().map_err(SyscallError::from)?;
            frame.set_ok(0, 0);
            Ok(())
        }
        Syscall::IpcSend => {
            let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
            let user_ptr = frame.args[SYSCALL_ARG_PTR];
            let len = frame.args[SYSCALL_ARG_LEN];

            let sender_tid = kernel
                .scheduler
                .current_tid()
                .ok_or(SyscallError::Internal)?;
            let payload = if kernel.task_asid(sender_tid).is_some() {
                match kernel.copy_from_current_user(user_ptr, len) {
                    Ok(payload) => payload,
                    Err(KernelError::UserMemoryFault) => {
                        kernel.record_fault(FaultInfo {
                            addr: user_ptr,
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
        Syscall::IpcRecv => {
            let cap = CapId(frame.args[SYSCALL_ARG_CAP] as u64);
            let user_ptr = frame.args[SYSCALL_ARG_PTR];
            let user_len = frame.args[SYSCALL_ARG_LEN];
            let received = kernel.ipc_recv(cap).map_err(SyscallError::from)?;

            match received {
                Some(msg) => {
                    let current_tid = kernel
                        .scheduler
                        .current_tid()
                        .ok_or(SyscallError::Internal)?;

                    if kernel.task_asid(current_tid).is_some() {
                        if user_len < msg.len as usize {
                            return Err(SyscallError::InvalidArgs);
                        }
                        match kernel.copy_to_current_user(user_ptr, msg.as_slice()) {
                            Ok(()) => frame.set_ok(msg.sender_tid as usize, msg.len as usize),
                            Err(KernelError::UserMemoryFault) => {
                                kernel.record_fault(FaultInfo {
                                    addr: user_ptr,
                                    access: FaultAccess::Write,
                                });
                                frame.set_err(SyscallError::PageFault.code());
                                return Ok(());
                            }
                            Err(other) => return Err(SyscallError::from(other)),
                        };
                        frame.ret1 = 0;
                    } else {
                        frame.set_ok(msg.sender_tid as usize, msg.len as usize);
                        frame.ret1 = pack_payload(msg.as_slice());
                    }
                }
                None => frame.set_err(SyscallError::WouldBlock.code()),
            }
            Ok(())
        }
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
}
