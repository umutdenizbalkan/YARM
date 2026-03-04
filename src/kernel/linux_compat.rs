use super::bootstrap::{KernelError, KernelState};
use super::capabilities::CapId;
use super::trapframe::TrapFrame;
use super::vm::{PAGE_SIZE, PageFlags, VirtAddr};

pub const LINUX_COMPAT_ABI_VERSION: u16 = 1;
pub const LINUX_COMPAT_SYSCALL_COUNT: usize = 7;

pub const LINUX_NR_BRK: usize = 214;
pub const LINUX_NR_MUNMAP: usize = 215;
pub const LINUX_NR_MMAP: usize = 222;
pub const LINUX_NR_MPROTECT: usize = 226;
pub const LINUX_NR_GETPID: usize = 172;
pub const LINUX_NR_EXIT: usize = 93;
pub const LINUX_NR_GETPPID: usize = 173;

const PROC_OP_GETPID: u16 = 1;
const PROC_OP_EXIT: u16 = 2;
const PROC_OP_GETPPID: u16 = 3;

pub const PROT_READ: usize = 0x1;
pub const PROT_WRITE: usize = 0x2;
pub const PROT_EXEC: usize = 0x4;

pub const EINVAL: i32 = 22;
pub const EPERM: i32 = 1;
pub const ENOMEM: i32 = 12;
pub const ENOSYS: i32 = 38;

const LINUX_BRK_DEFAULT_BASE: usize = 0x4000_0000;
const LINUX_ARG0: usize = 0;
const LINUX_ARG1: usize = 1;
const LINUX_ARG2: usize = 2;
const LINUX_ARG3: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxErrno {
    Inval,
    Perm,
    NoMem,
    NoSys,
}

impl LinuxErrno {
    pub const fn code(self) -> i32 {
        match self {
            Self::Inval => EINVAL,
            Self::Perm => EPERM,
            Self::NoMem => ENOMEM,
            Self::NoSys => ENOSYS,
        }
    }

    pub const fn neg_code(self) -> isize {
        -(self.code() as isize)
    }
}

