// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(test, feature = "legacy-tests"))]
use yarm::kernel::boot::KernelState;
use yarm_fs_servers::common::service::FsService;
#[cfg(all(test, feature = "legacy-tests"))]
use yarm_fs_servers::common::vfs_ipc::VfsBackend;
use yarm_fs_servers::common::vfs_ipc::VfsError;
use yarm_fs_servers::common::vfs_ipc::{
    InMemoryBackend, ReadWriteRequest, close_message, dup_message, epoll_create1_message,
    epoll_ctl_message, epoll_pwait_message, fcntl_message, ioctl_message, openat_inline_message,
    poll_message, read_message, sendfile_message, statx_inline_message, write_message,
};
use yarm_srv_common::service_loop::run_typed_request_loop;
use yarm_srv_common::vfs_reply::VfsReply;
#[cfg(all(test, feature = "legacy-tests"))]
use yarm_user_rt::capability::CapId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsLoopSummary {
    pub fd: u64,
    pub dup_fd: u64,
    pub epoll_fd: u64,
    pub handled: usize,
}

#[cfg(all(test, feature = "legacy-tests"))]
const VFS_ROUNDTRIP_RECV_TIMEOUT_TICKS: u64 = 1;

#[cfg(all(test, feature = "legacy-tests"))]
fn path_bytes_from_id(path_id: u64) -> Option<&'static [u8]> {
    use yarm_fs_servers::devfs::{DEV_CONSOLE_PATH, DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH, DEV_NULL_PATH_PTR};
    use yarm_fs_servers::initramfs::{INITRAMFS_BOOT_MARKER_PATH, INITRAMFS_BOOT_MARKER_PATH_PTR};
    match path_id {
        DEV_NULL_PATH_PTR => Some(DEV_NULL_PATH),
        DEV_CONSOLE_PATH_PTR => Some(DEV_CONSOLE_PATH),
        INITRAMFS_BOOT_MARKER_PATH_PTR => Some(INITRAMFS_BOOT_MARKER_PATH),
        _ => None,
    }
}

fn decode_fd_reply(reply: yarm_user_rt::ipc::Message) -> Result<u64, VfsError> {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .map_err(|_| VfsError::Malformed)?
        .expect_fd(reply.opcode)
        .map_err(|_| VfsError::Malformed)
}

