// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::trapframe::TrapFrame;
#[cfg(test)]
use crate::kernel::ipc::Message;
use crate::services::compatibility::posix_compat::{
    LINUX_NR_CLOSE, LINUX_NR_CONNECT, LINUX_NR_EXIT, LINUX_NR_GETPID, LINUX_NR_GETPPID,
    LINUX_NR_OPENAT, LINUX_NR_READ, LINUX_NR_SENDTO, LINUX_NR_SOCKET, LINUX_NR_WRITE,
    POSIX_COMPAT_ABI_VERSION, PosixErrno, PosixServiceBindings, dispatch,
};

/// Runtime-facing sysdeps client that speaks to process/vfs managers through
/// the POSIX compatibility syscall dispatch and IPC bindings.
///
/// Deprecated in-process service ownership has been intentionally removed.
#[derive(Debug)]
pub struct PosixSysdepsContext<'a> {
    pub kernel: &'a mut KernelState,
    bindings: PosixServiceBindings,
}

impl<'a> PosixSysdepsContext<'a> {
    pub fn new(kernel: &'a mut KernelState) -> Self {
        Self {
            kernel,
            bindings: PosixServiceBindings::default(),
        }
    }

    pub fn register_process_manager(
        &mut self,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), PosixErrno> {
        self.bindings
            .register_process_manager(self.kernel, request_send_cap, reply_recv_cap)
            .map_err(PosixErrno::from)
    }

    pub fn register_vfs_manager(
        &mut self,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), PosixErrno> {
        self.bindings
            .register_vfs_manager(self.kernel, request_send_cap, reply_recv_cap)
            .map_err(PosixErrno::from)
    }

    pub fn register_socket_manager(
        &mut self,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), PosixErrno> {
        self.bindings
            .register_socket_manager(self.kernel, request_send_cap, reply_recv_cap)
            .map_err(PosixErrno::from)
    }

    pub const fn abi_version(&self) -> u16 {
        POSIX_COMPAT_ABI_VERSION
    }

    fn decode_ret(ret: usize) -> Result<usize, PosixErrno> {
        let signed = ret as isize;
        if signed < 0 {
            let errno = i32::try_from(-signed).map_err(|_| PosixErrno::Inval)?;
            return Err(PosixErrno::from_raw_errno(errno));
        }
        Ok(ret)
    }

    fn run_syscall(&mut self, nr: usize, args: [usize; 6]) -> Result<usize, PosixErrno> {
        let mut frame = TrapFrame::new(nr, args);
        dispatch(self.kernel, &self.bindings, &mut frame);
        if let Some(errno) = frame.error_code() {
            let raw = i32::try_from(errno).map_err(|_| PosixErrno::Inval)?;
            return Err(PosixErrno::from_raw_errno(raw));
        }
        Self::decode_ret(frame.ret0())
    }

    pub fn clock_gettime_hook(&mut self) -> Result<u64, PosixErrno> {
        Ok(self.kernel.scheduler_tick_now().saturating_mul(1_000_000))
    }

    pub fn nanosleep_hook(&mut self, nanos: u64) -> Result<(), PosixErrno> {
        if nanos == 0 {
            return Ok(());
        }
        let ticks = nanos.saturating_add(999_999) / 1_000_000;
        for _ in 0..ticks {
            let _ = self.kernel.scheduler_tick_advance();
        }
        Ok(())
    }

