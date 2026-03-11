use super::bootstrap::{KernelError, KernelState};
use super::capabilities::CapId;
use super::ipc::Message;
use super::trapframe::TrapFrame;
use super::vm::{PAGE_SIZE, PageFlags, VirtAddr};

pub const LINUX_COMPAT_ABI_VERSION: u16 = 1;
pub const LINUX_COMPAT_SYSCALL_COUNT: usize = 20;
pub const LINUX_PROC_SERVER_ABI_VERSION: u16 = 1;
pub const LINUX_VFS_SERVER_ABI_VERSION: u16 = 1;

pub const LINUX_NR_BRK: usize = 214;
pub const LINUX_NR_MUNMAP: usize = 215;
pub const LINUX_NR_MMAP: usize = 222;
pub const LINUX_NR_MPROTECT: usize = 226;
pub const LINUX_NR_GETPID: usize = 172;
pub const LINUX_NR_EXIT: usize = 93;
pub const LINUX_NR_GETPPID: usize = 173;
pub const LINUX_NR_OPENAT: usize = 56;
pub const LINUX_NR_CLOSE: usize = 57;
pub const LINUX_NR_READ: usize = 63;
pub const LINUX_NR_WRITE: usize = 64;
pub const LINUX_NR_IOCTL: usize = 29;
pub const LINUX_NR_DUP: usize = 23;
pub const LINUX_NR_FCNTL: usize = 25;
pub const LINUX_NR_POLL: usize = 73;
pub const LINUX_NR_EPOLL_CREATE1: usize = 20;
pub const LINUX_NR_EPOLL_CTL: usize = 21;
pub const LINUX_NR_EPOLL_PWAIT: usize = 22;
pub const LINUX_NR_SENDFILE: usize = 71;
pub const LINUX_NR_STATX: usize = 291;

pub const PROC_OP_GETPID: u16 = 1;
pub const PROC_OP_EXIT: u16 = 2;
pub const PROC_OP_GETPPID: u16 = 3;
pub const PROC_OP_SPAWN_V2: u16 = 4;
pub const PROC_OP_WAITPID_V2: u16 = 5;

pub const VFS_OP_OPENAT: u16 = 10;
pub const VFS_OP_CLOSE: u16 = 11;
pub const VFS_OP_READ: u16 = 12;
pub const VFS_OP_WRITE: u16 = 13;
pub const VFS_OP_IOCTL: u16 = 14;
pub const VFS_OP_DUP: u16 = 15;
pub const VFS_OP_FCNTL: u16 = 16;
pub const VFS_OP_POLL: u16 = 17;
pub const VFS_OP_EPOLL_CREATE1: u16 = 18;
pub const VFS_OP_EPOLL_CTL: u16 = 19;
pub const VFS_OP_EPOLL_PWAIT: u16 = 20;
pub const VFS_OP_SENDFILE: u16 = 21;
pub const VFS_OP_STATX: u16 = 22;

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

/// Userspace-owned bindings for Linux personality servers.
///
/// Kept out of `KernelState` so the kernel remains service-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinuxServiceBindings {
    proc_mgr_request_send: Option<CapId>,
    proc_mgr_reply_recv: Option<CapId>,
    vfs_request_send: Option<CapId>,
    vfs_reply_recv: Option<CapId>,
}

impl LinuxServiceBindings {
    pub fn register_process_manager(
        &mut self,
        kernel: &KernelState,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        if !kernel
            .cspace
            .has_right(request_send_cap, super::capabilities::CapRights::Send)
        {
            return Err(KernelError::MissingRight);
        }
        if !kernel
            .cspace
            .has_right(reply_recv_cap, super::capabilities::CapRights::Receive)
        {
            return Err(KernelError::MissingRight);
        }
        self.proc_mgr_request_send = Some(request_send_cap);
        self.proc_mgr_reply_recv = Some(reply_recv_cap);
        Ok(())
    }

    pub fn register_vfs_manager(
        &mut self,
        kernel: &KernelState,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        if !kernel
            .cspace
            .has_right(request_send_cap, super::capabilities::CapRights::Send)
        {
            return Err(KernelError::MissingRight);
        }
        if !kernel
            .cspace
            .has_right(reply_recv_cap, super::capabilities::CapRights::Receive)
        {
            return Err(KernelError::MissingRight);
        }
        self.vfs_request_send = Some(request_send_cap);
        self.vfs_reply_recv = Some(reply_recv_cap);
        Ok(())
    }

