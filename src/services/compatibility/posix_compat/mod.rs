// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{Asid, PAGE_SIZE, PageFlags, VirtAddr};
#[cfg(test)]
use yarm_ipc_abi::process_abi::{PROC_CODEC_V2_VERSION, ProcV2Args, WaitPidV2Reply};
use yarm_ipc_abi::process_abi::{
    PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_WAITPID_V2, PROC_SERVER_ABI_VERSION,
    SpawnV2Args, WaitPidV2Args,
};
use yarm_ipc_abi::socket_abi::{
    ConnectArgs, SOCKET_OP_CONNECT, SOCKET_OP_SENDTO, SOCKET_OP_SOCKET, SendToArgs, SocketArgs,
};
#[cfg(test)]
use yarm_ipc_abi::vfs_abi::{OpenAtArgs, ReadWriteArgs, VFS_CODEC_V1_VERSION};
use yarm_ipc_abi::vfs_abi::{
    StatxArgs, VFS_OP_CLOSE, VFS_OP_DUP, VFS_OP_EPOLL_CREATE1, VFS_OP_EPOLL_CTL,
    VFS_OP_EPOLL_PWAIT, VFS_OP_FCNTL, VFS_OP_IOCTL, VFS_OP_OPENAT, VFS_OP_POLL, VFS_OP_READ,
    VFS_OP_SENDFILE, VFS_OP_STATX, VFS_OP_WRITE, VFS_SERVER_ABI_VERSION, VfsV1Args,
};

pub mod sim;
pub mod sysdeps;

// Linux syscall numbers in this module follow the LP64 numbering used by
// RISC-V/AArch64 style ABIs in this prototype compatibility personality.

pub const LINUX_COMPAT_ABI_VERSION: u16 = 1;
pub const LINUX_COMPAT_SYSCALL_COUNT: usize = 23;
pub const LINUX_PROC_SERVER_ABI_VERSION: u16 = PROC_SERVER_ABI_VERSION;
pub const LINUX_VFS_SERVER_ABI_VERSION: u16 = VFS_SERVER_ABI_VERSION;
pub const POSIX_COMPAT_ABI_VERSION: u16 = LINUX_COMPAT_ABI_VERSION;
pub const POSIX_COMPAT_SYSCALL_COUNT: usize = LINUX_COMPAT_SYSCALL_COUNT;
pub const POSIX_PROC_SERVER_ABI_VERSION: u16 = LINUX_PROC_SERVER_ABI_VERSION;
pub const POSIX_VFS_SERVER_ABI_VERSION: u16 = LINUX_VFS_SERVER_ABI_VERSION;

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
pub const LINUX_NR_SOCKET: usize = 198;
pub const LINUX_NR_CONNECT: usize = 203;
pub const LINUX_NR_SENDTO: usize = 206;

pub const PROT_READ: usize = 0x1;
pub const PROT_WRITE: usize = 0x2;
pub const PROT_EXEC: usize = 0x4;

pub const EINVAL: i32 = 22;
pub const EPERM: i32 = 1;
pub const EINTR: i32 = 4;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const ETIMEDOUT: i32 = 110;
pub const ENOSYS: i32 = 38;

const LINUX_BRK_DEFAULT_BASE: usize = crate::arch::vm_layout::USER_BRK_DEFAULT_BASE;
const LINUX_ARG0: usize = 0;
const LINUX_ARG1: usize = 1;
const LINUX_ARG2: usize = 2;
const LINUX_ARG3: usize = 3;
const LINUX_ARG4: usize = 4;
const LINUX_ARG5: usize = 5;

/// Userspace-owned bindings for POSIX compatibility personality servers.
///
/// Kept out of `KernelState` so the kernel remains service-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PosixServiceBindings {
    proc_mgr_request_send: Option<CapId>,
    proc_mgr_reply_recv: Option<CapId>,
    vfs_request_send: Option<CapId>,
    vfs_reply_recv: Option<CapId>,
    socket_request_send: Option<CapId>,
    socket_reply_recv: Option<CapId>,
}

