// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(test)]
use yarm::kernel::boot::KernelState;
#[cfg(test)]
use yarm_user_rt::capability::CapId;
use yarm_fs_servers::common::vfs_ipc::VfsError;
use yarm_fs_servers::common::vfs_ipc::{
    InMemoryBackend, OpenAtRequest, ReadWriteRequest, StatxRequest, close_message,
    dup_message, epoll_create1_message, epoll_ctl_message, epoll_pwait_message, fcntl_message,
    ioctl_message, openat_message, poll_message, read_message, sendfile_message, statx_message,
    write_message,
};
#[cfg(test)]
use yarm_fs_servers::common::vfs_ipc::VfsBackend;
use yarm_fs_servers::common::service::FsService;
use yarm_srv_common::service_loop::run_typed_request_loop;
use yarm_srv_common::vfs_reply::VfsReply;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsLoopSummary {
    pub fd: u64,
    pub dup_fd: u64,
    pub epoll_fd: u64,
    pub handled: usize,
}

#[cfg(test)]
const VFS_ROUNDTRIP_RECV_TIMEOUT_TICKS: u64 = 1;

fn decode_fd_reply(reply: yarm_user_rt::ipc::Message) -> Result<u64, VfsError> {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .map_err(|_| VfsError::Malformed)?
        .expect_fd(reply.opcode)
        .map_err(|_| VfsError::Malformed)
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
            close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd: dup_fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd: epoll_fd })
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

#[cfg(test)]
fn map_kernel_ipc_err<T>(
    result: Result<T, yarm::kernel::boot::KernelError>,
) -> Result<T, VfsError> {
    result.map_err(|_| VfsError::Unsupported)
}

#[cfg(test)]
fn map_kernel_ipc_error(_: yarm::kernel::boot::KernelError) -> VfsError {
    VfsError::Unsupported
}

#[cfg(test)]
fn roundtrip_ipc<B: VfsBackend>(
    kernel: &mut KernelState,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    server_send_cap: CapId,
    client_recv_cap: CapId,
    request: yarm_user_rt::ipc::Message,
) -> Result<yarm_user_rt::ipc::Message, VfsError> {
    synthetic_roundtrip_call_reply_with_budget(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        request,
        VFS_ROUNDTRIP_RECV_TIMEOUT_TICKS,
    )
}

#[cfg(test)]
fn synthetic_roundtrip_call_reply_with_budget<B: VfsBackend>(
    kernel: &mut KernelState,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    _server_send_cap: CapId,
    client_recv_cap: CapId,
    request: yarm_user_rt::ipc::Message,
    recv_timeout_ticks: u64,
) -> Result<yarm_user_rt::ipc::Message, VfsError> {
    super::super::ipc_roundtrip::synthetic_roundtrip_call_reply_with_budget(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        request,
        recv_timeout_ticks,
        map_kernel_ipc_error,
        || VfsError::Malformed,
        || VfsError::Unsupported,
    )
}

#[allow(dead_code)]
#[cfg(test)]
fn roundtrip_ipc_with_budget<B: VfsBackend>(
    kernel: &mut KernelState,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    server_send_cap: CapId,
    client_recv_cap: CapId,
    request: yarm_user_rt::ipc::Message,
    recv_timeout_ticks: u64,
) -> Result<yarm_user_rt::ipc::Message, VfsError> {
    synthetic_roundtrip_call_reply_with_budget(
        kernel,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        request,
        recv_timeout_ticks,
    )
}

#[cfg(test)]
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
        close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd: dup_fd })
            .map_err(|_| VfsError::Malformed)?,
        close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd }).map_err(|_| VfsError::Malformed)?,
        close_message(yarm_fs_servers::common::vfs_ipc::CloseRequest { fd: epoll_fd })
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

#[cfg(test)]
pub fn run_with_kernel_ipc(
    kernel: &mut KernelState,
    path_ptr: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    run_request_loop_over_kernel_ipc(kernel, &mut vfs, path_ptr)
}