    fn send_proc_request(
        &self,
        kernel: &mut KernelState,
        opcode: u16,
        arg0: u64,
    ) -> Result<(), KernelError> {
        let send_cap = self
            .proc_mgr_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let msg = Message::with_header(0, opcode, 0, None, &arg0.to_le_bytes())
            .map_err(|_| KernelError::WrongObject)?;
        kernel.ipc_send(send_cap, msg)
    }

    #[allow(dead_code)]
    fn send_proc_request2(
        &self,
        kernel: &mut KernelState,
        opcode: u16,
        arg0: u64,
        arg1: u64,
    ) -> Result<(), KernelError> {
        let send_cap = self
            .proc_mgr_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&arg0.to_le_bytes());
        payload[8..16].copy_from_slice(&arg1.to_le_bytes());
        let msg = Message::with_header(0, opcode, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        kernel.ipc_send(send_cap, msg)
    }

    fn recv_proc_reply(&self, kernel: &mut KernelState) -> Result<Option<Message>, KernelError> {
        let recv_cap = self
            .proc_mgr_reply_recv
            .ok_or(KernelError::InvalidCapability)?;
        kernel.ipc_recv(recv_cap)
    }

    fn send_vfs_request(
        &self,
        kernel: &mut KernelState,
        opcode: u16,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        let send_cap = self
            .vfs_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let msg = Message::with_header(0, opcode, 0, None, payload)
            .map_err(|_| KernelError::WrongObject)?;
        kernel.ipc_send(send_cap, msg)
    }

    fn recv_vfs_reply(&self, kernel: &mut KernelState) -> Result<Option<Message>, KernelError> {
        let recv_cap = self.vfs_reply_recv.ok_or(KernelError::InvalidCapability)?;
        kernel.ipc_recv(recv_cap)
    }
}

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
    Openat = LINUX_NR_OPENAT,
    Close = LINUX_NR_CLOSE,
    Read = LINUX_NR_READ,
    Write = LINUX_NR_WRITE,
    Ioctl = LINUX_NR_IOCTL,
    Dup = LINUX_NR_DUP,
    Fcntl = LINUX_NR_FCNTL,
    Poll = LINUX_NR_POLL,
    EpollCreate1 = LINUX_NR_EPOLL_CREATE1,
    EpollCtl = LINUX_NR_EPOLL_CTL,
    EpollPwait = LINUX_NR_EPOLL_PWAIT,
    Sendfile = LINUX_NR_SENDFILE,
    Statx = LINUX_NR_STATX,
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
        LINUX_NR_OPENAT,
        LINUX_NR_CLOSE,
        LINUX_NR_READ,
        LINUX_NR_WRITE,
        LINUX_NR_IOCTL,
        LINUX_NR_DUP,
        LINUX_NR_FCNTL,
        LINUX_NR_POLL,
        LINUX_NR_EPOLL_CREATE1,
        LINUX_NR_EPOLL_CTL,
        LINUX_NR_EPOLL_PWAIT,
        LINUX_NR_SENDFILE,
        LINUX_NR_STATX,
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
            LINUX_NR_OPENAT => Ok(Self::Openat),
            LINUX_NR_CLOSE => Ok(Self::Close),
            LINUX_NR_READ => Ok(Self::Read),
            LINUX_NR_WRITE => Ok(Self::Write),
            LINUX_NR_IOCTL => Ok(Self::Ioctl),
            LINUX_NR_DUP => Ok(Self::Dup),
            LINUX_NR_FCNTL => Ok(Self::Fcntl),
            LINUX_NR_POLL => Ok(Self::Poll),
            LINUX_NR_EPOLL_CREATE1 => Ok(Self::EpollCreate1),
            LINUX_NR_EPOLL_CTL => Ok(Self::EpollCtl),
            LINUX_NR_EPOLL_PWAIT => Ok(Self::EpollPwait),
            LINUX_NR_SENDFILE => Ok(Self::Sendfile),
            LINUX_NR_STATX => Ok(Self::Statx),
            LINUX_NR_BRK => Ok(Self::Brk),
            LINUX_NR_MUNMAP => Ok(Self::Munmap),
            LINUX_NR_MMAP => Ok(Self::Mmap),
            LINUX_NR_MPROTECT => Ok(Self::Mprotect),
            _ => Err(LinuxErrno::NoSys),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcV2Args {
    pub arg0: u64,
    pub arg1: u64,
}

