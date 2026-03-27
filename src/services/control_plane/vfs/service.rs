use crate::kernel::boot::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::vfs::VfsError;
use crate::kernel::vfs::{
    InMemoryBackend, OpenAtRequest, ReadWriteRequest, StatxRequest, VfsBackend, close_message,
    dup_message, epoll_create1_message, epoll_ctl_message, epoll_pwait_message, fcntl_message,
    ioctl_message, openat_message, poll_message, read_message, sendfile_message, statx_message,
    write_message,
};
use crate::services::common::service::{FsService, run_typed_request_loop};
use crate::services::common::vfs_service::VfsReply;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsLoopSummary {
    pub fd: u64,
    pub dup_fd: u64,
    pub epoll_fd: u64,
    pub handled: usize,
}

fn decode_fd_reply(reply: crate::kernel::ipc::Message) -> Result<u64, VfsError> {
    match VfsReply::from_message(reply)? {
        VfsReply::OpenAtFd(fd) | VfsReply::DupFd(fd) | VfsReply::EpollFd(fd) => Ok(fd),
        _ => Err(VfsError::Malformed),
    }
}

pub fn run_request_loop(
    vfs: &mut FsService<InMemoryBackend>,
    path_ptr: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let reply = run_typed_request_loop(
        vfs,
        [openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr,
            flags: 0,
            mode: 0,
        })
        .map_err(|_| VfsError::Malformed)?],
    )?[0];
    let fd = decode_fd_reply(reply)?;
    let dup_fd = decode_fd_reply(
        run_typed_request_loop(vfs, [dup_message(fd).map_err(|_| VfsError::Malformed)?])?[0],
    )?;
    let epoll_fd = decode_fd_reply(
        run_typed_request_loop(
            vfs,
            [epoll_create1_message(0).map_err(|_| VfsError::Malformed)?],
        )?[0],
    )?;
    let _ = run_typed_request_loop(
        vfs,
        [
            read_message(ReadWriteRequest {
                fd,
                buf_ptr: 0x2000,
                len: 64,
            })
            .map_err(|_| VfsError::Malformed)?,
            write_message(ReadWriteRequest {
                fd,
                buf_ptr: 0x3000,
                len: 32,
            })
            .map_err(|_| VfsError::Malformed)?,
            statx_message(StatxRequest {
                dirfd: 0,
                path_ptr,
                flags: 0,
                mask_or_buf: 0,
            })
            .map_err(|_| VfsError::Malformed)?,
            ioctl_message(fd, 0x1234, 0x55).map_err(|_| VfsError::Malformed)?,
            fcntl_message(fd, 3, 9).map_err(|_| VfsError::Malformed)?,
            poll_message(0x9000, 2, 10).map_err(|_| VfsError::Malformed)?,
            epoll_ctl_message(epoll_fd, 1, fd, 0xA000).map_err(|_| VfsError::Malformed)?,
            epoll_pwait_message(epoll_fd, 0xB000, 4, 10).map_err(|_| VfsError::Malformed)?,
            sendfile_message(fd, dup_fd, 0xC000, 99).map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd: dup_fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd: epoll_fd })
                .map_err(|_| VfsError::Malformed)?,
        ],
    )?;
    Ok(VfsLoopSummary {
        fd,
        dup_fd,
        epoll_fd,
        handled: vfs.handled_count(),
    })
}

fn map_kernel_ipc_err<T>(
    result: Result<T, crate::kernel::boot::KernelError>,
) -> Result<T, VfsError> {
    result.map_err(|_| VfsError::Unsupported)
}

fn roundtrip_ipc<B: VfsBackend>(
    kernel: &mut KernelState,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    server_send_cap: CapId,
    client_recv_cap: CapId,
    request: crate::kernel::ipc::Message,
) -> Result<crate::kernel::ipc::Message, VfsError> {
    map_kernel_ipc_err(kernel.ipc_send(client_send_cap, request))?;
    let request_for_server =
        map_kernel_ipc_err(kernel.ipc_recv(server_recv_cap))?.ok_or(VfsError::Malformed)?;
    let response = vfs.handle(request_for_server)?;
    map_kernel_ipc_err(kernel.ipc_send(server_send_cap, response))?;
    map_kernel_ipc_err(kernel.ipc_recv(client_recv_cap))?.ok_or(VfsError::Malformed)
}

pub fn run_request_loop_over_kernel_ipc(
    kernel: &mut KernelState,
    vfs: &mut FsService<impl VfsBackend>,
    path_ptr: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let (_, client_send_cap, server_recv_cap) = map_kernel_ipc_err(kernel.create_endpoint(16))?;
    let (_, server_send_cap, client_recv_cap) = map_kernel_ipc_err(kernel.create_endpoint(16))?;

    let open_reply = roundtrip_ipc(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr,
            flags: 0,
            mode: 0,
        })
        .map_err(|_| VfsError::Malformed)?,
    )?;
    let fd = decode_fd_reply(open_reply)?;

    let dup_fd = decode_fd_reply(roundtrip_ipc(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        dup_message(fd).map_err(|_| VfsError::Malformed)?,
    )?)?;

    let epoll_fd = decode_fd_reply(roundtrip_ipc(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        epoll_create1_message(0).map_err(|_| VfsError::Malformed)?,
    )?)?;

    let requests = [
        read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0x2000,
            len: 64,
        })
        .map_err(|_| VfsError::Malformed)?,
        write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0x3000,
            len: 32,
        })
        .map_err(|_| VfsError::Malformed)?,
        statx_message(StatxRequest {
            dirfd: 0,
            path_ptr,
            flags: 0,
            mask_or_buf: 0,
        })
        .map_err(|_| VfsError::Malformed)?,
        ioctl_message(fd, 0x1234, 0x55).map_err(|_| VfsError::Malformed)?,
        fcntl_message(fd, 3, 9).map_err(|_| VfsError::Malformed)?,
        poll_message(0x9000, 2, 10).map_err(|_| VfsError::Malformed)?,
        epoll_ctl_message(epoll_fd, 1, fd, 0xA000).map_err(|_| VfsError::Malformed)?,
        epoll_pwait_message(epoll_fd, 0xB000, 4, 10).map_err(|_| VfsError::Malformed)?,
        sendfile_message(fd, dup_fd, 0xC000, 99).map_err(|_| VfsError::Malformed)?,
        close_message(crate::kernel::vfs::CloseRequest { fd: dup_fd })
            .map_err(|_| VfsError::Malformed)?,
        close_message(crate::kernel::vfs::CloseRequest { fd }).map_err(|_| VfsError::Malformed)?,
        close_message(crate::kernel::vfs::CloseRequest { fd: epoll_fd })
            .map_err(|_| VfsError::Malformed)?,
    ];
    for request in requests {
        let _ = roundtrip_ipc(
            kernel,
            vfs,
            client_send_cap,
            server_recv_cap,
            server_send_cap,
            client_recv_cap,
            request,
        )?;
    }

    Ok(VfsLoopSummary {
        fd,
        dup_fd,
        epoll_fd,
        handled: vfs.handled_count(),
    })
}

