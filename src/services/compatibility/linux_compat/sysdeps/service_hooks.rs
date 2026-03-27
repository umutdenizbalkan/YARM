use crate::kernel::boot::KernelState;
use crate::kernel::ipc::Message;
use crate::kernel::process::ProcessService;
use crate::kernel::process_abi::{PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID};
use crate::kernel::vfs::{
    CloseRequest, OpenAtRequest, ReadWriteRequest, VfsBackend, close_message, openat_message,
    read_message, write_message,
};
use crate::services::common::service::FsService;
use crate::services::common::vfs_service::VfsReply;
use crate::services::compatibility::linux_compat::LinuxErrno;
use crate::services::network::socket::service::SocketAdapterService;

pub struct LinuxSysdepsContext<'a, B: VfsBackend> {
    pub kernel: &'a mut KernelState,
    pub proc_service: &'a mut ProcessService,
    pub vfs_service: &'a mut FsService<B>,
    pub socket_service: &'a mut SocketAdapterService,
}

impl<'a, B: VfsBackend> LinuxSysdepsContext<'a, B> {
    pub fn new(
        kernel: &'a mut KernelState,
        proc_service: &'a mut ProcessService,
        vfs_service: &'a mut FsService<B>,
        socket_service: &'a mut SocketAdapterService,
    ) -> Self {
        Self {
            kernel,
            proc_service,
            vfs_service,
            socket_service,
        }
    }

    fn decode_u64(reply: Message) -> Result<usize, LinuxErrno> {
        let value = VfsReply::from_message(reply)
            .map_err(|_| LinuxErrno::Inval)?
            .as_u64();
        usize::try_from(value).map_err(|_| LinuxErrno::Inval)
    }

    pub fn clock_gettime_hook(&mut self) -> Result<u64, LinuxErrno> {
        Ok(self
            .kernel
            .timer
            .current_ticks()
            .0
            .saturating_mul(1_000_000))
    }

    pub fn nanosleep_hook(&mut self, nanos: u64) -> Result<(), LinuxErrno> {
        if nanos == 0 {
            return Ok(());
        }
        let ticks = nanos.saturating_add(999_999) / 1_000_000;
        for _ in 0..ticks {
            let _ = self.kernel.timer.tick_and_check();
        }
        Ok(())
    }

    pub fn getpid_hook(&mut self) -> Result<u64, LinuxErrno> {
        let tid = self.kernel.current_tid().ok_or(LinuxErrno::NoSys)?;
        let reply = self
            .proc_service
            .handle(
                Message::with_header(0, PROC_OP_GETPID, 0, None, &tid.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(Self::decode_u64(reply)? as u64)
    }

    pub fn getppid_hook(&mut self) -> Result<u64, LinuxErrno> {
        let tid = self.kernel.current_tid().ok_or(LinuxErrno::NoSys)?;
        let reply = self
            .proc_service
            .handle(
                Message::with_header(0, PROC_OP_GETPPID, 0, None, &tid.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(Self::decode_u64(reply)? as u64)
    }

    pub fn exit_hook(&mut self, code: u64) -> Result<(), LinuxErrno> {
        let tid = self.kernel.current_tid().ok_or(LinuxErrno::NoSys)?;
        self.proc_service
            .handle(
                Message::with_header(tid, PROC_OP_EXIT, 0, None, &code.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(())
    }

    pub fn openat_hook(
        &mut self,
        path_ptr: usize,
        flags: u32,
        mode: u32,
    ) -> Result<i32, LinuxErrno> {
        let reply = self
            .vfs_service
            .handle(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: path_ptr as u64,
                    flags: flags as u64,
                    mode: mode as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        i32::try_from(Self::decode_u64(reply)?).map_err(|_| LinuxErrno::Inval)
    }

    pub fn socket_hook(
        &mut self,
        domain: i32,
        sock_type: i32,
        protocol: i32,
    ) -> Result<i32, LinuxErrno> {
        self.socket_service
            .open(domain, sock_type, protocol)
            .map_err(|_| LinuxErrno::Inval)
    }

    pub fn read_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self
                .socket_service
                .read(fd, buf_len)
                .map_err(|_| LinuxErrno::Inval);
        }
        let reply = self
            .vfs_service
            .handle(
                read_message(ReadWriteRequest {
                    fd: fd as u64,
                    buf_ptr: buf_ptr as u64,
                    len: buf_len as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Self::decode_u64(reply)
    }

    pub fn write_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self
                .socket_service
                .write(fd, buf_len)
                .map_err(|_| LinuxErrno::Inval);
        }
        let reply = self
            .vfs_service
            .handle(
                write_message(ReadWriteRequest {
                    fd: fd as u64,
                    buf_ptr: buf_ptr as u64,
                    len: buf_len as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Self::decode_u64(reply)
    }

    pub fn close_hook(&mut self, fd: i32) -> Result<(), LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self.socket_service.close(fd).map_err(|_| LinuxErrno::Inval);
        }
        self.vfs_service
            .handle(close_message(CloseRequest { fd: fd as u64 }).map_err(|_| LinuxErrno::Inval)?)
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::vfs::InMemoryBackend;

    #[test]
    fn service_backed_clock_hooks_use_kernel_timer() {
        let mut kernel = Bootstrap::init().expect("init");
        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);
        assert_eq!(ctx.clock_gettime_hook().expect("before"), 0);
        ctx.nanosleep_hook(2_500_000).expect("sleep");
        assert_eq!(ctx.clock_gettime_hook().expect("after"), 3_000_000);
    }

    #[test]
    fn service_backed_proc_and_vfs_hooks_roundtrip_real_services() {
        let mut kernel = Bootstrap::init().expect("init");
        kernel.register_task(41).expect("task");
        kernel.enqueue_current_cpu(41).expect("enqueue");
        kernel.dispatch_next_task().expect("dispatch");
        kernel.yield_current().expect("switch");

        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);

        assert_eq!(ctx.getpid_hook().expect("getpid"), 41);
        assert_eq!(ctx.getppid_hook().expect("getppid"), 40);
        ctx.exit_hook(0).expect("exit");

        let fd = ctx.openat_hook(0x1000, 0, 0).expect("open");
        assert!(fd >= 3);
        assert_eq!(ctx.read_hook(fd, 0x2000, 128).expect("read"), 128);
        assert_eq!(ctx.write_hook(fd, 0x2000, 11).expect("write"), 11);
        ctx.close_hook(fd).expect("close");
        assert_eq!(ctx.read_hook(fd, 0x2000, 1), Err(LinuxErrno::Inval));
    }

    #[test]
    fn socket_hooks_route_through_socket_service() {
        let mut kernel = Bootstrap::init().expect("init");
        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);
        let fd = ctx.socket_hook(2, 1, 0).expect("socket");
        assert!(fd >= 1000);
        assert_eq!(ctx.read_hook(fd, 0, 128).expect("read"), 64);
        assert_eq!(ctx.write_hook(fd, 0, 32).expect("write"), 32);
        ctx.close_hook(fd).expect("close");
    }
}