impl ProcV2Args {
    pub const fn new(arg0: u64, arg1: u64) -> Self {
        Self { arg0, arg1 }
    }

    pub const fn encode(self) -> [u8; 16] {
        let mut payload = [0u8; 16];
        let a0 = self.arg0.to_le_bytes();
        let a1 = self.arg1.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            payload[i] = a0[i];
            payload[8 + i] = a1[i];
            i += 1;
        }
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, LinuxErrno> {
        if payload.len() < 16 {
            return Err(LinuxErrno::Inval);
        }
        let mut a0 = [0u8; 8];
        let mut a1 = [0u8; 8];
        a0.copy_from_slice(&payload[..8]);
        a1.copy_from_slice(&payload[8..16]);
        Ok(Self {
            arg0: u64::from_le_bytes(a0),
            arg1: u64::from_le_bytes(a1),
        })
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

fn decode_u64_reply(reply: &[u8]) -> Result<usize, LinuxErrno> {
    if reply.len() < 8 {
        return Err(LinuxErrno::Inval);
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&reply[..8]);
    Ok(u64::from_le_bytes(bytes) as usize)
}

fn pack_vfs4(a0: usize, a1: usize, a2: usize, a3: usize) -> [u8; 32] {
    let mut payload = [0u8; 32];
    payload[0..8].copy_from_slice(&(a0 as u64).to_le_bytes());
    payload[8..16].copy_from_slice(&(a1 as u64).to_le_bytes());
    payload[16..24].copy_from_slice(&(a2 as u64).to_le_bytes());
    payload[24..32].copy_from_slice(&(a3 as u64).to_le_bytes());
    payload
}

fn pack_epoll_ctl(epfd: usize, op: usize, fd: usize, event_ptr: usize) -> [u8; 32] {
    pack_vfs4(epfd, op, fd, event_ptr)
}

fn pack_sendfile(out_fd: usize, in_fd: usize, offset_ptr: usize, count: usize) -> [u8; 32] {
    pack_vfs4(out_fd, in_fd, offset_ptr, count)
}

fn pack_statx(dirfd: usize, path_ptr: usize, flags: usize, mask: usize) -> [u8; 32] {
    pack_vfs4(dirfd, path_ptr, flags, mask)
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
            self.map_user_page_with_caps(aspace_map_cap, mem_cap, VirtAddr(va as u64), flags)
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
            self.unmap_user_page(aspace_map_cap, VirtAddr(va as u64))
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
            self.protect_user_page(aspace_map_cap, VirtAddr(va as u64), flags)
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

pub fn dispatch(kernel: &mut KernelState, bindings: &LinuxServiceBindings, frame: &mut TrapFrame) {
    let result: Result<usize, LinuxErrno> = (|| match LinuxCompatSyscall::decode(frame.syscall_num)?
    {
        LinuxCompatSyscall::Exit => {
            let code = frame.args[LINUX_ARG0] as u64;
            bindings
                .send_proc_request(kernel, PROC_OP_EXIT, code)
                .map_err(LinuxErrno::from)?;
            Ok(0)
        }
        LinuxCompatSyscall::Getpid => {
            let tid = kernel.scheduler.current_tid().ok_or(LinuxErrno::NoSys)?;
            bindings
                .send_proc_request(kernel, PROC_OP_GETPID, tid)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_proc_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Getppid => {
            let tid = kernel.scheduler.current_tid().ok_or(LinuxErrno::NoSys)?;
            bindings
                .send_proc_request(kernel, PROC_OP_GETPPID, tid)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_proc_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Openat => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_OPENAT, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Close => {
            let payload = pack_vfs4(frame.args[LINUX_ARG0], 0, 0, 0);
            bindings
                .send_vfs_request(kernel, VFS_OP_CLOSE, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Read => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                0,
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_READ, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Write => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                0,
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_WRITE, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Ioctl => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_IOCTL, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
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
        LinuxCompatSyscall::Dup => {
            let payload = pack_vfs4(frame.args[LINUX_ARG0], 0, 0, 0);
            bindings
                .send_vfs_request(kernel, VFS_OP_DUP, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Fcntl => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                0,
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_FCNTL, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Poll => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                0,
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_POLL, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::EpollCreate1 => {
            let payload = pack_vfs4(frame.args[LINUX_ARG0], 0, 0, 0);
            bindings
                .send_vfs_request(kernel, VFS_OP_EPOLL_CREATE1, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::EpollCtl => {
            let payload = pack_epoll_ctl(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_EPOLL_CTL, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::EpollPwait => {
            let payload = pack_vfs4(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_EPOLL_PWAIT, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Sendfile => {
            let payload = pack_sendfile(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_SENDFILE, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
        }
        LinuxCompatSyscall::Statx => {
            let payload = pack_statx(
                frame.args[LINUX_ARG0],
                frame.args[LINUX_ARG1],
                frame.args[LINUX_ARG2],
                frame.args[LINUX_ARG3],
            );
            bindings
                .send_vfs_request(kernel, VFS_OP_STATX, &payload)
                .map_err(LinuxErrno::from)?;
            let reply = bindings
                .recv_vfs_reply(kernel)
                .map_err(LinuxErrno::from)?
                .ok_or(LinuxErrno::NoSys)?;
            decode_u64_reply(reply.as_slice())
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
    fn proc_v2_args_codec_roundtrip() {
        let args = ProcV2Args::new(10, 20);
        let encoded = args.encode();
        let decoded = ProcV2Args::decode(&encoded).expect("decode");
        assert_eq!(decoded, args);
    }

    #[test]
    fn linux_compat_errno_mapping_stable() {
        assert_eq!(LinuxErrno::Inval.code(), EINVAL);
        assert_eq!(LinuxErrno::Perm.code(), EPERM);
        assert_eq!(LinuxErrno::NoMem.code(), ENOMEM);
        assert_eq!(LinuxErrno::NoSys.code(), ENOSYS);
    }

    #[test]
    fn linux_vfs_payload_helpers_are_stable() {
        let epoll = pack_epoll_ctl(4, 2, 9, 0xABC0);
        let sendfile = pack_sendfile(3, 8, 0x1000, 4096);
        let statx = pack_statx(5, 0x2000, 0x4, 0x7FF);

        let decode = |payload: [u8; 32]| {
            let mut out = [0usize; 4];
            for (i, slot) in out.iter_mut().enumerate() {
                let mut bytes = [0u8; 8];
                let start = i * 8;
                bytes.copy_from_slice(&payload[start..start + 8]);
                *slot = u64::from_le_bytes(bytes) as usize;
            }
            out
        };

        assert_eq!(decode(epoll), [4, 2, 9, 0xABC0]);
        assert_eq!(decode(sendfile), [3, 8, 0x1000, 4096]);
        assert_eq!(decode(statx), [5, 0x2000, 0x4, 0x7FF]);
    }

    #[test]
    fn linux_compat_abi_contract_is_frozen() {
        assert_eq!(LINUX_COMPAT_ABI_VERSION, 1);
        assert_eq!(LINUX_COMPAT_SYSCALL_COUNT, 20);
        assert_eq!(LINUX_PROC_SERVER_ABI_VERSION, 1);
        assert_eq!(LINUX_VFS_SERVER_ABI_VERSION, 1);
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
            LinuxCompatSyscall::decode(LINUX_NR_OPENAT),
            Ok(LinuxCompatSyscall::Openat)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_CLOSE),
            Ok(LinuxCompatSyscall::Close)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_READ),
            Ok(LinuxCompatSyscall::Read)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_WRITE),
            Ok(LinuxCompatSyscall::Write)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_IOCTL),
            Ok(LinuxCompatSyscall::Ioctl)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_DUP),
            Ok(LinuxCompatSyscall::Dup)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_FCNTL),
            Ok(LinuxCompatSyscall::Fcntl)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_POLL),
            Ok(LinuxCompatSyscall::Poll)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_EPOLL_CREATE1),
            Ok(LinuxCompatSyscall::EpollCreate1)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_EPOLL_CTL),
            Ok(LinuxCompatSyscall::EpollCtl)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_EPOLL_PWAIT),
            Ok(LinuxCompatSyscall::EpollPwait)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_SENDFILE),
            Ok(LinuxCompatSyscall::Sendfile)
        );
        assert_eq!(
            LinuxCompatSyscall::decode(LINUX_NR_STATX),
            Ok(LinuxCompatSyscall::Statx)
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
        let bindings = LinuxServiceBindings::default();
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
        dispatch(&mut state, &bindings, &mut mmap_frame);
        assert_eq!(mmap_frame.error, 0);
        assert_eq!(mmap_frame.ret0, 0x8000);

        let mut munmap_frame = TrapFrame::new(
            LINUX_NR_MUNMAP,
            [aspace_cap.0 as usize, 0x8000, PAGE_SIZE * 2, 0, 0, 0],
        );
        dispatch(&mut state, &bindings, &mut munmap_frame);
        assert_eq!(munmap_frame.error, 0);
    }

    #[test]
    fn linux_dispatch_getpid_and_exit_route_to_process_manager_ipc() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = LinuxServiceBindings::default();
        let (_req_ep, req_send, req_recv) = state.create_endpoint(4).expect("req ep");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(4).expect("rep ep");
        bindings
            .register_process_manager(&state, req_send, rep_recv)
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
        dispatch(&mut state, &bindings, &mut getpid_frame);
        assert_eq!(getpid_frame.error, 0);
        assert_eq!(getpid_frame.ret0, pid as usize);

        let req_msg = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("req msg");
        assert_eq!(req_msg.opcode, PROC_OP_GETPID);

        let mut getppid_frame = TrapFrame::new(LINUX_NR_GETPPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut getppid_frame);
        assert_eq!(getppid_frame.error, 0);
        assert_eq!(getppid_frame.ret0, ppid as usize);

        let getppid_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("getppid req");
        assert_eq!(getppid_req.opcode, PROC_OP_GETPPID);

        let mut exit_frame = TrapFrame::new(LINUX_NR_EXIT, [7, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut exit_frame);
        assert_eq!(exit_frame.error, 0);

        let exit_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("exit req");
        assert_eq!(exit_req.opcode, PROC_OP_EXIT);
        assert_eq!(exit_req.as_slice()[0], 7);
    }

    #[test]
    fn linux_dispatch_vfs_syscalls_route_to_vfs_ipc() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = LinuxServiceBindings::default();
        let (_req_ep, req_send, req_recv) = state.create_endpoint(16).expect("vfs req ep");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(16).expect("vfs rep ep");
        bindings
            .register_vfs_manager(&state, req_send, rep_recv)
            .expect("register vfs");

        for value in [
            42u64, 0u64, 128u64, 64u64, 0u64, 43u64, 0u64, 1u64, 7u64, 0u64, 1u64, 99u64, 0u64,
        ] {
            state
                .ipc_send(
                    rep_send,
                    Message::with_header(0, 0, 0, None, &value.to_le_bytes()).expect("reply"),
                )
                .expect("seed reply");
        }

        let mut openat = TrapFrame::new(LINUX_NR_OPENAT, [3, 0x2000, 0x10, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut openat);
        assert_eq!(openat.error, 0);
        assert_eq!(openat.ret0, 42);
        let open_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(open_req.opcode, VFS_OP_OPENAT);

        let mut close = TrapFrame::new(LINUX_NR_CLOSE, [42, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut close);
        assert_eq!(close.error, 0);
        let close_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(close_req.opcode, VFS_OP_CLOSE);

        let mut read = TrapFrame::new(LINUX_NR_READ, [42, 0x3000, 128, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut read);
        assert_eq!(read.error, 0);
        assert_eq!(read.ret0, 128);
        let read_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(read_req.opcode, VFS_OP_READ);

        let mut write = TrapFrame::new(LINUX_NR_WRITE, [42, 0x4000, 64, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut write);
        assert_eq!(write.error, 0);
        assert_eq!(write.ret0, 64);
        let write_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(write_req.opcode, VFS_OP_WRITE);

        let mut ioctl = TrapFrame::new(LINUX_NR_IOCTL, [42, 0x1234, 0x5555, 0x6666, 0, 0]);
        dispatch(&mut state, &bindings, &mut ioctl);
        assert_eq!(ioctl.error, 0);
        let ioctl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(ioctl_req.opcode, VFS_OP_IOCTL);

        let mut dup = TrapFrame::new(LINUX_NR_DUP, [42, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut dup);
        assert_eq!(dup.error, 0);
        assert_eq!(dup.ret0, 43);
        let dup_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(dup_req.opcode, VFS_OP_DUP);

        let mut fcntl = TrapFrame::new(LINUX_NR_FCNTL, [42, 3, 0xF0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut fcntl);
        assert_eq!(fcntl.error, 0);
        let fcntl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(fcntl_req.opcode, VFS_OP_FCNTL);

        let mut poll = TrapFrame::new(LINUX_NR_POLL, [0x9000, 2, 10, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut poll);
        assert_eq!(poll.error, 0);
        assert_eq!(poll.ret0, 1);
        let poll_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(poll_req.opcode, VFS_OP_POLL);
        let mut epoll_create = TrapFrame::new(LINUX_NR_EPOLL_CREATE1, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_create);
        assert_eq!(epoll_create.error, 0);
        assert_eq!(epoll_create.ret0, 7);
        let epc_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epc_req.opcode, VFS_OP_EPOLL_CREATE1);

        let mut epoll_ctl = TrapFrame::new(LINUX_NR_EPOLL_CTL, [7, 1, 42, 0xA000, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_ctl);
        assert_eq!(epoll_ctl.error, 0);
        let epctl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epctl_req.opcode, VFS_OP_EPOLL_CTL);
        assert_eq!(epctl_req.as_slice().len(), 32);
        assert_eq!(&epctl_req.as_slice()[..8], &(7u64).to_le_bytes());

        let mut epoll_wait = TrapFrame::new(LINUX_NR_EPOLL_PWAIT, [7, 0xB000, 4, 10, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_wait);
        assert_eq!(epoll_wait.error, 0);
        assert_eq!(epoll_wait.ret0, 1);
        let epwait_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epwait_req.opcode, VFS_OP_EPOLL_PWAIT);

        let mut sendfile = TrapFrame::new(LINUX_NR_SENDFILE, [1, 2, 0xC000, 99, 0, 0]);
        dispatch(&mut state, &bindings, &mut sendfile);
        assert_eq!(sendfile.error, 0);
        assert_eq!(sendfile.ret0, 99);
        let sendfile_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(sendfile_req.opcode, VFS_OP_SENDFILE);
        assert_eq!(sendfile_req.as_slice().len(), 32);
        assert_eq!(&sendfile_req.as_slice()[24..32], &(99u64).to_le_bytes());

        let mut statx = TrapFrame::new(LINUX_NR_STATX, [3, 0xD000, 0, 0xE000, 0, 0]);
        dispatch(&mut state, &bindings, &mut statx);
        assert_eq!(statx.error, 0);
        let statx_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(statx_req.opcode, VFS_OP_STATX);
        assert_eq!(statx_req.as_slice().len(), 32);
        assert_eq!(&statx_req.as_slice()[8..16], &(0xD000u64).to_le_bytes());
    }

    #[test]
    fn process_manager_v2_dual_arg_payload_roundtrip() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = LinuxServiceBindings::default();
        let (_req_ep, req_send, req_recv) = state.create_endpoint(4).expect("req ep");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(4).expect("rep ep");
        bindings
            .register_process_manager(&state, req_send, rep_recv)
            .expect("register proc mgr");

        bindings
            .send_proc_request2(&mut state, PROC_OP_WAITPID_V2, 42, 0x10)
            .expect("send");
        let req = state.ipc_recv(req_recv).expect("recv").expect("msg");
        assert_eq!(req.opcode, PROC_OP_WAITPID_V2);
        assert_eq!(req.as_slice().len(), 16);
        let args = ProcV2Args::decode(req.as_slice()).expect("decode");
        assert_eq!(args, ProcV2Args::new(42, 0x10));

        state
            .ipc_send(
                rep_send,
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &7u64.to_le_bytes())
                    .expect("reply"),
            )
            .expect("seed");
        let reply = bindings
            .recv_proc_reply(&mut state)
            .expect("recv")
            .expect("msg");
        assert_eq!(reply.opcode, PROC_OP_WAITPID_V2);
    }

    #[test]
    fn linux_personality_shim_end_to_end_open_getpid_and_exit() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = LinuxServiceBindings::default();
        let (_proc_req_ep, proc_req_send, proc_req_recv) =
            state.create_endpoint(8).expect("proc req");
        let (_proc_rep_ep, proc_rep_send, proc_rep_recv) =
            state.create_endpoint(8).expect("proc rep");
        bindings
            .register_process_manager(&state, proc_req_send, proc_rep_recv)
            .expect("register proc");

        let (_vfs_req_ep, vfs_req_send, vfs_req_recv) = state.create_endpoint(8).expect("vfs req");
        let (_vfs_rep_ep, vfs_rep_send, vfs_rep_recv) = state.create_endpoint(8).expect("vfs rep");
        bindings
            .register_vfs_manager(&state, vfs_req_send, vfs_rep_recv)
            .expect("register vfs");

        state
            .ipc_send(
                proc_rep_send,
                Message::with_header(0, PROC_OP_GETPID, 0, None, &42u64.to_le_bytes())
                    .expect("rep"),
            )
            .expect("seed pid");
        state
            .ipc_send(
                vfs_rep_send,
                Message::with_header(0, VFS_OP_OPENAT, 0, None, &3u64.to_le_bytes()).expect("rep"),
            )
            .expect("seed open");

        let mut getpid = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut getpid);
        assert_eq!(getpid.error, 0);
        assert_eq!(getpid.ret0, 42);

        let mut openat = TrapFrame::new(LINUX_NR_OPENAT, [0, 0x2000, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut openat);
        assert_eq!(openat.error, 0);
        assert_eq!(openat.ret0, 3);

        let mut exit = TrapFrame::new(LINUX_NR_EXIT, [5, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut exit);
        assert_eq!(exit.error, 0);

        let proc_getpid = state.ipc_recv(proc_req_recv).expect("recv").expect("msg");
        assert_eq!(proc_getpid.opcode, PROC_OP_GETPID);
        let proc_exit = state.ipc_recv(proc_req_recv).expect("recv").expect("msg");
        assert_eq!(proc_exit.opcode, PROC_OP_EXIT);

        let vfs_open = state.ipc_recv(vfs_req_recv).expect("recv").expect("msg");
        assert_eq!(vfs_open.opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn linux_personality_deterministic_sequence_is_stable() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = LinuxServiceBindings::default();
        let (_proc_req_ep, proc_req_send, proc_req_recv) =
            state.create_endpoint(16).expect("proc req");
        let (_proc_rep_ep, proc_rep_send, proc_rep_recv) =
            state.create_endpoint(16).expect("proc rep");
        bindings
            .register_process_manager(&state, proc_req_send, proc_rep_recv)
            .expect("register proc");

        let (_vfs_req_ep, vfs_req_send, vfs_req_recv) = state.create_endpoint(16).expect("vfs req");
        let (_vfs_rep_ep, vfs_rep_send, vfs_rep_recv) = state.create_endpoint(16).expect("vfs rep");
        bindings
            .register_vfs_manager(&state, vfs_req_send, vfs_rep_recv)
            .expect("register vfs");

        for pid in [101u64, 102, 103] {
            state
                .ipc_send(
                    proc_rep_send,
                    Message::with_header(0, PROC_OP_GETPID, 0, None, &pid.to_le_bytes())
                        .expect("pid"),
                )
                .expect("seed pid");
        }
        for fd in [3u64, 4, 5] {
            state
                .ipc_send(
                    vfs_rep_send,
                    Message::with_header(0, VFS_OP_OPENAT, 0, None, &fd.to_le_bytes()).expect("fd"),
                )
                .expect("seed fd");
        }

        let sequence = [
            LINUX_NR_GETPID,
            LINUX_NR_OPENAT,
            LINUX_NR_GETPID,
            LINUX_NR_OPENAT,
            LINUX_NR_GETPID,
            LINUX_NR_OPENAT,
        ];
        let mut observed = [0usize; 6];
        for (i, nr) in sequence.iter().enumerate() {
            let mut frame = TrapFrame::new(*nr, [0, 0x2000 + i * 8, 0, 0, 0, 0]);
            dispatch(&mut state, &bindings, &mut frame);
            assert_eq!(frame.error, 0);
            observed[i] = frame.ret0;
        }

        assert_eq!(observed, [101, 3, 102, 4, 103, 5]);
        let mut proc_count = 0;
        for _ in 0..3 {
            assert!(state.ipc_recv(proc_req_recv).expect("recv").is_some());
            proc_count += 1;
        }
        let mut vfs_count = 0;
        for _ in 0..3 {
            assert!(state.ipc_recv(vfs_req_recv).expect("recv").is_some());
            vfs_count += 1;
        }
        assert_eq!(proc_count, 3);
        assert_eq!(vfs_count, 3);
    }
}