pub fn run() {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let summary = run_request_loop(&mut vfs, 0x1000).expect("vfs loop");

    crate::yarm_log!(
        "vfs request-loop ready: fd={}, dup_fd={}, epoll_fd={}, handled={}",
        summary.fd,
        summary.dup_fd,
        summary.epoll_fd,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::services::fs::devfs::{DEV_NULL_PATH_PTR, DevFsBackend};
    use crate::services::fs::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
    use crate::services::fs::ramfs::RamFsBackend;

    fn setup_ipc_caps(kernel: &mut KernelState) -> (CapId, CapId, CapId, CapId) {
        let (_, client_send_cap, server_recv_cap) =
            map_kernel_ipc_err(kernel.create_endpoint(8)).expect("req endpoint");
        let (_, server_send_cap, client_recv_cap) =
            map_kernel_ipc_err(kernel.create_endpoint(8)).expect("rep endpoint");
        (
            client_send_cap,
            server_recv_cap,
            server_send_cap,
            client_recv_cap,
        )
    }

    #[test]
    fn vfs_request_loop_entrypoint_opens_one_fd() {
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let summary = run_request_loop(&mut vfs, 0x1010).expect("loop");

        assert_eq!(summary.fd, 3);
        assert_eq!(summary.dup_fd, 4);
        assert_eq!(summary.epoll_fd, 5);
        assert_eq!(summary.handled, 15);
    }

    #[test]
    fn vfs_request_loop_can_roundtrip_over_kernel_ipc() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let summary =
            run_request_loop_over_kernel_ipc(&mut kernel, &mut vfs, 0x1010).expect("loop");

        assert_eq!(summary.fd, 3);
        assert_eq!(summary.dup_fd, 4);
        assert_eq!(summary.epoll_fd, 5);
        assert_eq!(summary.handled, 15);
    }

    #[test]
    fn devfs_and_ramfs_conformance_roundtrip_over_kernel_ipc() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let (client_send, server_recv, server_send, client_recv) = setup_ipc_caps(&mut kernel);

        let mut devfs = FsService::with_backend(DevFsBackend::default());
        let open_dev = roundtrip_ipc(
            &mut kernel,
            &mut devfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: DEV_NULL_PATH_PTR,
                flags: 0,
                mode: 0,
            })
            .expect("open msg"),
        )
        .expect("open devfs");
        let dev_fd = decode_fd_reply(open_dev).expect("fd");
        let write_dev = roundtrip_ipc(
            &mut kernel,
            &mut devfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            write_message(ReadWriteRequest {
                fd: dev_fd,
                buf_ptr: 0,
                len: 9,
            })
            .expect("write msg"),
        )
        .expect("write devfs");
        assert_eq!(
            VfsReply::from_message(write_dev).expect("decode"),
            VfsReply::WriteLen(9)
        );

        let mut ramfs = FsService::with_backend(RamFsBackend::new());
        let open_ram = roundtrip_ipc(
            &mut kernel,
            &mut ramfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: 0xABCD,
                flags: 0,
                mode: 0,
            })
            .expect("open msg"),
        )
        .expect("open ramfs");
        let ram_fd = decode_fd_reply(open_ram).expect("fd");
        let read_ram = roundtrip_ipc(
            &mut kernel,
            &mut ramfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            read_message(ReadWriteRequest {
                fd: ram_fd,
                buf_ptr: 0,
                len: 4,
            })
            .expect("read msg"),
        )
        .expect("read ramfs");
        assert_eq!(
            VfsReply::from_message(read_ram).expect("decode"),
            VfsReply::ReadLen(0)
        );
    }

    #[test]
    fn initramfs_write_rejection_roundtrips_over_kernel_ipc() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let (client_send, server_recv, server_send, client_recv) = setup_ipc_caps(&mut kernel);

        let mut initramfs = FsService::with_backend(InitramfsBackend::new(4096));
        let open = roundtrip_ipc(
            &mut kernel,
            &mut initramfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
                flags: 0,
                mode: 0,
            })
            .expect("open msg"),
        )
        .expect("open initramfs");
        let fd = decode_fd_reply(open).expect("fd");
        let write = roundtrip_ipc(
            &mut kernel,
            &mut initramfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            write_message(ReadWriteRequest {
                fd,
                buf_ptr: 0,
                len: 1,
            })
            .expect("write msg"),
        );
        assert_eq!(write, Err(VfsError::Unsupported));
    }
}