pub fn run_request_loop(
    vfs: &mut FsService<InMemoryBackend>,
    path: &[u8],
) -> Result<VfsLoopSummary, VfsError> {
    let reply = run_typed_request_loop(
        vfs,
        [openat_inline_message(0, path, 0, 0).map_err(|_| VfsError::Malformed)?],
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
            statx_inline_message(0, path, 0, 0).map_err(|_| VfsError::Malformed)?,
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

#[cfg(all(test, feature = "legacy-tests"))]
fn map_kernel_ipc_err<T>(
    result: Result<T, yarm::kernel::boot::KernelError>,
) -> Result<T, VfsError> {
    result.map_err(|_| VfsError::Unsupported)
}

#[cfg(all(test, feature = "legacy-tests"))]
fn map_kernel_ipc_error(_: yarm::kernel::boot::KernelError) -> VfsError {
    VfsError::Unsupported
}

#[cfg(all(test, feature = "legacy-tests"))]
fn roundtrip_ipc<B: VfsBackend>(
    runtime: &mut impl VfsKernelIpcRuntime,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    server_send_cap: CapId,
    client_recv_cap: CapId,
    request: yarm_user_rt::ipc::Message,
) -> Result<yarm_user_rt::ipc::Message, VfsError> {
    runtime.synthetic_roundtrip_call_reply_with_budget(
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        request,
        VFS_ROUNDTRIP_RECV_TIMEOUT_TICKS,
    )
}

#[cfg(all(test, feature = "legacy-tests"))]
pub trait VfsKernelIpcRuntime {
    fn create_endpoint(&mut self, depth: usize) -> Result<(usize, CapId, CapId), VfsError>;

    fn synthetic_roundtrip_call_reply_with_budget<B: VfsBackend>(
        &mut self,
        vfs: &mut FsService<B>,
        client_send_cap: CapId,
        server_recv_cap: CapId,
        _server_send_cap: CapId,
        client_recv_cap: CapId,
        request: yarm_user_rt::ipc::Message,
        recv_timeout_ticks: u64,
    ) -> Result<yarm_user_rt::ipc::Message, VfsError>;
}

#[cfg(all(test, feature = "legacy-tests"))]
impl VfsKernelIpcRuntime for KernelState {
    fn create_endpoint(&mut self, depth: usize) -> Result<(usize, CapId, CapId), VfsError> {
        map_kernel_ipc_err(self.create_endpoint(depth))
    }

    fn synthetic_roundtrip_call_reply_with_budget<B: VfsBackend>(
        &mut self,
        vfs: &mut FsService<B>,
        client_send_cap: CapId,
        server_recv_cap: CapId,
        _server_send_cap: CapId,
        client_recv_cap: CapId,
        request: yarm_user_rt::ipc::Message,
        recv_timeout_ticks: u64,
    ) -> Result<yarm_user_rt::ipc::Message, VfsError> {
        super::super::ipc_roundtrip::synthetic_roundtrip_call_reply_with_budget(
            self,
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
}

#[allow(dead_code)]
#[cfg(all(test, feature = "legacy-tests"))]
fn roundtrip_ipc_with_budget<B: VfsBackend>(
    runtime: &mut impl VfsKernelIpcRuntime,
    vfs: &mut FsService<B>,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    server_send_cap: CapId,
    client_recv_cap: CapId,
    request: yarm_user_rt::ipc::Message,
    recv_timeout_ticks: u64,
) -> Result<yarm_user_rt::ipc::Message, VfsError> {
    runtime.synthetic_roundtrip_call_reply_with_budget(
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        request,
        recv_timeout_ticks,
    )
}

#[cfg(all(test, feature = "legacy-tests"))]
pub fn run_request_loop_over_kernel_ipc(
    runtime: &mut impl VfsKernelIpcRuntime,
    vfs: &mut FsService<impl VfsBackend>,
    path_id: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let path = path_bytes_from_id(path_id).ok_or(VfsError::InvalidPath)?;
    let (_, client_send_cap, server_recv_cap) = runtime.create_endpoint(16)?;
    let (_, server_send_cap, client_recv_cap) = runtime.create_endpoint(16)?;

    let open_reply = roundtrip_ipc(
        runtime,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        openat_inline_message(0, path, 0, 0)
        .map_err(|_| VfsError::Malformed)?,
    )?;
    let fd = decode_fd_reply(open_reply)?;

    let dup_fd = decode_fd_reply(roundtrip_ipc(
        runtime,
        vfs,
        client_send_cap,
        server_recv_cap,
        server_send_cap,
        client_recv_cap,
        dup_message(fd).map_err(|_| VfsError::Malformed)?,
    )?)?;

    let epoll_fd = decode_fd_reply(roundtrip_ipc(
        runtime,
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
        statx_inline_message(0, path, 0, 0)
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
    ];
    for request in requests {
        let _ = roundtrip_ipc(
            runtime,
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

#[cfg(all(test, feature = "legacy-tests"))]
pub fn run_with_kernel_ipc(
    runtime: &mut impl VfsKernelIpcRuntime,
    path_id: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    run_request_loop_over_kernel_ipc(runtime, &mut vfs, path_id)
}

fn vfs_error_reply(status: u32) -> yarm_user_rt::ipc::Message {
    yarm_user_rt::ipc::Message::new(1u64, &status.to_le_bytes())
        .unwrap_or_else(|_| yarm_user_rt::ipc::Message::new(1u64, &[]).expect("err msg"))
}

#[cfg(test)]
fn route_for_test(
    mount_table: &super::mount_table::VfsMountTable,
    fd_table: &super::fd_table::VfsFdTable,
    msg: &yarm_user_rt::ipc::Message,
    client_id: u64,
) -> Result<(u32, super::mount_table::MountLabel), u32> {
    use yarm_ipc_abi::vfs_abi::{
        OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ,
        VFS_OP_STATX, VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_CODEC, VFS_STATUS_ERR_INVALID_PATH,
        VFS_STATUS_ERR_NO_MOUNT, VFS_STATUS_ERR_UNKNOWN_OP,
    };
    match msg.opcode {
        VFS_OP_STATX | VFS_OP_OPENAT => {
            let raw_path: Option<&[u8]> = if msg.opcode == VFS_OP_STATX {
                StatxInlinePath::decode(msg.as_slice()).map(|s| s.path)
            } else {
                OpenAtInlinePath::decode(msg.as_slice()).map(|s| s.path)
            };
            let raw_path = raw_path.ok_or(VFS_STATUS_ERR_INVALID_PATH)?;
            let norm = super::path::normalize(raw_path).map_err(|_| VFS_STATUS_ERR_INVALID_PATH)?;
            let path = norm.as_bytes();
            mount_table.route(path).ok_or(VFS_STATUS_ERR_NO_MOUNT)
        }
        VFS_OP_READ | VFS_OP_CLOSE => {
            let args = ReadWriteArgs::decode(msg.as_slice()).map_err(|_| VFS_STATUS_ERR_CODEC)?;
            fd_table.lookup(args.fd, client_id).ok_or(VFS_STATUS_ERR_BAD_FD)
        }
        _ => Err(VFS_STATUS_ERR_UNKNOWN_OP),
    }
}

#[cfg(test)]
mod service_level_tests {
    use super::*;
    use yarm_fs_servers::common::vfs_ipc::{
        close_message, openat_inline_message, read_message, CloseRequest, ReadWriteRequest,
    };
    use yarm_ipc_abi::vfs_abi::{VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_NO_MOUNT};

    #[test]
    fn vfs_service_open_routes_normalized_paths_to_expected_mount() {
        let mut mounts = super::super::mount_table::VfsMountTable::new();
        assert!(mounts.register(b"/", "root", 1));
        assert!(mounts.register(b"/dev/", "devfs", 2));
        let fds = super::super::fd_table::VfsFdTable::new();

        let m = openat_inline_message(0, b"//dev///null", 0, 0).expect("msg");
        let (cap, _) = route_for_test(&mounts, &fds, &m, 10).expect("route");
        assert_eq!(cap, 2);

        let m = openat_inline_message(0, b"/dev", 0, 0).expect("msg");
        let (cap, _) = route_for_test(&mounts, &fds, &m, 10).expect("route");
        assert_eq!(cap, 2);

        let m = openat_inline_message(0, b"/sbin/init", 0, 0).expect("msg");
        let (cap, _) = route_for_test(&mounts, &fds, &m, 10).expect("route");
        assert_eq!(cap, 1);

        let m = openat_inline_message(0, b"/device", 0, 0).expect("msg");
        let (cap, _) = route_for_test(&mounts, &fds, &m, 10).expect("route");
        assert_eq!(cap, 1); // root mount catches non-/dev paths.

        let mut mounts2 = super::super::mount_table::VfsMountTable::new();
        assert!(mounts2.register(b"/dev/", "devfs", 2));
        let m = openat_inline_message(0, b"/sbin/init", 0, 0).expect("msg");
        assert!(matches!(
            route_for_test(&mounts2, &fds, &m, 10),
            Err(VFS_STATUS_ERR_NO_MOUNT)
        ));
    }

    #[test]
    fn vfs_service_rejects_cross_client_fd_read_and_close() {
        let mounts = super::super::mount_table::VfsMountTable::new();
        let mut fds = super::super::fd_table::VfsFdTable::new();
        assert!(fds.insert(7, 11, "devfs", 100));
        let read = read_message(ReadWriteRequest { fd: 7, buf_ptr: 0, len: 1 }).expect("read");
        let close = close_message(CloseRequest { fd: 7 }).expect("close");
        assert!(matches!(
            route_for_test(&mounts, &fds, &read, 200),
            Err(VFS_STATUS_ERR_BAD_FD)
        ));
        assert!(matches!(
            route_for_test(&mounts, &fds, &close, 200),
            Err(VFS_STATUS_ERR_BAD_FD)
        ));
        assert!(route_for_test(&mounts, &fds, &read, 100).is_ok());
    }

    #[test]
    fn vfs_service_rejects_read_after_close_and_double_close() {
        let mounts = super::super::mount_table::VfsMountTable::new();
        let mut fds = super::super::fd_table::VfsFdTable::new();
        assert!(fds.insert(9, 12, "root", 300));
        fds.remove(9, 300);
        let read = read_message(ReadWriteRequest { fd: 9, buf_ptr: 0, len: 1 }).expect("read");
        let close = close_message(CloseRequest { fd: 9 }).expect("close");
        assert!(matches!(
            route_for_test(&mounts, &fds, &read, 300),
            Err(VFS_STATUS_ERR_BAD_FD)
        ));
        assert!(matches!(
            route_for_test(&mounts, &fds, &close, 300),
            Err(VFS_STATUS_ERR_BAD_FD)
        ));
    }

    #[test]
    fn vfs_service_fd_reuse_does_not_cross_client_leak() {
        let mounts = super::super::mount_table::VfsMountTable::new();
        let mut fds = super::super::fd_table::VfsFdTable::new();
        assert!(fds.insert(3, 10, "initramfs", 1));
        fds.remove(3, 1);
        assert!(fds.insert(3, 20, "devfs", 2));
        let read = read_message(ReadWriteRequest { fd: 3, buf_ptr: 0, len: 1 }).expect("read");
        assert!(matches!(
            route_for_test(&mounts, &fds, &read, 1),
            Err(VFS_STATUS_ERR_BAD_FD)
        ));
        assert!(route_for_test(&mounts, &fds, &read, 2).is_ok());
    }
}

pub fn run() {
    yarm_user_rt::user_log!("VFS_SRV_ENTRY");
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let summary = run_request_loop(&mut vfs, b"/control-plane/vfs-probe").expect("vfs loop");

    yarm_user_rt::user_log!(
        "vfs request-loop ready: fd={}, dup_fd={}, epoll_fd={}, handled={}",
        summary.fd,
        summary.dup_fd,
        summary.epoll_fd,
        summary.handled
    );

    yarm_user_rt::user_log!("VFS_SRV_RESIDENT_WAIT_BEGIN");
    let ctx = yarm_user_rt::runtime::startup_context();
    let initramfs_send = ctx.service_extra_cap_0.unwrap_or(0);
    let devfs_send = ctx.service_extra_cap_1.unwrap_or(0);
    yarm_user_rt::user_log!(
        "VFS_ROUTE_INIT initramfs_send={} devfs_send={}",
        initramfs_send, devfs_send
    );

    let mut mount_table = super::mount_table::VfsMountTable::new();
    if initramfs_send != 0 {
        mount_table.register(b"/initramfs/", "initramfs", initramfs_send);
    }
    if devfs_send != 0 {
        mount_table.register(b"/dev/", "devfs", devfs_send);
    }
    yarm_user_rt::user_log!("VFS_MOUNT_TABLE_READY entries={}", mount_table.len());

    let mut fd_table = super::fd_table::VfsFdTable::new();

    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("VFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("VFS_SRV_RECV_CAP cap={}", recv_cap);
    yarm_user_rt::user_log!("VFS_SRV_BLOCKING_RECV_LOOP");

    loop {
        // Receive client request; recv_v2 opcode/payload are sourced from kernel
        // out-meta decode (no userspace raw-lane/opcode-prefix heuristics).
        let (msg, client_reply_cap) =
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let Some(reply_cap) = received.reply_cap else { continue; };
                    (received.message, reply_cap)
                },
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                    continue;
                }
            };

        yarm_user_rt::user_log!(
            "VFS_RECV_GOT_MSG opcode={} reply_cap={}",
            msg.opcode, client_reply_cap
        );
        let client_id = msg.sender_tid.0;

        // ── VFS_OP_MOUNT_REGISTER — handled locally, never forwarded ────────
        if msg.opcode == yarm_ipc_abi::vfs_abi::VFS_OP_MOUNT_REGISTER {
            use yarm_ipc_abi::vfs_abi::{
                MountRegisterArgs, VFS_MOUNT_STATUS_ERR_DUPLICATE, VFS_MOUNT_STATUS_ERR_FULL,
                VFS_MOUNT_STATUS_ERR_INVALID_CAP, VFS_MOUNT_STATUS_ERR_INVALID_PREFIX,
                VFS_MOUNT_STATUS_OK,
            };
            use super::mount_table::MountRegisterError;

            let status: u32 = match MountRegisterArgs::decode(msg.as_slice()) {
                None => {
                    yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_DECODE_ERR");
                    VFS_MOUNT_STATUS_ERR_INVALID_PREFIX
                }
                Some(args) => {
                    yarm_user_rt::user_log!(
                        "VFS_MOUNT_REGISTER_REQUEST cap={} flags={}",
                        args.backend_send_cap, args.flags
                    );
                    let cap32 = args.backend_send_cap as u32;
                    match mount_table.insert_dynamic(args.prefix, cap32, args.flags as u32) {
                        Ok(()) => {
                            yarm_user_rt::user_log!(
                                "VFS_MOUNT_REGISTER_OK entries={}",
                                mount_table.len()
                            );
                            VFS_MOUNT_STATUS_OK
                        }
                        Err(MountRegisterError::DuplicatePrefix) => {
                            yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_ERR reason=duplicate");
                            VFS_MOUNT_STATUS_ERR_DUPLICATE
                        }
                        Err(MountRegisterError::TableFull) => {
                            yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_ERR reason=full");
                            VFS_MOUNT_STATUS_ERR_FULL
                        }
                        Err(MountRegisterError::InvalidSendCap) => {
                            yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_ERR reason=invalid-cap");
                            VFS_MOUNT_STATUS_ERR_INVALID_CAP
                        }
                        Err(MountRegisterError::PrefixTooLong)
                        | Err(MountRegisterError::InvalidPrefix) => {
                            yarm_user_rt::user_log!(
                                "VFS_MOUNT_REGISTER_ERR reason=invalid-prefix"
                            );
                            VFS_MOUNT_STATUS_ERR_INVALID_PREFIX
                        }
                    }
                }
            };
            let reply_opcode: u64 = if status == VFS_MOUNT_STATUS_OK { 0 } else { 1 };
            let reply = yarm_user_rt::ipc::Message::new(reply_opcode, &status.to_le_bytes())
                .unwrap_or_else(|_| yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"));
            let _ = unsafe { yarm_user_rt::syscall::ipc_reply(client_reply_cap, &reply) };
            continue;
        }

        // ── Route by path prefix (STATX/OPENAT) or fd table (READ/CLOSE) ───
        // Path-based ops are normalized before mount lookup.
        // `route()` and `lookup()` return owned `MountLabel` copies so that
        // callers may freely mutate the tables afterward (e.g. fd_table.remove).
        // Error variants carry a canonical VFS_STATUS_ERR_* code.
        let route: Result<(u32, super::mount_table::MountLabel), u32> = 'route: {
            use yarm_ipc_abi::vfs_abi::{
                OpenAtInlinePath, ReadWriteArgs, StatxInlinePath,
                VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX,
                VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_CODEC,
                VFS_STATUS_ERR_INVALID_PATH, VFS_STATUS_ERR_NO_MOUNT,
                VFS_STATUS_ERR_UNKNOWN_OP,
            };
            match msg.opcode {
                VFS_OP_STATX | VFS_OP_OPENAT => {
                    let raw_path: Option<&[u8]> = if msg.opcode == VFS_OP_STATX {
                        StatxInlinePath::decode(msg.as_slice()).map(|s| s.path)
                    } else {
                        OpenAtInlinePath::decode(msg.as_slice()).map(|s| s.path)
                    };
                    let raw_path = match raw_path {
                        Some(p) => p,
                        None => break 'route Err(VFS_STATUS_ERR_INVALID_PATH),
                    };
                    let norm = match super::path::normalize(raw_path) {
                        Ok(n) => n,
                        Err(e) => {
                            yarm_user_rt::user_log!(
                                "VFS_PATH_NORM_REJECT reason={}", e.as_str()
                            );
                            break 'route Err(VFS_STATUS_ERR_INVALID_PATH);
                        }
                    };
                    let path = norm.as_bytes();
                    let path_str = core::str::from_utf8(path).unwrap_or("?");
                    if let Some((send_cap, label)) = mount_table.route(path) {
                        yarm_user_rt::user_log!(
                            "VFS_ROUTE_LOOKUP path={} target={}",
                            path_str, label.as_str()
                        );
                        Ok((send_cap, label))
                    } else {
                        yarm_user_rt::user_log!("VFS_ROUTE_NO_MOUNT path={}", path_str);
                        Err(VFS_STATUS_ERR_NO_MOUNT)
                    }
                }
                VFS_OP_READ | VFS_OP_CLOSE => {
                    match ReadWriteArgs::decode(msg.as_slice()) {
                        Ok(args) => {
                            if let Some((send_cap, label)) = fd_table.lookup(args.fd, client_id)
                            {
                                yarm_user_rt::user_log!(
                                    "VFS_ROUTE_FD_LOOKUP fd={} target={}",
                                    args.fd, label.as_str()
                                );
                                Ok((send_cap, label))
                            } else {
                                yarm_user_rt::user_log!("VFS_ROUTE_BAD_FD fd={}", args.fd);
                                Err(VFS_STATUS_ERR_BAD_FD)
                            }
                        }
                        Err(_) => {
                            yarm_user_rt::user_log!("VFS_ROUTE_CODEC_ERR");
                            Err(VFS_STATUS_ERR_CODEC)
                        }
                    }
                }
                op => {
                    yarm_user_rt::user_log!("VFS_ROUTE_UNKNOWN_OP op={}", op);
                    Err(VFS_STATUS_ERR_UNKNOWN_OP)
                }
            }
        };

        let (backend_send_cap, target_label) = match route {
            Ok(pair) => pair,
            Err(status) => {
                yarm_user_rt::user_log!("VFS_ROUTE_ERR status={}", status);
                let err = vfs_error_reply(status);
                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(client_reply_cap, &err) };
                continue;
            }
        };

        yarm_user_rt::user_log!(
            "VFS_ROUTE_FORWARD target={} send_cap={}",
            target_label.as_str(), backend_send_cap
        );

        // Forward request to backend; pass our own recv_cap as the reply endpoint so
        // the backend's ipc_reply delivers the response back to this endpoint.
        let _ = unsafe { yarm_user_rt::syscall::ipc_call(backend_send_cap, recv_cap, &msg) };

        // Wait for backend reply (satisfies phase-6 timed-receive guardrail).
        let backend_reply =
            unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(recv_cap, 0) };

        match backend_reply {
            Ok(Some(ref response)) => {
                use yarm_ipc_abi::vfs_abi::{ReadWriteArgs, VFS_OP_CLOSE, VFS_OP_OPENAT};
                if msg.opcode == VFS_OP_OPENAT {
                    let payload = response.as_slice();
                    if payload.len() >= 8 {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(&payload[..8]);
                        let fd = u64::from_le_bytes(b);
                        // target_label is owned (Copy), no borrow conflict.
                        if fd_table.insert(fd, backend_send_cap, target_label.as_str(), client_id) {
                            yarm_user_rt::user_log!(
                                "VFS_FD_TRACK fd={} backend={}",
                                fd, backend_send_cap
                            );
                        }
                    }
                } else if msg.opcode == VFS_OP_CLOSE {
                    // Extract status from reply; remove fd from table on success.
                    let status = {
                        let payload = response.as_slice();
                        if payload.len() >= 8 {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(&payload[..8]);
                            u64::from_le_bytes(b)
                        } else {
                            1
                        }
                    };
                    let closed_fd = ReadWriteArgs::decode(msg.as_slice())
                        .map(|a| a.fd)
                        .unwrap_or(u64::MAX);
                    // Log before remove; target_label is owned so no borrow conflict.
                    yarm_user_rt::user_log!(
                        "VFS_FD_CLOSE fd={} target={} status={}",
                        closed_fd, target_label.as_str(), status
                    );
                    if status == 0 {
                        fd_table.remove(closed_fd, client_id);
                    }
                }
                yarm_user_rt::user_log!("VFS_ROUTE_REPLY status=0");
                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(client_reply_cap, response) };
            }
            _ => {
                yarm_user_rt::user_log!("VFS_ROUTE_REPLY status=1");
                let err = vfs_error_reply(yarm_ipc_abi::vfs_abi::VFS_STATUS_ERR_BACKEND);
                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(client_reply_cap, &err) };
            }
        }
    }
}

#[cfg(all(test, feature = "legacy-tests"))]
mod tests {
    use super::*;
    use yarm::kernel::boot::Bootstrap;
    use yarm::std::thread;
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
            openat_inline_message(0, path_bytes_from_id(0x4444).unwrap_or(b"/invalid"), 0, 0)
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
            openat_inline_message(0, path_bytes_from_id(DEV_NULL_PATH_PTR).unwrap_or(b"/invalid"), 0, 0)
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
            openat_inline_message(0, path_bytes_from_id(0xABCD).unwrap_or(b"/invalid"), 0, 0)
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
                        openat_inline_message(0, path_bytes_from_id(INITRAMFS_BOOT_MARKER_PATH_PTR).unwrap_or(b"/invalid"), 0, 0)
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
            with_kernel_roundtrip(
                DevFsBackend::default(),
                |kernel, devfs, client_send, server_recv, server_send, client_recv| {
                    let dev_null_fd = decode_fd_reply(
                        roundtrip_ipc(
                            kernel,
                            devfs,
                            client_send,
                            server_recv,
                            server_send,
                            client_recv,
                            openat_inline_message(0, path_bytes_from_id(DEV_NULL_PATH_PTR).unwrap_or(b"/invalid"), 0, 0)
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
                        VfsReply::from_opcode_payload_checked(
                            dev_null_read.opcode,
                            dev_null_read.as_slice()
                        )
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
                            openat_inline_message(0, path_bytes_from_id(DEV_CONSOLE_PATH_PTR).unwrap_or(b"/invalid"), 0, 0)
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
                },
            );

            // RamFS: write then statx reflects non-zero encoded metadata.
            with_kernel_roundtrip(
                RamFsBackend::new(),
                |kernel, ramfs, client_send, server_recv, server_send, client_recv| {
                    let ram_fd = decode_fd_reply(
                        roundtrip_ipc(
                            kernel,
                            ramfs,
                            client_send,
                            server_recv,
                            server_send,
                            client_recv,
                            openat_inline_message(0, path_bytes_from_id(0xCAFE).unwrap_or(b"/invalid"), 0, 0)
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
                        statx_inline_message(0, path_bytes_from_id(0xCAFE).unwrap_or(b"/invalid"), 0, 0)
                        .expect("stat ramfs"),
                    )
                    .expect("stat ramfs reply");
                    assert!(
                        VfsReply::from_opcode_payload_checked(ram_stat.opcode, ram_stat.as_slice())
                            .expect("decode")
                            .as_u64()
                            > 0
                    );
                },
            );

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
                            openat_inline_message(0, path_bytes_from_id(INITRAMFS_BOOT_MARKER_PATH_PTR).unwrap_or(b"/invalid"), 0, 0)
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
                        VfsReply::from_opcode_payload_checked(
                            init_read.opcode,
                            init_read.as_slice()
                        )
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