impl PosixServiceBindings {
    pub fn register_process_manager(
        &mut self,
        kernel: &KernelState,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = kernel
            .current_task_cnode()
            .ok_or(KernelError::TaskMissing)?;
        if !kernel.cnode_capability_has_right(
            cnode,
            request_send_cap,
            crate::kernel::capabilities::CapRights::SEND,
        ) {
            return Err(KernelError::MissingRight);
        }
        if !kernel.cnode_capability_has_right(
            cnode,
            reply_recv_cap,
            crate::kernel::capabilities::CapRights::RECEIVE,
        ) {
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
        let cnode = kernel
            .current_task_cnode()
            .ok_or(KernelError::TaskMissing)?;
        if !kernel.cnode_capability_has_right(
            cnode,
            request_send_cap,
            crate::kernel::capabilities::CapRights::SEND,
        ) {
            return Err(KernelError::MissingRight);
        }
        if !kernel.cnode_capability_has_right(
            cnode,
            reply_recv_cap,
            crate::kernel::capabilities::CapRights::RECEIVE,
        ) {
            return Err(KernelError::MissingRight);
        }
        self.vfs_request_send = Some(request_send_cap);
        self.vfs_reply_recv = Some(reply_recv_cap);
        Ok(())
    }

    pub fn register_socket_manager(
        &mut self,
        kernel: &KernelState,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let cnode = kernel
            .current_task_cnode()
            .ok_or(KernelError::TaskMissing)?;
        if !kernel.cnode_capability_has_right(
            cnode,
            request_send_cap,
            crate::kernel::capabilities::CapRights::SEND,
        ) {
            return Err(KernelError::MissingRight);
        }
        if !kernel.cnode_capability_has_right(
            cnode,
            reply_recv_cap,
            crate::kernel::capabilities::CapRights::RECEIVE,
        ) {
            return Err(KernelError::MissingRight);
        }
        self.socket_request_send = Some(request_send_cap);
        self.socket_reply_recv = Some(reply_recv_cap);
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
        let payload = match opcode {
            PROC_OP_WAITPID_V2 => WaitPidV2Args::new(arg0, arg1).encode(),
            _ => SpawnV2Args::new(arg0, arg1).encode(),
        };
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

    fn send_socket_request(
        &self,
        kernel: &mut KernelState,
        opcode: u16,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        let send_cap = self
            .socket_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let msg = Message::with_header(0, opcode, 0, None, payload)
            .map_err(|_| KernelError::WrongObject)?;
        kernel.ipc_send(send_cap, msg)
    }

    fn recv_socket_reply(&self, kernel: &mut KernelState) -> Result<Option<Message>, KernelError> {
        let recv_cap = self
            .socket_reply_recv
            .ok_or(KernelError::InvalidCapability)?;
        kernel.ipc_recv(recv_cap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosixErrno {
    Inval,
    Perm,
    Intr,
    Again,
    NoMem,
    TimedOut,
    NoSys,
}

impl PosixErrno {
    pub const fn code(self) -> i32 {
        match self {
            Self::Inval => EINVAL,
            Self::Perm => EPERM,
            Self::Intr => EINTR,
            Self::Again => EAGAIN,
            Self::NoMem => ENOMEM,
            Self::TimedOut => ETIMEDOUT,
            Self::NoSys => ENOSYS,
        }
    }

    pub const fn neg_code(self) -> isize {
        -(self.code() as isize)
    }

    pub const fn from_raw_errno(errno: i32) -> Self {
        match errno {
            EINVAL => Self::Inval,
            EPERM => Self::Perm,
            EINTR => Self::Intr,
            EAGAIN => Self::Again,
            ENOMEM => Self::NoMem,
            ETIMEDOUT => Self::TimedOut,
            ENOSYS => Self::NoSys,
            _ => Self::Inval,
        }
    }
}

impl From<KernelError> for PosixErrno {
    fn from(value: KernelError) -> Self {
        match value {
            KernelError::MissingRight => Self::Perm,
            KernelError::WouldBlock => Self::Intr,
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
pub enum PosixCompatSyscall {
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
    Socket = LINUX_NR_SOCKET,
    Connect = LINUX_NR_CONNECT,
    Sendto = LINUX_NR_SENDTO,
    Brk = LINUX_NR_BRK,
    Munmap = LINUX_NR_MUNMAP,
    Mmap = LINUX_NR_MMAP,
    Mprotect = LINUX_NR_MPROTECT,
}

impl PosixCompatSyscall {
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
        LINUX_NR_SOCKET,
        LINUX_NR_CONNECT,
        LINUX_NR_SENDTO,
        LINUX_NR_BRK,
        LINUX_NR_MUNMAP,
        LINUX_NR_MMAP,
        LINUX_NR_MPROTECT,
    ];

    pub fn decode(raw: usize) -> Result<Self, PosixErrno> {
        if !Self::DISPATCH_TABLE.contains(&raw) {
            return Err(PosixErrno::NoSys);
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
            LINUX_NR_SOCKET => Ok(Self::Socket),
            LINUX_NR_CONNECT => Ok(Self::Connect),
            LINUX_NR_SENDTO => Ok(Self::Sendto),
            LINUX_NR_BRK => Ok(Self::Brk),
            LINUX_NR_MUNMAP => Ok(Self::Munmap),
            LINUX_NR_MMAP => Ok(Self::Mmap),
            LINUX_NR_MPROTECT => Ok(Self::Mprotect),
            _ => Err(PosixErrno::NoSys),
        }
    }
}

fn round_up_page(value: usize) -> Result<usize, PosixErrno> {
    if value.is_multiple_of(PAGE_SIZE) {
        Ok(value)
    } else {
        let rounded = value.checked_add(PAGE_SIZE - 1).ok_or(PosixErrno::Inval)?;
        Ok(rounded & !(PAGE_SIZE - 1))
    }
}

fn prot_to_page_flags(prot: usize) -> Result<PageFlags, PosixErrno> {
    Ok(PageFlags {
        read: (prot & PROT_READ) != 0,
        write: (prot & PROT_WRITE) != 0,
        execute: (prot & PROT_EXEC) != 0,
        user: true,
        cache_policy: crate::kernel::vm::CachePolicy::WriteBack,
    })
}

fn decode_u64_reply(reply: &[u8]) -> Result<usize, PosixErrno> {
    if reply.len() < 8 {
        return Err(PosixErrno::Inval);
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&reply[..8]);
    let raw = i64::from_le_bytes(bytes);
    if raw < 0 {
        let errno = i32::try_from((-raw) as u64).map_err(|_| PosixErrno::Inval)?;
        return Err(PosixErrno::from_raw_errno(errno));
    }
    // Keep conversion checked so narrower pointer-width targets do not silently truncate.
    usize::try_from(raw as u64).map_err(|_| PosixErrno::Inval)
}

fn pack_vfs4(a0: usize, a1: usize, a2: usize, a3: usize) -> [u8; 32] {
    VfsV1Args::new(a0 as u64, a1 as u64, a2 as u64, a3 as u64).encode()
}

fn pack_epoll_ctl(epfd: usize, op: usize, fd: usize, event_ptr: usize) -> [u8; 32] {
    pack_vfs4(epfd, op, fd, event_ptr)
}

fn pack_sendfile(out_fd: usize, in_fd: usize, offset_ptr: usize, count: usize) -> [u8; 32] {
    pack_vfs4(out_fd, in_fd, offset_ptr, count)
}

fn pack_statx(dirfd: usize, path_ptr: usize, flags: usize, mask: usize) -> [u8; 32] {
    StatxArgs::new(dirfd as u64, path_ptr as u64, flags as u64, mask as u64).encode()
}

impl KernelState {
    fn current_posix_asid(&self) -> Result<Asid, PosixErrno> {
        let tid = self.current_tid().ok_or(PosixErrno::NoSys)?;
        self.task_asid(tid).ok_or(PosixErrno::NoSys)
    }

    pub fn posix_mmap_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<usize, PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }

        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            let (_, mem_cap) = self
                .alloc_anonymous_memory_object()
                .map_err(PosixErrno::from)?;
            self.map_user_page_with_caps(aspace_map_cap, mem_cap, VirtAddr(va as u64), flags)
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(addr)
    }

    pub fn posix_mmap_region_current_task(
        &mut self,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<usize, PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }
        let asid = self.current_posix_asid()?;
        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            let (_, mem_cap) = self
                .alloc_anonymous_memory_object()
                .map_err(PosixErrno::from)?;
            self.map_user_page_in_asid_with_caps(asid, mem_cap, VirtAddr(va as u64), flags)
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(addr)
    }

    pub fn posix_munmap_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
    ) -> Result<(), PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.unmap_user_page(aspace_map_cap, VirtAddr(va as u64))
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn posix_munmap_region_current_task(
        &mut self,
        addr: usize,
        len: usize,
    ) -> Result<(), PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }
        let asid = self.current_posix_asid()?;
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.unmap_user_page_in_asid(asid, VirtAddr(va as u64))
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn posix_mprotect_region(
        &mut self,
        aspace_map_cap: CapId,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<(), PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }
        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.protect_user_page(aspace_map_cap, VirtAddr(va as u64), flags)
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn posix_mprotect_region_current_task(
        &mut self,
        addr: usize,
        len: usize,
        prot: usize,
    ) -> Result<(), PosixErrno> {
        if len == 0 || !addr.is_multiple_of(PAGE_SIZE) {
            return Err(PosixErrno::Inval);
        }
        let asid = self.current_posix_asid()?;
        let flags = prot_to_page_flags(prot)?;
        let end = addr
            .checked_add(round_up_page(len)?)
            .ok_or(PosixErrno::Inval)?;
        let mut va = addr;
        while va < end {
            self.protect_user_page_in_asid(asid, VirtAddr(va as u64), flags)
                .map_err(PosixErrno::from)?;
            va += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn posix_brk(
        &mut self,
        tid: u64,
        aspace_map_cap: CapId,
        requested_end: usize,
        prot: usize,
    ) -> Result<usize, PosixErrno> {
        let (base, current_end) = self
            .task_brk_bounds(tid)
            .unwrap_or((LINUX_BRK_DEFAULT_BASE, LINUX_BRK_DEFAULT_BASE));

        if requested_end == 0 {
            return Ok(current_end);
        }
        if requested_end < base {
            return Err(PosixErrno::Inval);
        }

        let current_rounded = round_up_page(current_end)?;
        let requested_rounded = round_up_page(requested_end)?;

        if requested_rounded > current_rounded {
            let map_start = current_rounded;
            let map_len = requested_rounded - map_start;
            if map_len > 0 {
                self.posix_mmap_region(aspace_map_cap, map_start, map_len, prot)?;
            }
        } else if requested_rounded < current_rounded {
            let unmap_len = current_rounded - requested_rounded;
            self.posix_munmap_region(aspace_map_cap, requested_rounded, unmap_len)?;
        }

        self.set_task_brk_bounds(tid, base, requested_end)
            .map_err(PosixErrno::from)?;
        Ok(requested_end)
    }

    pub fn posix_brk_current_task(
        &mut self,
        tid: u64,
        requested_end: usize,
        prot: usize,
    ) -> Result<usize, PosixErrno> {
        let (base, current_end) = self
            .task_brk_bounds(tid)
            .unwrap_or((LINUX_BRK_DEFAULT_BASE, LINUX_BRK_DEFAULT_BASE));

        if requested_end == 0 {
            return Ok(current_end);
        }
        if requested_end < base {
            return Err(PosixErrno::Inval);
        }

        let current_rounded = round_up_page(current_end)?;
        let requested_rounded = round_up_page(requested_end)?;

        if requested_rounded > current_rounded {
            let map_start = current_rounded;
            let map_len = requested_rounded - map_start;
            if map_len > 0 {
                self.posix_mmap_region_current_task(map_start, map_len, prot)?;
            }
        } else if requested_rounded < current_rounded {
            let unmap_len = current_rounded - requested_rounded;
            self.posix_munmap_region_current_task(requested_rounded, unmap_len)?;
        }

        self.set_task_brk_bounds(tid, base, requested_end)
            .map_err(PosixErrno::from)?;
        Ok(requested_end)
    }
}

pub fn dispatch(kernel: &mut KernelState, bindings: &PosixServiceBindings, frame: &mut TrapFrame) {
    // Linux ABI compatibility note:
    // - mmap/munmap/mprotect consume Linux argument order directly (addr/len/prot/...).
    // - Capability-targeted VM mapping is exposed via kernel-native `sys_vm_map`.
    // - brk consumes Linux arg0 (`requested_end`) and targets current task ASID.
    let result: Result<usize, PosixErrno> =
        (|| match PosixCompatSyscall::decode(frame.syscall_num())? {
            PosixCompatSyscall::Exit => {
                let code = frame.arg(LINUX_ARG0) as u64;
                bindings
                    .send_proc_request(kernel, PROC_OP_EXIT, code)
                    .map_err(PosixErrno::from)?;
                Ok(0)
            }
            PosixCompatSyscall::Getpid => {
                let tid = kernel.current_tid().ok_or(PosixErrno::NoSys)?;
                bindings
                    .send_proc_request(kernel, PROC_OP_GETPID, tid)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_proc_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Getppid => {
                let tid = kernel.current_tid().ok_or(PosixErrno::NoSys)?;
                bindings
                    .send_proc_request(kernel, PROC_OP_GETPPID, tid)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_proc_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Openat => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_OPENAT, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Close => {
                let payload = pack_vfs4(frame.arg(LINUX_ARG0), 0, 0, 0);
                bindings
                    .send_vfs_request(kernel, VFS_OP_CLOSE, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Read => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    0,
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_READ, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Write => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    0,
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_WRITE, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Ioctl => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_IOCTL, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Mmap => {
                let addr = frame.arg(LINUX_ARG0);
                let len = frame.arg(LINUX_ARG1);
                let prot = frame.arg(LINUX_ARG2);
                kernel.posix_mmap_region_current_task(addr, len, prot)
            }
            PosixCompatSyscall::Munmap => {
                let addr = frame.arg(LINUX_ARG0);
                let len = frame.arg(LINUX_ARG1);
                kernel.posix_munmap_region_current_task(addr, len)?;
                Ok(0)
            }
            PosixCompatSyscall::Mprotect => {
                let addr = frame.arg(LINUX_ARG0);
                let len = frame.arg(LINUX_ARG1);
                let prot = frame.arg(LINUX_ARG2);
                kernel.posix_mprotect_region_current_task(addr, len, prot)?;
                Ok(0)
            }
            PosixCompatSyscall::Dup => {
                let payload = pack_vfs4(frame.arg(LINUX_ARG0), 0, 0, 0);
                bindings
                    .send_vfs_request(kernel, VFS_OP_DUP, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Fcntl => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    0,
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_FCNTL, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Poll => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    0,
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_POLL, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::EpollCreate1 => {
                let payload = pack_vfs4(frame.arg(LINUX_ARG0), 0, 0, 0);
                bindings
                    .send_vfs_request(kernel, VFS_OP_EPOLL_CREATE1, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::EpollCtl => {
                let payload = pack_epoll_ctl(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_EPOLL_CTL, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::EpollPwait => {
                let payload = pack_vfs4(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_EPOLL_PWAIT, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Sendfile => {
                let payload = pack_sendfile(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_SENDFILE, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Statx => {
                let payload = pack_statx(
                    frame.arg(LINUX_ARG0),
                    frame.arg(LINUX_ARG1),
                    frame.arg(LINUX_ARG2),
                    frame.arg(LINUX_ARG3),
                );
                bindings
                    .send_vfs_request(kernel, VFS_OP_STATX, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_vfs_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Socket => {
                let payload = SocketArgs::new(
                    frame.arg(LINUX_ARG0) as u64,
                    frame.arg(LINUX_ARG1) as u64,
                    frame.arg(LINUX_ARG2) as u64,
                )
                .encode();
                bindings
                    .send_socket_request(kernel, SOCKET_OP_SOCKET, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_socket_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Connect => {
                let payload = ConnectArgs::new(
                    frame.arg(LINUX_ARG0) as u64,
                    frame.arg(LINUX_ARG1) as u64,
                    frame.arg(LINUX_ARG2) as u64,
                )
                .encode();
                bindings
                    .send_socket_request(kernel, SOCKET_OP_CONNECT, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_socket_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Sendto => {
                let payload = SendToArgs::new(
                    frame.arg(LINUX_ARG0) as u64,
                    frame.arg(LINUX_ARG1) as u64,
                    frame.arg(LINUX_ARG2) as u64,
                    frame.arg(LINUX_ARG3) as u64,
                    frame.arg(LINUX_ARG4) as u64,
                    frame.arg(LINUX_ARG5) as u64,
                )
                .encode();
                bindings
                    .send_socket_request(kernel, SOCKET_OP_SENDTO, &payload)
                    .map_err(PosixErrno::from)?;
                let reply = bindings
                    .recv_socket_reply(kernel)
                    .map_err(PosixErrno::from)?
                    .ok_or(PosixErrno::NoSys)?;
                decode_u64_reply(reply.as_slice())
            }
            PosixCompatSyscall::Brk => {
                let requested = frame.arg(LINUX_ARG0);
                let tid = kernel.current_tid().ok_or(PosixErrno::NoSys)?;
                kernel.posix_brk_current_task(tid, requested, PROT_READ | PROT_WRITE)
            }
        })();

    match result {
        Ok(value) => frame.set_ok(value, 0, 0),
        Err(errno) => frame.set_err(errno.code() as usize),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::ipc::Message;
    use crate::std::thread;

    fn run_with_large_stack<F>(f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let handle = thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(f)
            .expect("spawn large-stack test thread");
        handle.join().expect("join large-stack test thread");
    }

    #[test]
    fn proc_v2_args_codec_roundtrip() {
        let args = SpawnV2Args::new(10, 20);
        let encoded = args.encode();
        let decoded = SpawnV2Args::decode(&encoded).expect("decode");
        assert_eq!(decoded, args);
    }

    #[test]
    fn proc_v2_codec_golden_vector_is_frozen() {
        let args = WaitPidV2Args::new(0x11, 0x22);
        let encoded = args.encode();
        assert_eq!(&encoded[..8], &0x11u64.to_le_bytes());
        assert_eq!(&encoded[8..16], &0x22u64.to_le_bytes());
    }

    #[test]
    fn vfs_v1_codec_golden_vector_is_frozen() {
        let args = OpenAtArgs::new(1, 2, 3, 4);
        let encoded = args.encode();
        assert_eq!(&encoded[..8], &1u64.to_le_bytes());
        assert_eq!(&encoded[8..16], &2u64.to_le_bytes());
        assert_eq!(&encoded[16..24], &3u64.to_le_bytes());
        assert_eq!(&encoded[24..32], &4u64.to_le_bytes());
    }

    #[test]
    fn vfs_v1_args_codec_roundtrip() {
        let args = ReadWriteArgs::new(1, 2, 3);
        let encoded = args.encode();
        let decoded = ReadWriteArgs::decode(&encoded).expect("decode");
        assert_eq!(decoded, args);
    }

    #[test]
    fn codec_fixture_vectors_are_frozen() {
        let fixtures_proc = [
            (WaitPidV2Reply::new(0, 0), [0u8; 16]),
            (WaitPidV2Reply::new(1, 2), {
                let mut b = [0u8; 16];
                b[..8].copy_from_slice(&1u64.to_le_bytes());
                b[8..16].copy_from_slice(&2u64.to_le_bytes());
                b
            }),
        ];
        for (args, expected) in fixtures_proc {
            assert_eq!(args.encode(), expected);
            assert_eq!(WaitPidV2Reply::decode(&expected).expect("decode"), args);
        }

        let fixtures_vfs = [
            OpenAtArgs::new(0, 0, 0, 0),
            OpenAtArgs::new(9, 8, 7, 6),
            OpenAtArgs::new(u64::MAX, 1, 2, 3),
        ];
        for args in fixtures_vfs {
            let encoded = args.encode();
            assert_eq!(OpenAtArgs::decode(&encoded).expect("decode"), args);
        }
    }

    #[test]
    fn codec_rejects_truncated_payloads() {
        assert!(WaitPidV2Args::decode(&[0u8; 15]).is_err());
        assert!(OpenAtArgs::decode(&[0u8; 31]).is_err());
    }

    #[test]
    fn posix_compat_errno_mapping_stable() {
        assert_eq!(PosixErrno::Inval.code(), EINVAL);
        assert_eq!(PosixErrno::Perm.code(), EPERM);
        assert_eq!(PosixErrno::NoMem.code(), ENOMEM);
        assert_eq!(PosixErrno::NoSys.code(), ENOSYS);
    }

    #[test]
    fn posix_vfs_payload_helpers_are_stable() {
        let epoll = pack_epoll_ctl(4, 2, 9, 0xABC0);
        let sendfile = pack_sendfile(3, 8, 0x1000, 4096);
        let statx = pack_statx(5, 0x2000, 0x4, 0x7FF);

        let decode = |payload: [u8; 32]| {
            let args = StatxArgs::decode(&payload).expect("decode");
            [
                args.dirfd as usize,
                args.path_ptr as usize,
                args.flags as usize,
                args.mask_or_buf as usize,
            ]
        };

        assert_eq!(decode(epoll), [4, 2, 9, 0xABC0]);
        assert_eq!(decode(sendfile), [3, 8, 0x1000, 4096]);
        assert_eq!(decode(statx), [5, 0x2000, 0x4, 0x7FF]);
    }

    #[test]
    fn posix_compat_abi_contract_is_frozen() {
        assert_eq!(LINUX_COMPAT_ABI_VERSION, 1);
        assert_eq!(LINUX_COMPAT_SYSCALL_COUNT, 23);
        assert_eq!(LINUX_PROC_SERVER_ABI_VERSION, 1);
        assert_eq!(LINUX_VFS_SERVER_ABI_VERSION, 1);
        assert_eq!(PROC_CODEC_V2_VERSION, 2);
        assert_eq!(ProcV2Args::VERSION, PROC_CODEC_V2_VERSION);
        assert_eq!(VFS_CODEC_V1_VERSION, 1);
        assert_eq!(VfsV1Args::VERSION, VFS_CODEC_V1_VERSION);
        assert_eq!(
            SocketArgs::VERSION,
            yarm_ipc_abi::socket_abi::SOCKET_CODEC_V1_VERSION
        );
        assert_eq!(SOCKET_OP_SOCKET, 1);
        assert_eq!(SOCKET_OP_SENDTO, 3);
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_EXIT),
            Ok(PosixCompatSyscall::Exit)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_GETPID),
            Ok(PosixCompatSyscall::Getpid)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_GETPPID),
            Ok(PosixCompatSyscall::Getppid)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_OPENAT),
            Ok(PosixCompatSyscall::Openat)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_CLOSE),
            Ok(PosixCompatSyscall::Close)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_READ),
            Ok(PosixCompatSyscall::Read)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_WRITE),
            Ok(PosixCompatSyscall::Write)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_IOCTL),
            Ok(PosixCompatSyscall::Ioctl)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_DUP),
            Ok(PosixCompatSyscall::Dup)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_FCNTL),
            Ok(PosixCompatSyscall::Fcntl)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_POLL),
            Ok(PosixCompatSyscall::Poll)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_EPOLL_CREATE1),
            Ok(PosixCompatSyscall::EpollCreate1)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_EPOLL_CTL),
            Ok(PosixCompatSyscall::EpollCtl)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_EPOLL_PWAIT),
            Ok(PosixCompatSyscall::EpollPwait)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_SENDFILE),
            Ok(PosixCompatSyscall::Sendfile)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_STATX),
            Ok(PosixCompatSyscall::Statx)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_SOCKET),
            Ok(PosixCompatSyscall::Socket)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_CONNECT),
            Ok(PosixCompatSyscall::Connect)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_SENDTO),
            Ok(PosixCompatSyscall::Sendto)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_BRK),
            Ok(PosixCompatSyscall::Brk)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_MUNMAP),
            Ok(PosixCompatSyscall::Munmap)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_MMAP),
            Ok(PosixCompatSyscall::Mmap)
        );
        assert_eq!(
            PosixCompatSyscall::decode(LINUX_NR_MPROTECT),
            Ok(PosixCompatSyscall::Mprotect)
        );
        assert_eq!(PosixCompatSyscall::decode(0xFFFF), Err(PosixErrno::NoSys));
    }

    #[test]
    fn prot_none_maps_to_guard_page_flags() {
        let flags = prot_to_page_flags(0).expect("prot none");
        assert!(!flags.read);
        assert!(!flags.write);
        assert!(!flags.execute);
        assert!(flags.user);
    }

    #[test]
    fn round_up_page_handles_boundaries_and_overflow() {
        assert_eq!(round_up_page(0), Ok(0));
        assert_eq!(round_up_page(PAGE_SIZE), Ok(PAGE_SIZE));
        assert_eq!(round_up_page(PAGE_SIZE + 1), Ok(PAGE_SIZE * 2));
        assert_eq!(round_up_page(usize::MAX), Err(PosixErrno::Inval));
    }

    #[test]
    fn linux_multi_page_vm_wrappers_work() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let addr = state
            .posix_mmap_region(aspace_cap, 0x4000, PAGE_SIZE * 3, PROT_READ | PROT_WRITE)
            .expect("mmap");
        assert_eq!(addr, 0x4000);

        state
            .posix_mprotect_region(aspace_cap, 0x4000, PAGE_SIZE * 3, PROT_READ)
            .expect("mprotect");
        state
            .posix_munmap_region(aspace_cap, 0x4000, PAGE_SIZE * 3)
            .expect("munmap");
    }

    #[test]
    fn posix_brk_grows_and_shrinks_heap_range() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");

        let grown = state
            .posix_brk(
                0,
                aspace_cap,
                LINUX_BRK_DEFAULT_BASE + PAGE_SIZE * 2,
                PROT_READ | PROT_WRITE,
            )
            .expect("brk grow");
        assert_eq!(grown, LINUX_BRK_DEFAULT_BASE + PAGE_SIZE * 2);

        let shrunk = state
            .posix_brk(
                0,
                aspace_cap,
                LINUX_BRK_DEFAULT_BASE + PAGE_SIZE,
                PROT_READ | PROT_WRITE,
            )
            .expect("brk shrink");
        assert_eq!(shrunk, LINUX_BRK_DEFAULT_BASE + PAGE_SIZE);

        let query = state
            .posix_brk(0, aspace_cap, 0, PROT_READ | PROT_WRITE)
            .expect("brk query");
        assert_eq!(query, shrunk);
    }