impl From<KernelError> for LinuxErrno {
    fn from(value: KernelError) -> Self {
        match value {
            KernelError::MissingRight => Self::Perm,
            KernelError::VmFull | KernelError::TaskTableFull | KernelError::MemoryObjectFull => {
                Self::NoMem
            }
            KernelError::WrongObject
            | KernelError::InvalidCapability
            | KernelError::MemoryObjectMissing
            | KernelError::Vm(_)
            | KernelError::UserMemoryFault => Self::Inval,
            _ => Self::NoSys,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum LinuxCompatSyscall {
    Exit = LINUX_NR_EXIT,
    Getpid = LINUX_NR_GETPID,
    Getppid = LINUX_NR_GETPPID,
    Brk = LINUX_NR_BRK,
    Munmap = LINUX_NR_MUNMAP,
    Mmap = LINUX_NR_MMAP,
    Mprotect = LINUX_NR_MPROTECT,
}

impl LinuxCompatSyscall {
    const DISPATCH_TABLE: [usize; LINUX_COMPAT_SYSCALL_COUNT] = [
        LINUX_NR_EXIT,
        LINUX_NR_GETPID,
        LINUX_NR_GETPPID,
        LINUX_NR_BRK,
        LINUX_NR_MUNMAP,
        LINUX_NR_MMAP,
        LINUX_NR_MPROTECT,
    ];

    pub fn decode(raw: usize) -> Result<Self, LinuxErrno> {
        if !Self::DISPATCH_TABLE.contains(&raw) {
            return Err(LinuxErrno::NoSys);
        }

        match raw {
            LINUX_NR_EXIT => Ok(Self::Exit),
            LINUX_NR_GETPID => Ok(Self::Getpid),
            LINUX_NR_GETPPID => Ok(Self::Getppid),
            LINUX_NR_BRK => Ok(Self::Brk),
            LINUX_NR_MUNMAP => Ok(Self::Munmap),
            LINUX_NR_MMAP => Ok(Self::Mmap),
            LINUX_NR_MPROTECT => Ok(Self::Mprotect),
            _ => Err(LinuxErrno::NoSys),
        }
    }
}

fn round_up_page(value: usize) -> usize {
    if value.is_multiple_of(PAGE_SIZE) {
        value
    } else {
        (value + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    }
}

fn prot_to_page_flags(prot: usize) -> Result<PageFlags, LinuxErrno> {
    if prot == 0 {
        return Err(LinuxErrno::Inval);
    }

    Ok(PageFlags {
        read: (prot & PROT_READ) != 0,
        write: (prot & PROT_WRITE) != 0,
        execute: (prot & PROT_EXEC) != 0,
        user: true,
    })
}

impl KernelState {
    pub fn linux_mmap_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<usize, LinuxErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(LinuxErrno::Inval);
        }

        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len))
            .ok_or(LinuxErrno::Inval)?;
        let mut va = addr;
        while va < end {
            let (_, mem_cap) = self
                .alloc_anonymous_memory_object()
                .map_err(LinuxErrno::from)?;
            self.map_user_page_with_caps(aspace_map_cap, mem_cap, VirtAddr(va), flags)
                .map_err(LinuxErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(addr)
    }

    pub fn linux_munmap_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
    ) -> Result<(), LinuxErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(LinuxErrno::Inval);
        }
        let end = addr
            .checked_add(round_up_page(len))
            .ok_or(LinuxErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.unmap_user_page(aspace_map_cap, VirtAddr(va))
                .map_err(LinuxErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn linux_mprotect_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<(), LinuxErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(LinuxErrno::Inval);
        }
        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len))
            .ok_or(LinuxErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.protect_user_page(aspace_map_cap, VirtAddr(va), flags)
                .map_err(LinuxErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn linux_brk(
        &mut self,
        tid: u64,
        aspace_map_cap: CapId,
        requested_end: usize,
        prot: usize,
    ) -> Result<usize, LinuxErrno> {
        let (base, current_end) = self
            .task_brk_bounds(tid)
            .unwrap_or((LINUX_BRK_DEFAULT_BASE, LINUX_BRK_DEFAULT_BASE));

        if requested_end == 0 {
            return Ok(current_end);
        }
        if requested_end < base {
            return Err(LinuxErrno::Inval);
        }

        let current_rounded = round_up_page(current_end);
        let requested_rounded = round_up_page(requested_end);

        if requested_rounded > current_rounded {
            let map_start = current_rounded;
            let map_len = requested_rounded - map_start;
            if map_len > 0 {
                self.linux_mmap_region(aspace_map_cap, map_start, map_len, prot)?;
            }
        } else if requested_rounded < current_rounded {
            let unmap_len = current_rounded - requested_rounded;
            self.linux_munmap_region(aspace_map_cap, requested_rounded, unmap_len)?;
        }

        self.set_task_brk_bounds(tid, base, requested_end)
            .map_err(LinuxErrno::from)?;
        Ok(requested_end)
    }
}

pub fn dispatch(kernel: &mut KernelState, frame: &mut TrapFrame) {
    let result: Result<usize, LinuxErrno> = (|| match LinuxCompatSyscall::decode(frame.syscall_num)?
    {
        LinuxCompatSyscall::Exit => {
            let code = frame.args[LINUX_ARG0] as u64;
            kernel
                .send_linux_process_manager_request(PROC_OP_EXIT, code)
                .map_err(LinuxErrno::from)?;
            Ok(0)
        }
        LinuxCompatSyscall::Getpid => {
            let tid = kernel.scheduler.current_tid().ok_or(LinuxErrno::NoSys)?;
            kernel
                .send_linux_process_manager_request(PROC_OP_GETPID, tid)
                .map_err(LinuxErrno::from)?;
            let reply = kernel
                .recv_linux_process_manager_reply()
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            if reply.len < 8 {
                return Err(LinuxErrno::Inval);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&reply.as_slice()[..8]);
            Ok(u64::from_le_bytes(bytes) as usize)
        }
        LinuxCompatSyscall::Getppid => {
            let tid = kernel.scheduler.current_tid().ok_or(LinuxErrno::NoSys)?;
            kernel
                .send_linux_process_manager_request(PROC_OP_GETPPID, tid)
                .map_err(LinuxErrno::from)?;
            let reply = kernel
                .recv_linux_process_manager_reply()
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            if reply.len < 8 {
                return Err(LinuxErrno::Inval);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&reply.as_slice()[..8]);
            Ok(u64::from_le_bytes(bytes) as usize)
        }
        LinuxCompatSyscall::Mmap => {
            let aspace_cap = CapId(frame.args[LINUX_ARG0] as u64);
            let addr = frame.args[LINUX_ARG1];
            let len = frame.args[LINUX_ARG2];
            let prot = frame.args[LINUX_ARG3];
            kernel.linux_mmap_region(aspace_cap, addr, len, prot)
        }
        LinuxCompatSyscall::Munmap => {
            let aspace_cap = CapId(frame.args[LINUX_ARG0] as u64);
            let addr = frame.args[LINUX_ARG1];
            let len = frame.args[LINUX_ARG2];
            kernel.linux_munmap_region(aspace_cap, addr, len)?;
            Ok(0)
        }
        LinuxCompatSyscall::Mprotect => {
            let aspace_cap = CapId(frame.args[LINUX_ARG0] as u64);
            let addr = frame.args[LINUX_ARG1];
            let len = frame.args[LINUX_ARG2];
            let prot = frame.args[LINUX_ARG3];
            kernel.linux_mprotect_region(aspace_cap, addr, len, prot)?;
            Ok(0)
        }
        LinuxCompatSyscall::Brk => {
            let requested = frame.args[LINUX_ARG0];
            let aspace_cap = CapId(frame.args[LINUX_ARG1] as u64);
            let prot = frame.args[LINUX_ARG2];
            let tid = kernel.scheduler.current_tid().ok_or(LinuxErrno::NoSys)?;
            kernel.linux_brk(tid, aspace_cap, requested, prot)
        }
    })();

    match result {
        Ok(value) => frame.set_ok(value, 0),
        Err(errno) => frame.set_err(errno.code() as usize),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::ipc::Message;

    #[test]
    fn linux_compat_errno_mapping_stable() {
        assert_eq!(LinuxErrno::Inval.code(), EINVAL);
        assert_eq!(LinuxErrno::Perm.code(), EPERM);
        assert_eq!(LinuxErrno::NoMem.code(), ENOMEM);
        assert_eq!(LinuxErrno::NoSys.code(), ENOSYS);
    }

    #[test]
    fn linux_compat_abi_contract_is_frozen() {
        assert_eq!(LINUX_COMPAT_ABI_VERSION, 1);
        assert_eq!(LINUX_COMPAT_SYSCALL_COUNT, 7);
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_EXIT),
            Ok(LinuxCompatSyscall::Exit)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_GETPID),
            Ok(LinuxCompatSyscall::Getpid)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_GETPPID),
            Ok(LinuxCompatSyscall::Getppid)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_BRK),
            Ok(LinuxCompatSyscall::Brk)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_MUNMAP),
            Ok(LinuxCompatSyscall::Munmap)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_MMAP),
            Ok(LinuxCompatSyscall::Mmap)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_MPROTECT),
            Ok(LinuxCompatSyscall::Mprotect)
        );
    }

    #[test]
    fn linux_multi_page_vm_wrappers_work() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let addr = state
            .linux_mmap_region(aspace_cap, 0x4000, PAGE_SIZE * 3, PROT_READ | PROT_WRITE)
            .expect("mmap");
        assert_eq!(addr, 0x4000);

        state
            .linux_mprotect_region(aspace_cap, 0x4000, PAGE_SIZE * 3, PROT_READ)
            .expect("mprotect");
        state
            .linux_munmap_region(aspace_cap, 0x4000, PAGE_SIZE * 3)
            .expect("munmap");
    }

    #[test]
    fn linux_brk_grows_and_shrinks_heap_range() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let grown = state
            .linux_brk(
                0,
                aspace_cap,
                LINUX_BRK_DEFAULT_BASE + PAGE_SIZE * 2,
                PROT_READ | PROT_WRITE,
            )
            .expect("brk grow");
        assert_eq!(grown, LINUX_BRK_DEFAULT_BASE + PAGE_SIZE * 2);

        let shrunk = state
            .linux_brk(
                0,
                aspace_cap,
                LINUX_BRK_DEFAULT_BASE + PAGE_SIZE,
                PROT_READ | PROT_WRITE,
            )
            .expect("brk shrink");
        assert_eq!(shrunk, LINUX_BRK_DEFAULT_BASE + PAGE_SIZE);
    }

    #[test]
    fn linux_dispatch_table_drives_mmap_and_munmap() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let mut mmap_frame = TrapFrame::new(
            LINUX_NR_MMAP,
            [
                aspace_cap.0 as usize,
                0x8000,
                PAGE_SIZE * 2,
                PROT_READ | PROT_WRITE,
                0,
                0,
            ],
        );
        dispatch(&mut state, &mut mmap_frame);
        assert_eq!(mmap_frame.error, 0);
        assert_eq!(mmap_frame.ret0, 0x8000);

        let mut munmap_frame = TrapFrame::new(
            LINUX_NR_MUNMAP,
            [aspace_cap.0 as usize, 0x8000, PAGE_SIZE * 2, 0, 0, 0],
        );
        dispatch(&mut state, &mut munmap_frame);
        assert_eq!(munmap_frame.error, 0);
    }

    #[test]
    fn linux_dispatch_getpid_and_exit_route_to_process_manager_ipc() {
        let mut state = Bootstrap::init().expect("init");
        let (_req_ep, req_send, req_recv) = state.create_endpoint(4).expect("req ep");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(4).expect("rep ep");
        state
            .register_linux_process_manager(req_send, rep_recv)
            .expect("register proc mgr");

        let pid: u64 = 1234;
        let ppid: u64 = 77;
        state
            .ipc_send(
                rep_send,
                Message::with_header(0, PROC_OP_GETPID, 0, None, &pid.to_le_bytes())
                    .expect("reply"),
            )
            .expect("seed reply");
        state
            .ipc_send(
                rep_send,
                Message::with_header(0, PROC_OP_GETPPID, 0, None, &ppid.to_le_bytes())
                    .expect("reply"),
            )
            .expect("seed ppid reply");

        let mut getpid_frame = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut getpid_frame);
        assert_eq!(getpid_frame.error, 0);
        assert_eq!(getpid_frame.ret0, pid as usize);

        let req_msg = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("req msg");
        assert_eq!(req_msg.opcode, PROC_OP_GETPID);

        let mut getppid_frame = TrapFrame::new(LINUX_NR_GETPPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut getppid_frame);
        assert_eq!(getppid_frame.error, 0);
        assert_eq!(getppid_frame.ret0, ppid as usize);

        let getppid_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("getppid req");
        assert_eq!(getppid_req.opcode, PROC_OP_GETPPID);

        let mut exit_frame = TrapFrame::new(LINUX_NR_EXIT, [7, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &mut exit_frame);
        assert_eq!(exit_frame.error, 0);

        let exit_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("exit req");
        assert_eq!(exit_req.opcode, PROC_OP_EXIT);
        assert_eq!(exit_req.as_slice()[0], 7);
    }
}