pub fn run() {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let summary = run_request_loop(&mut vfs, 0x1000).expect("vfs loop");

    yarm_user_rt::user_log!(
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
    use yarm::std::thread;
    use yarm::kernel::boot::Bootstrap;
    use yarm_fs_servers::devfs::{DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH_PTR, DevFsBackend};
    use yarm_fs_servers::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
    use yarm_fs_servers::ramfs::RamFsBackend;

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

    fn with_kernel_roundtrip<B, F>(backend: B, f: F)
    where
        B: VfsBackend,
        F: FnOnce(&mut KernelState, &mut FsService<B>, CapId, CapId, CapId, CapId),
    {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let (client_send, server_recv, server_send, client_recv) = setup_ipc_caps(&mut kernel);
        let mut vfs = FsService::with_backend(backend);
        f(
            &mut kernel,
            &mut vfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
        );
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
        run_with_large_stack(|| {
            let mut kernel = Bootstrap::init().expect("kernel init");
            let mut vfs = FsService::with_backend(InMemoryBackend::new());
            let summary =
                run_request_loop_over_kernel_ipc(&mut kernel, &mut vfs, 0x1010).expect("loop");

            assert_eq!(summary.fd, 3);
            assert_eq!(summary.dup_fd, 4);
            assert_eq!(summary.epoll_fd, 5);
            assert_eq!(summary.handled, 15);
        });
    }

    #[test]
    #[ignore = "stack-heavy vfs integration path overflows in hosted-dev unit-test harness"]
    fn vfs_roundtrip_timed_recv_deadline_times_out_when_queue_empty() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let (_, _client_send_cap, server_recv_cap) =
            map_kernel_ipc_err(kernel.create_endpoint(8)).expect("req endpoint");

        let timed =
            kernel.ipc_recv_with_deadline(server_recv_cap, VFS_ROUNDTRIP_RECV_TIMEOUT_TICKS);
        assert_eq!(timed, Ok(None));
    }

    #[test]
    #[ignore = "stack-heavy vfs integration path overflows in hosted-dev unit-test harness"]
    fn vfs_roundtrip_accepts_explicit_zero_tick_recv_budget_when_messages_are_queued() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let (client_send, server_recv, server_send, client_recv) = setup_ipc_caps(&mut kernel);
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let reply = roundtrip_ipc_with_budget(
            &mut kernel,
            &mut vfs,
            client_send,
            server_recv,
            server_send,
            client_recv,
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: 0x4444,
                flags: 0,
                mode: 0,
            })
            .expect("open"),
            0,
        )
        .expect("roundtrip");
        assert_eq!(
            VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
                .map_err(|_| VfsError::Malformed)
                .expect("decode"),
            VfsReply::OpenAtFd(3)
        );
    }

    #[test]
    fn vfs_source_guardrail_blocks_legacy_blocking_ipc_recv_regression() {
        let src = include_str!("service.rs");
        let legacy_call = ["kernel", ".ipc_recv", "("].concat();
        assert!(
            src.contains("ipc_recv_with_deadline("),
            "phase6 migration requires timed receive path in vfs service"
        );
        assert!(
            src.contains("ipc_reply("),
            "phase6 migration requires reply-cap call/reply path in vfs service"
        );
        assert!(
            !src.contains(legacy_call.as_str()),
            "legacy blocking ipc_recv path is deprecated for migrated vfs control-plane flow"
        );
    }

    #[test]
    #[ignore = "stack-heavy vfs integration path overflows in hosted-dev unit-test harness"]
    fn vfs_run_with_kernel_ipc_bootstraps_server_loop() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let summary = run_with_kernel_ipc(&mut kernel, 0x1010).expect("loop");
        assert_eq!(summary.fd, 3);
        assert_eq!(summary.dup_fd, 4);
        assert_eq!(summary.epoll_fd, 5);
        assert_eq!(summary.handled, 15);
    }

    #[test]
    #[ignore = "stack-heavy vfs integration path overflows in hosted-dev unit-test harness"]
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
            VfsReply::from_opcode_payload_checked(write_dev.opcode, write_dev.as_slice())
                .expect("decode"),
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
            VfsReply::from_opcode_payload_checked(read_ram.opcode, read_ram.as_slice())
                .expect("decode"),
            VfsReply::ReadLen(0)
        );
    }

    #[test]
    fn initramfs_write_rejection_roundtrips_over_kernel_ipc() {
        run_with_large_stack(|| {
            with_kernel_roundtrip(
                InitramfsBackend::new(4096),
                |kernel, initramfs, client_send, server_recv, server_send, client_recv| {
                    let open = roundtrip_ipc(
                        kernel,
                        initramfs,
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
                        kernel,
                        initramfs,
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
                },
            );
        });
    }

    #[test]
    fn backend_semantics_matrix_roundtrips_over_kernel_ipc() {
        run_with_large_stack(|| {
            // DevFS: null reads as 0 and console writes echo length.
            with_kernel_roundtrip(DevFsBackend::default(), |kernel, devfs, client_send, server_recv, server_send, client_recv| {
                let dev_null_fd = decode_fd_reply(
                    roundtrip_ipc(
                        kernel,
                        devfs,
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
                        .expect("open null"),
                    )
                    .expect("open null reply"),
                )
                .expect("decode fd");
                let dev_null_read = roundtrip_ipc(
                    kernel,
                    devfs,
                    client_send,
                    server_recv,
                    server_send,
                    client_recv,
                    read_message(ReadWriteRequest {
                        fd: dev_null_fd,
                        buf_ptr: 0,
                        len: 128,
                    })
                    .expect("read null"),
                )
                .expect("read null reply");
                assert_eq!(
                    VfsReply::from_opcode_payload_checked(dev_null_read.opcode, dev_null_read.as_slice())
                        .expect("decode"),
                    VfsReply::ReadLen(0)
                );

                let dev_console_fd = decode_fd_reply(
                    roundtrip_ipc(
                        kernel,
                        devfs,
                        client_send,
                        server_recv,
                        server_send,
                        client_recv,
                        openat_message(OpenAtRequest {
                            dirfd: 0,
                            path_ptr: DEV_CONSOLE_PATH_PTR,
                            flags: 0,
                            mode: 0,
                        })
                        .expect("open console"),
                    )
                    .expect("open console reply"),
                )
                .expect("decode fd");
                let dev_console_write = roundtrip_ipc(
                    kernel,
                    devfs,
                    client_send,
                    server_recv,
                    server_send,
                    client_recv,
                    write_message(ReadWriteRequest {
                        fd: dev_console_fd,
                        buf_ptr: 0,
                        len: 17,
                    })
                    .expect("write console"),
                )
                .expect("write console reply");
                assert_eq!(
                    VfsReply::from_opcode_payload_checked(
                        dev_console_write.opcode,
                        dev_console_write.as_slice(),
                    )
                    .expect("decode"),
                    VfsReply::WriteLen(17)
                );
            });

            // RamFS: write then statx reflects non-zero encoded metadata.
            with_kernel_roundtrip(RamFsBackend::new(), |kernel, ramfs, client_send, server_recv, server_send, client_recv| {
                let ram_fd = decode_fd_reply(
                    roundtrip_ipc(
                        kernel,
                        ramfs,
                        client_send,
                        server_recv,
                        server_send,
                        client_recv,
                        openat_message(OpenAtRequest {
                            dirfd: 0,
                            path_ptr: 0xCAFE,
                            flags: 0,
                            mode: 0,
                        })
                        .expect("open ramfs"),
                    )
                    .expect("open ramfs reply"),
                )
                .expect("decode fd");
                let _ = roundtrip_ipc(
                    kernel,
                    ramfs,
                    client_send,
                    server_recv,
                    server_send,
                    client_recv,
                    write_message(ReadWriteRequest {
                        fd: ram_fd,
                        buf_ptr: 0,
                        len: 64,
                    })
                    .expect("write ramfs"),
                )
                .expect("write ramfs reply");
                let ram_stat = roundtrip_ipc(
                    kernel,
                    ramfs,
                    client_send,
                    server_recv,
                    server_send,
                    client_recv,
                    statx_message(StatxRequest {
                        dirfd: 0,
                        path_ptr: 0xCAFE,
                        flags: 0,
                        mask_or_buf: 0,
                    })
                    .expect("stat ramfs"),
                )
                .expect("stat ramfs reply");
                assert!(
                    VfsReply::from_opcode_payload_checked(ram_stat.opcode, ram_stat.as_slice())
                        .expect("decode")
                        .as_u64()
                        > 0
                );
            });

            // Initramfs: read succeeds (bounded by file length) and write is rejected.
            with_kernel_roundtrip(
                InitramfsBackend::new(4096),
                |kernel, initramfs, client_send, server_recv, server_send, client_recv| {
                    let init_fd = decode_fd_reply(
                        roundtrip_ipc(
                            kernel,
                            initramfs,
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
                            .expect("open init"),
                        )
                        .expect("open init reply"),
                    )
                    .expect("decode fd");
                    let init_read = roundtrip_ipc(
                        kernel,
                        initramfs,
                        client_send,
                        server_recv,
                        server_send,
                        client_recv,
                        read_message(ReadWriteRequest {
                            fd: init_fd,
                            buf_ptr: 0,
                            len: 8192,
                        })
                        .expect("read init"),
                    )
                    .expect("read init reply");
                    assert_eq!(
                        VfsReply::from_opcode_payload_checked(init_read.opcode, init_read.as_slice())
                            .expect("decode"),
                        VfsReply::ReadLen(4096)
                    );
                    let init_write = roundtrip_ipc(
                        kernel,
                        initramfs,
                        client_send,
                        server_recv,
                        server_send,
                        client_recv,
                        write_message(ReadWriteRequest {
                            fd: init_fd,
                            buf_ptr: 0,
                            len: 1,
                        })
                        .expect("write init"),
                    );
                    assert_eq!(init_write, Err(VfsError::Unsupported));
                },
            );
        });
    }
}