    #[test]
    fn posix_dispatch_table_drives_mmap_and_munmap() {
        let mut state = Bootstrap::init().expect("init");
        let bindings = PosixServiceBindings::default();
        let (asid, _aspace_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");

        let mut mmap_frame = TrapFrame::new(
            LINUX_NR_MMAP,
            [0x8000, PAGE_SIZE * 2, PROT_READ | PROT_WRITE, 0, 0, 0],
        );
        dispatch(&mut state, &bindings, &mut mmap_frame);
        assert_eq!(mmap_frame.error_code(), None);
        assert_eq!(mmap_frame.ret0(), 0x8000);

        let mut munmap_frame = TrapFrame::new(LINUX_NR_MUNMAP, [0x8000, PAGE_SIZE * 2, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut munmap_frame);
        assert_eq!(munmap_frame.error_code(), None);
    }

    #[test]
    fn posix_dispatch_brk_uses_linux_arg_order() {
        let mut state = Bootstrap::init().expect("init");
        let bindings = PosixServiceBindings::default();
        let (asid, _aspace_cap) = state.create_user_address_space().expect("aspace");
        state.bind_task_asid(0, asid).expect("bind");

        let requested = LINUX_BRK_DEFAULT_BASE + PAGE_SIZE;
        let mut brk_frame = TrapFrame::new(LINUX_NR_BRK, [requested, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut brk_frame);
        assert_eq!(brk_frame.error_code(), None);
        assert_eq!(brk_frame.ret0(), requested);
    }

    #[test]
    fn posix_dispatch_getpid_and_exit_route_to_process_manager_ipc() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
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
        assert_eq!(getpid_frame.error_code(), None);
        assert_eq!(getpid_frame.ret0(), pid as usize);

        let req_msg = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("req msg");
        assert_eq!(req_msg.opcode, PROC_OP_GETPID);

        let mut getppid_frame = TrapFrame::new(LINUX_NR_GETPPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut getppid_frame);
        assert_eq!(getppid_frame.error_code(), None);
        assert_eq!(getppid_frame.ret0(), ppid as usize);

        let getppid_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("getppid req");
        assert_eq!(getppid_req.opcode, PROC_OP_GETPPID);

        let mut exit_frame = TrapFrame::new(LINUX_NR_EXIT, [7, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut exit_frame);
        assert_eq!(exit_frame.error_code(), None);

        let exit_req = state
            .ipc_recv(req_recv)
            .expect("req recv")
            .expect("exit req");
        assert_eq!(exit_req.opcode, PROC_OP_EXIT);
        assert_eq!(exit_req.as_slice()[0], 7);
    }

    #[test]
    fn posix_dispatch_vfs_syscalls_route_to_vfs_ipc() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
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
        assert_eq!(openat.error_code(), None);
        assert_eq!(openat.ret0(), 42);
        let open_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(open_req.opcode, VFS_OP_OPENAT);

        let mut close = TrapFrame::new(LINUX_NR_CLOSE, [42, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut close);
        assert_eq!(close.error_code(), None);
        let close_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(close_req.opcode, VFS_OP_CLOSE);

        let mut read = TrapFrame::new(LINUX_NR_READ, [42, 0x3000, 128, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut read);
        assert_eq!(read.error_code(), None);
        assert_eq!(read.ret0(), 128);
        let read_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(read_req.opcode, VFS_OP_READ);

        let mut write = TrapFrame::new(LINUX_NR_WRITE, [42, 0x4000, 64, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut write);
        assert_eq!(write.error_code(), None);
        assert_eq!(write.ret0(), 64);
        let write_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(write_req.opcode, VFS_OP_WRITE);

        let mut ioctl = TrapFrame::new(LINUX_NR_IOCTL, [42, 0x1234, 0x5555, 0x6666, 0, 0]);
        dispatch(&mut state, &bindings, &mut ioctl);
        assert_eq!(ioctl.error_code(), None);
        let ioctl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(ioctl_req.opcode, VFS_OP_IOCTL);

        let mut dup = TrapFrame::new(LINUX_NR_DUP, [42, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut dup);
        assert_eq!(dup.error_code(), None);
        assert_eq!(dup.ret0(), 43);
        let dup_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(dup_req.opcode, VFS_OP_DUP);

        let mut fcntl = TrapFrame::new(LINUX_NR_FCNTL, [42, 3, 0xF0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut fcntl);
        assert_eq!(fcntl.error_code(), None);
        let fcntl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(fcntl_req.opcode, VFS_OP_FCNTL);

        let mut poll = TrapFrame::new(LINUX_NR_POLL, [0x9000, 2, 10, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut poll);
        assert_eq!(poll.error_code(), None);
        assert_eq!(poll.ret0(), 1);
        let poll_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(poll_req.opcode, VFS_OP_POLL);
        let mut epoll_create = TrapFrame::new(LINUX_NR_EPOLL_CREATE1, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_create);
        assert_eq!(epoll_create.error_code(), None);
        assert_eq!(epoll_create.ret0(), 7);
        let epc_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epc_req.opcode, VFS_OP_EPOLL_CREATE1);

        let mut epoll_ctl = TrapFrame::new(LINUX_NR_EPOLL_CTL, [7, 1, 42, 0xA000, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_ctl);
        assert_eq!(epoll_ctl.error_code(), None);
        let epctl_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epctl_req.opcode, VFS_OP_EPOLL_CTL);
        assert_eq!(epctl_req.as_slice().len(), 32);
        assert_eq!(&epctl_req.as_slice()[..8], &(7u64).to_le_bytes());

        let mut epoll_wait = TrapFrame::new(LINUX_NR_EPOLL_PWAIT, [7, 0xB000, 4, 10, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_wait);
        assert_eq!(epoll_wait.error_code(), None);
        assert_eq!(epoll_wait.ret0(), 1);
        let epwait_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(epwait_req.opcode, VFS_OP_EPOLL_PWAIT);

        let mut sendfile = TrapFrame::new(LINUX_NR_SENDFILE, [1, 2, 0xC000, 99, 0, 0]);
        dispatch(&mut state, &bindings, &mut sendfile);
        assert_eq!(sendfile.error_code(), None);
        assert_eq!(sendfile.ret0(), 99);
        let sendfile_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(sendfile_req.opcode, VFS_OP_SENDFILE);
        assert_eq!(sendfile_req.as_slice().len(), 32);
        assert_eq!(&sendfile_req.as_slice()[24..32], &(99u64).to_le_bytes());

        let mut statx = TrapFrame::new(LINUX_NR_STATX, [3, 0xD000, 0, 0xE000, 0, 0]);
        dispatch(&mut state, &bindings, &mut statx);
        assert_eq!(statx.error_code(), None);
        let statx_req = state.ipc_recv(req_recv).expect("req").expect("msg");
        assert_eq!(statx_req.opcode, VFS_OP_STATX);
        assert_eq!(statx_req.as_slice().len(), 32);
        assert_eq!(&statx_req.as_slice()[8..16], &(0xD000u64).to_le_bytes());
    }

    #[test]
    fn posix_dispatch_socket_syscall_routes_to_socket_ipc_binding() {
        run_with_large_stack(|| {
            let mut state = Bootstrap::init().expect("init");
            let mut bindings = PosixServiceBindings::default();
            let (_req_ep, req_send, req_recv) = state.create_endpoint(4).expect("socket req ep");
            let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(4).expect("socket rep ep");
            bindings
                .register_socket_manager(&state, req_send, rep_recv)
                .expect("register socket");

            let socket_fd: u64 = 1001;
            state
                .ipc_send(
                    rep_send,
                    Message::with_header(
                        0,
                        SOCKET_OP_SOCKET,
                        0,
                        None,
                        &socket_fd.to_le_bytes(),
                    )
                    .expect("reply"),
                )
                .expect("seed reply");

            let mut socket = TrapFrame::new(LINUX_NR_SOCKET, [2, 1, 0, 0, 0, 0]);
            dispatch(&mut state, &bindings, &mut socket);
            assert_eq!(socket.error_code(), None);
            assert_eq!(socket.ret0(), socket_fd as usize);

            let req = state.ipc_recv(req_recv).expect("req").expect("msg");
            assert_eq!(req.opcode, SOCKET_OP_SOCKET);
            let args = SocketArgs::decode(req.as_slice()).expect("decode args");
            assert_eq!(args.domain, 2);
            assert_eq!(args.sock_type, 1);
            assert_eq!(args.protocol, 0);

            state
                .ipc_send(
                    rep_send,
                    Message::with_header(0, SOCKET_OP_CONNECT, 0, None, &0u64.to_le_bytes())
                        .expect("connect reply"),
                )
                .expect("seed connect reply");

            let mut connect = TrapFrame::new(LINUX_NR_CONNECT, [1001, 0xCAFE, 16, 0, 0, 0]);
            dispatch(&mut state, &bindings, &mut connect);
            assert_eq!(connect.error_code(), None);
            assert_eq!(connect.ret0(), 0);

            let connect_req = state.ipc_recv(req_recv).expect("req").expect("msg");
            assert_eq!(connect_req.opcode, SOCKET_OP_CONNECT);
            let connect_args = ConnectArgs::decode(connect_req.as_slice()).expect("decode args");
            assert_eq!(connect_args.fd, 1001);
            assert_eq!(connect_args.addr_ptr, 0xCAFE);
            assert_eq!(connect_args.addr_len, 16);

            state
                .ipc_send(
                    rep_send,
                    Message::with_header(0, SOCKET_OP_SENDTO, 0, None, &12u64.to_le_bytes())
                        .expect("sendto reply"),
                )
                .expect("seed sendto reply");

            let mut sendto =
                TrapFrame::new(LINUX_NR_SENDTO, [1001, 0xBEEF, 12, 0, 0xD00D, 16]);
            dispatch(&mut state, &bindings, &mut sendto);
            assert_eq!(sendto.error_code(), None);
            assert_eq!(sendto.ret0(), 12);

            let sendto_req = state.ipc_recv(req_recv).expect("req").expect("msg");
            assert_eq!(sendto_req.opcode, SOCKET_OP_SENDTO);
            let sendto_args = SendToArgs::decode(sendto_req.as_slice()).expect("decode args");
            assert_eq!(sendto_args.fd, 1001);
            assert_eq!(sendto_args.buf_ptr, 0xBEEF);
            assert_eq!(sendto_args.len, 12);
            assert_eq!(sendto_args.flags, 0);
            assert_eq!(sendto_args.dest_addr_ptr, 0xD00D);
            assert_eq!(sendto_args.addrlen, 16);
        });
    }

    #[test]
    fn posix_dispatch_maps_eintr_and_timeout_errno_at_shim_boundary() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
        let (_req_ep, req_send, _req_recv) = state.create_endpoint(8).expect("vfs req");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(8).expect("vfs rep");
        bindings
            .register_vfs_manager(&state, req_send, rep_recv)
            .expect("register vfs");

        for errno in [EINTR, ETIMEDOUT] {
            let raw = (-(errno as i64)).to_le_bytes();
            state
                .ipc_send(
                    rep_send,
                    Message::with_header(0, 0, 0, None, &raw).expect("reply"),
                )
                .expect("seed reply");
        }

        let mut poll = TrapFrame::new(LINUX_NR_POLL, [0x9000, 1, 10, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut poll);
        assert_eq!(poll.error_code(), Some(EINTR as usize));

        let mut epoll_wait = TrapFrame::new(LINUX_NR_EPOLL_PWAIT, [7, 0xB000, 4, 25, 0, 0]);
        dispatch(&mut state, &bindings, &mut epoll_wait);
        assert_eq!(epoll_wait.error_code(), Some(ETIMEDOUT as usize));
    }

    #[test]
    fn posix_dispatch_partial_io_and_invalid_handle_errno_are_explicit() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
        let (_req_ep, req_send, _req_recv) = state.create_endpoint(8).expect("vfs req");
        let (_rep_ep, rep_send, rep_recv) = state.create_endpoint(8).expect("vfs rep");
        bindings
            .register_vfs_manager(&state, req_send, rep_recv)
            .expect("register vfs");

        state
            .ipc_send(
                rep_send,
                Message::with_header(0, 0, 0, None, &5u64.to_le_bytes()).expect("partial"),
            )
            .expect("seed partial");
        state
            .ipc_send(
                rep_send,
                Message::with_header(0, 0, 0, None, &(-(EINVAL as i64)).to_le_bytes())
                    .expect("bad fd"),
            )
            .expect("seed bad fd");

        let mut partial_read = TrapFrame::new(LINUX_NR_READ, [42, 0x2000, 64, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut partial_read);
        assert_eq!(partial_read.error_code(), None);
        assert_eq!(partial_read.ret0(), 5);

        let mut bad_fd_read = TrapFrame::new(LINUX_NR_READ, [999, 0x2000, 64, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut bad_fd_read);
        assert_eq!(bad_fd_read.error_code(), Some(EINVAL as usize));
    }

    #[test]
    fn process_manager_v2_dual_arg_payload_roundtrip() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
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
        let args = WaitPidV2Args::decode(req.as_slice()).expect("decode");
        assert_eq!(args, WaitPidV2Args::new(42, 0x10));

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
    fn posix_personality_shim_end_to_end_open_getpid_and_exit() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
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
        assert_eq!(getpid.error_code(), None);
        assert_eq!(getpid.ret0(), 42);

        let mut openat = TrapFrame::new(LINUX_NR_OPENAT, [0, 0x2000, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut openat);
        assert_eq!(openat.error_code(), None);
        assert_eq!(openat.ret0(), 3);

        let mut exit = TrapFrame::new(LINUX_NR_EXIT, [5, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut exit);
        assert_eq!(exit.error_code(), None);

        let proc_getpid = state.ipc_recv(proc_req_recv).expect("recv").expect("msg");
        assert_eq!(proc_getpid.opcode, PROC_OP_GETPID);
        let proc_exit = state.ipc_recv(proc_req_recv).expect("recv").expect("msg");
        assert_eq!(proc_exit.opcode, PROC_OP_EXIT);

        let vfs_open = state.ipc_recv(vfs_req_recv).expect("recv").expect("msg");
        assert_eq!(vfs_open.opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn posix_personality_deterministic_sequence_is_stable() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();
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
            assert_eq!(frame.error_code(), None);
            observed[i] = frame.ret0();
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

    #[test]
    fn posix_personality_mixed_flow_with_notification_route_is_deterministic() {
        let mut state = Bootstrap::init().expect("init");
        let mut bindings = PosixServiceBindings::default();

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

        let (_notif, notif_cap, notif_recv) = state.create_notification(8).expect("notif");
        state.bind_irq_notification(9, notif_cap).expect("bind");

        state
            .ipc_send(
                proc_rep_send,
                Message::with_header(0, PROC_OP_GETPID, 0, None, &700u64.to_le_bytes())
                    .expect("pid"),
            )
            .expect("seed pid");
        state
            .ipc_send(
                vfs_rep_send,
                Message::with_header(0, VFS_OP_OPENAT, 0, None, &11u64.to_le_bytes()).expect("fd"),
            )
            .expect("seed fd");

        let mut getpid = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut getpid);
        assert_eq!(getpid.ret0(), 700);

        state
            .handle_trap_event(crate::kernel::trap::TrapEvent::ExternalInterrupt(9), None)
            .expect("irq");

        let mut openat = TrapFrame::new(LINUX_NR_OPENAT, [0, 0x1234, 0, 0, 0, 0]);
        dispatch(&mut state, &bindings, &mut openat);
        assert_eq!(openat.ret0(), 11);

        let proc_req = state.ipc_recv(proc_req_recv).expect("recv").expect("msg");
        let vfs_req = state.ipc_recv(vfs_req_recv).expect("recv").expect("msg");
        let notif = state.ipc_recv(notif_recv).expect("recv").expect("msg");

        assert_eq!(proc_req.opcode, PROC_OP_GETPID);
        assert_eq!(vfs_req.opcode, VFS_OP_OPENAT);
        assert_eq!(notif.opcode, 9);
    }

    #[test]
    fn posix_dispatch_table_is_frozen_contract() {
        let expected = [
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
            LINUX_NR_SOCKET,
            LINUX_NR_CONNECT,
            LINUX_NR_SENDTO,
            LINUX_NR_BRK,
            LINUX_NR_MUNMAP,
            LINUX_NR_MMAP,
            LINUX_NR_MPROTECT,
        ];
        assert_eq!(PosixCompatSyscall::DISPATCH_TABLE, expected);
        assert_eq!(LINUX_COMPAT_SYSCALL_COUNT, expected.len());
    }
}

pub mod service;
pub use service::run;