    pub fn getpid_hook(&mut self) -> Result<u64, PosixErrno> {
        let pid = self.run_syscall(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0])?;
        u64::try_from(pid).map_err(|_| PosixErrno::Inval)
    }

    pub fn getppid_hook(&mut self) -> Result<u64, PosixErrno> {
        let ppid = self.run_syscall(LINUX_NR_GETPPID, [0, 0, 0, 0, 0, 0])?;
        u64::try_from(ppid).map_err(|_| PosixErrno::Inval)
    }

    pub fn exit_hook(&mut self, code: u64) -> Result<(), PosixErrno> {
        self.run_syscall(LINUX_NR_EXIT, [code as usize, 0, 0, 0, 0, 0])
            .map(|_| ())
    }

    pub fn openat_hook(
        &mut self,
        path_ptr: usize,
        flags: u32,
        mode: u32,
    ) -> Result<i32, PosixErrno> {
        let fd = self.run_syscall(
            LINUX_NR_OPENAT,
            [0, path_ptr, flags as usize, mode as usize, 0, 0],
        )?;
        i32::try_from(fd).map_err(|_| PosixErrno::Inval)
    }

    pub fn socket_hook(
        &mut self,
        domain: i32,
        sock_type: i32,
        protocol: i32,
    ) -> Result<i32, PosixErrno> {
        let fd = self.run_syscall(
            LINUX_NR_SOCKET,
            [domain as usize, sock_type as usize, protocol as usize, 0, 0, 0],
        )?;
        i32::try_from(fd).map_err(|_| PosixErrno::Inval)
    }

    pub fn connect_hook(
        &mut self,
        fd: i32,
        addr_ptr: usize,
        addr_len: usize,
    ) -> Result<(), PosixErrno> {
        self.run_syscall(
            LINUX_NR_CONNECT,
            [fd as usize, addr_ptr, addr_len, 0, 0, 0],
        )
        .map(|_| ())
    }

    pub fn sendto_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        len: usize,
        flags: i32,
        dest_addr_ptr: usize,
        addrlen: usize,
    ) -> Result<usize, PosixErrno> {
        self.run_syscall(
            LINUX_NR_SENDTO,
            [
                fd as usize,
                buf_ptr,
                len,
                flags as usize,
                dest_addr_ptr,
                addrlen,
            ],
        )
    }

    pub fn read_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, PosixErrno> {
        self.run_syscall(
            LINUX_NR_READ,
            [fd as usize, buf_ptr, buf_len, 0, 0, 0],
        )
    }

    pub fn write_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, PosixErrno> {
        self.run_syscall(
            LINUX_NR_WRITE,
            [fd as usize, buf_ptr, buf_len, 0, 0, 0],
        )
    }

    pub fn close_hook(&mut self, fd: i32) -> Result<(), PosixErrno> {
        self.run_syscall(LINUX_NR_CLOSE, [fd as usize, 0, 0, 0, 0, 0])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::std::thread;
    use yarm_ipc_abi::process_abi::{PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID};
    use yarm_ipc_abi::socket_abi::{
        ConnectArgs, SOCKET_OP_CONNECT, SOCKET_OP_SENDTO, SOCKET_OP_SOCKET, SendToArgs,
    };
    use yarm_ipc_abi::vfs_abi::{VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_WRITE};

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
    fn service_backed_clock_hooks_use_kernel_timer() {
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("init");
            let mut ctx = PosixSysdepsContext::new(&mut kernel);
            assert_eq!(ctx.abi_version(), POSIX_COMPAT_ABI_VERSION);
            assert_eq!(ctx.clock_gettime_hook().expect("before"), 0);
            ctx.nanosleep_hook(2_500_000).expect("sleep");
            assert_eq!(ctx.clock_gettime_hook().expect("after"), 3_000_000);
        });
    }

    #[test]
    fn proc_and_vfs_hooks_route_via_ipc_bindings() {
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("init");
            kernel.register_task(41).expect("task");
            kernel.enqueue_current_cpu(41).expect("enqueue");
            let _ = kernel.dispatch_next_task().expect("dispatch");

            let (_, proc_req_send, proc_req_recv) = kernel.create_endpoint(8).expect("proc req");
            let (_, proc_rep_send, proc_rep_recv) = kernel.create_endpoint(8).expect("proc rep");
            let (_, vfs_req_send, vfs_req_recv) = kernel.create_endpoint(8).expect("vfs req");
            let (_, vfs_rep_send, vfs_rep_recv) = kernel.create_endpoint(8).expect("vfs rep");

            let mut ctx = PosixSysdepsContext::new(&mut kernel);
            ctx.register_process_manager(proc_req_send, proc_rep_recv)
                .expect("bind proc");
            ctx.register_vfs_manager(vfs_req_send, vfs_rep_recv)
                .expect("bind vfs");

            ctx.kernel
                .ipc_send(
                    proc_rep_send,
                    Message::with_header(0, PROC_OP_GETPID, 0, None, &41u64.to_le_bytes())
                        .expect("pid"),
                )
                .expect("seed pid");
            ctx.kernel
                .ipc_send(
                    proc_rep_send,
                    Message::with_header(0, PROC_OP_GETPPID, 0, None, &40u64.to_le_bytes())
                        .expect("ppid"),
                )
                .expect("seed ppid");
            ctx.kernel
                .ipc_send(
                    vfs_rep_send,
                    Message::with_header(0, VFS_OP_OPENAT, 0, None, &3u64.to_le_bytes())
                        .expect("open"),
                )
                .expect("seed open");
            ctx.kernel
                .ipc_send(
                    vfs_rep_send,
                    Message::with_header(0, VFS_OP_READ, 0, None, &128u64.to_le_bytes())
                        .expect("read"),
                )
                .expect("seed read");
            ctx.kernel
                .ipc_send(
                    vfs_rep_send,
                    Message::with_header(0, VFS_OP_WRITE, 0, None, &11u64.to_le_bytes())
                        .expect("write"),
                )
                .expect("seed write");
            ctx.kernel
                .ipc_send(
                    vfs_rep_send,
                    Message::with_header(0, VFS_OP_CLOSE, 0, None, &0u64.to_le_bytes())
                        .expect("close"),
                )
                .expect("seed close");

            assert_eq!(ctx.getpid_hook().expect("getpid"), 41);
            assert_eq!(ctx.getppid_hook().expect("getppid"), 40);
            ctx.exit_hook(7).expect("exit");
            let proc_req0 = ctx
                .kernel
                .ipc_recv(proc_req_recv)
                .expect("recv proc 0")
                .expect("proc req 0");
            let proc_req1 = ctx
                .kernel
                .ipc_recv(proc_req_recv)
                .expect("recv proc 1")
                .expect("proc req 1");
            let proc_req2 = ctx
                .kernel
                .ipc_recv(proc_req_recv)
                .expect("recv proc 2")
                .expect("proc req 2");
            let proc_opcodes = [proc_req0.opcode, proc_req1.opcode, proc_req2.opcode];
            assert!(proc_opcodes.contains(&PROC_OP_EXIT));

            let fd = ctx.openat_hook(0x1000, 0, 0).expect("open");
            assert_eq!(fd, 3);
            assert_eq!(ctx.read_hook(fd, 0x2000, 128).expect("read"), 128);
            assert_eq!(ctx.write_hook(fd, 0x2000, 11).expect("write"), 11);
            ctx.close_hook(fd).expect("close");

            let req0 = ctx
                .kernel
                .ipc_recv(vfs_req_recv)
                .expect("recv vfs 0")
                .expect("vfs req 0");
            let req1 = ctx
                .kernel
                .ipc_recv(vfs_req_recv)
                .expect("recv vfs 1")
                .expect("vfs req 1");
            let req2 = ctx
                .kernel
                .ipc_recv(vfs_req_recv)
                .expect("recv vfs 2")
                .expect("vfs req 2");
            let req3 = ctx
                .kernel
                .ipc_recv(vfs_req_recv)
                .expect("recv vfs 3")
                .expect("vfs req 3");
            let opcodes = [req0.opcode, req1.opcode, req2.opcode, req3.opcode];
            assert!(opcodes.contains(&VFS_OP_OPENAT));
        });
    }

    #[test]
    fn socket_hook_routes_via_socket_binding_dispatch() {
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("init");
            let (_, socket_req_send, socket_req_recv) =
                kernel.create_endpoint(8).expect("socket req");
            let (_, socket_rep_send, socket_rep_recv) =
                kernel.create_endpoint(8).expect("socket rep");
            let mut ctx = PosixSysdepsContext::new(&mut kernel);
            ctx.register_socket_manager(socket_req_send, socket_rep_recv)
                .expect("bind socket");
            ctx.kernel
                .ipc_send(
                    socket_rep_send,
                    Message::with_header(0, SOCKET_OP_SOCKET, 0, None, &1001u64.to_le_bytes())
                        .expect("socket reply"),
                )
                .expect("seed socket reply");
            assert_eq!(ctx.socket_hook(2, 1, 0).expect("socket"), 1001);
            let socket_req = ctx
                .kernel
                .ipc_recv(socket_req_recv)
                .expect("recv socket req")
                .expect("socket req");
            assert_eq!(socket_req.opcode, SOCKET_OP_SOCKET);

            ctx.kernel
                .ipc_send(
                    socket_rep_send,
                    Message::with_header(0, SOCKET_OP_CONNECT, 0, None, &0u64.to_le_bytes())
                        .expect("connect reply"),
                )
                .expect("seed connect reply");
            ctx.connect_hook(1001, 0xCAFE, 16).expect("connect");
            let connect_req = ctx
                .kernel
                .ipc_recv(socket_req_recv)
                .expect("recv connect req")
                .expect("connect req");
            assert_eq!(connect_req.opcode, SOCKET_OP_CONNECT);
            let args = ConnectArgs::decode(connect_req.as_slice()).expect("decode connect args");
            assert_eq!(args.fd, 1001);
            assert_eq!(args.addr_ptr, 0xCAFE);
            assert_eq!(args.addr_len, 16);

            ctx.kernel
                .ipc_send(
                    socket_rep_send,
                    Message::with_header(0, SOCKET_OP_SENDTO, 0, None, &7u64.to_le_bytes())
                        .expect("sendto reply"),
                )
                .expect("seed sendto reply");
            let sent = ctx
                .sendto_hook(1001, 0xBEEF, 7, 0, 0xD00D, 16)
                .expect("sendto");
            assert_eq!(sent, 7);
            let sendto_req = ctx
                .kernel
                .ipc_recv(socket_req_recv)
                .expect("recv sendto req")
                .expect("sendto req");
            assert_eq!(sendto_req.opcode, SOCKET_OP_SENDTO);
            let sendto_args = SendToArgs::decode(sendto_req.as_slice()).expect("decode sendto");
            assert_eq!(sendto_args.fd, 1001);
            assert_eq!(sendto_args.buf_ptr, 0xBEEF);
            assert_eq!(sendto_args.len, 7);
            assert_eq!(sendto_args.flags, 0);
            assert_eq!(sendto_args.dest_addr_ptr, 0xD00D);
            assert_eq!(sendto_args.addrlen, 16);
        });
    }

    #[test]
    fn sendto_hook_propagates_negative_errno_from_socket_reply() {
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("init");
            let (_, socket_req_send, socket_req_recv) = kernel.create_endpoint(8).expect("socket req");
            let (_, socket_rep_send, socket_rep_recv) = kernel.create_endpoint(8).expect("socket rep");
            let mut ctx = PosixSysdepsContext::new(&mut kernel);
            ctx.register_socket_manager(socket_req_send, socket_rep_recv)
                .expect("bind socket");

            let errno = crate::services::compatibility::posix_compat::EINVAL as i64;
            ctx.kernel
                .ipc_send(
                    socket_rep_send,
                    Message::with_header(0, SOCKET_OP_SENDTO, 0, None, &(-errno).to_le_bytes())
                        .expect("sendto error reply"),
                )
                .expect("seed sendto error reply");

            let err = ctx
                .sendto_hook(1001, 0xBEEF, 7, 0, 0xD00D, 16)
                .expect_err("sendto should fail");
            assert_eq!(err, PosixErrno::Inval);

            let sendto_req = ctx
                .kernel
                .ipc_recv(socket_req_recv)
                .expect("recv sendto req")
                .expect("sendto req");
            assert_eq!(sendto_req.opcode, SOCKET_OP_SENDTO);
            let sendto_args = SendToArgs::decode(sendto_req.as_slice()).expect("decode sendto");
            assert_eq!(sendto_args.fd, 1001);
            assert_eq!(sendto_args.buf_ptr, 0xBEEF);
            assert_eq!(sendto_args.len, 7);
            assert_eq!(sendto_args.flags, 0);
            assert_eq!(sendto_args.dest_addr_ptr, 0xD00D);
            assert_eq!(sendto_args.addrlen, 16);
        });
    }

    #[test]
    fn connect_hook_propagates_negative_errno_from_socket_reply() {
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("init");
            let (_, socket_req_send, socket_req_recv) = kernel.create_endpoint(8).expect("socket req");
            let (_, socket_rep_send, socket_rep_recv) = kernel.create_endpoint(8).expect("socket rep");
            let mut ctx = PosixSysdepsContext::new(&mut kernel);
            ctx.register_socket_manager(socket_req_send, socket_rep_recv)
                .expect("bind socket");

            let errno = crate::services::compatibility::posix_compat::EINVAL as i64;
            ctx.kernel
                .ipc_send(
                    socket_rep_send,
                    Message::with_header(0, SOCKET_OP_CONNECT, 0, None, &(-errno).to_le_bytes())
                        .expect("connect error reply"),
                )
                .expect("seed connect error reply");

            let err = ctx
                .connect_hook(1001, 0xCAFE, 16)
                .expect_err("connect should fail");
            assert_eq!(err, PosixErrno::Inval);

            let connect_req = ctx
                .kernel
                .ipc_recv(socket_req_recv)
                .expect("recv connect req")
                .expect("connect req");
            assert_eq!(connect_req.opcode, SOCKET_OP_CONNECT);
            let connect_args = ConnectArgs::decode(connect_req.as_slice()).expect("decode connect");
            assert_eq!(connect_args.fd, 1001);
            assert_eq!(connect_args.addr_ptr, 0xCAFE);
            assert_eq!(connect_args.addr_len, 16);
        });
    }
}
