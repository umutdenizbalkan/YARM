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
    use yarm_fs_servers::devfs::{
        DEV_CONSOLE_PATH, DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH, DEV_NULL_PATH_PTR,
    };
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
        openat_inline_message(0, path, 0, 0).map_err(|_| VfsError::Malformed)?,
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

/// VFS-CORRECT1B — the 4-byte mount-administration status reply. It carries no capability: a
/// refused commit's capability stays in VFS's retained record (a transfer would only add a copy at
/// the caller; VFS's own would remain).
fn mount_admin_reply(reply_cap: u64, status: u32) {
    let Ok(reply) = yarm_user_rt::ipc::Message::with_header(0, 0, 0, None, &status.to_le_bytes())
    else {
        return;
    };
    // SAFETY: the one-shot reply capability of the request being answered.
    if let Err(err) = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) } {
        yarm_user_rt::user_log!("VFS_MOUNT_ADMIN_REPLY_FAIL status={} err={:?}", status, err);
    }
}

/// VFS-CORRECT1B — move every message waiting on the admin endpoint into the staged table while
/// it has room (deadline-0 receives, so this never blocks). A message that reached this endpoint
/// was sent by a holder of its SEND capability: that is the administrator's authority.
fn drain_mount_admin_endpoint(
    admin_recv: u64,
    admin: &mut yarm_fs_servers::common::mount_admin::MountAdmin,
) {
    use yarm_fs_servers::common::mount_admin::StageOutcome;
    while admin.has_room() {
        // SAFETY: VFS's own admin RECEIVE capability.
        let Ok(Some(m)) = (unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(admin_recv, 0) })
        else {
            break;
        };
        let cap = m.transferred_cap().map(|c| c.0);
        match admin.stage(m.sender_tid.0, m.opcode, cap, m.as_slice()) {
            StageOutcome::Staged(s) => yarm_user_rt::user_log!(
                "VFS_MOUNT_STAGE_RECEIVED sender_tid={} prefix={} flags={} vfs_local_cap={} sender_local_cap={}",
                s.sender_tid,
                alloc::string::String::from_utf8_lossy(s.prefix()),
                s.flags,
                s.backend_cap,
                s.sender_local_cap
            ),
            StageOutcome::Retained {
                backend_cap,
                reason,
            } => yarm_user_rt::user_log!(
                "VFS_MOUNT_STAGE_RETAINED sender_tid={} vfs_local_cap={} reason={}",
                m.sender_tid.0,
                backend_cap,
                reason
            ),
            StageOutcome::Ignored { reason } => yarm_user_rt::user_log!(
                "VFS_MOUNT_STAGE_IGNORED sender_tid={} reason={}",
                m.sender_tid.0,
                reason
            ),
        }
    }
}

/// VFS-CORRECT1B — the registration handshake: one `VFS_OP_STATX` of the prefix through the
/// staged capability, over the production client wrapper and VFS's backend reply endpoint.
struct MountHandshake {
    reply_recv_cap: u64,
}

impl yarm_fs_servers::common::mount_admin::BackendHandshake for MountHandshake {
    fn handshake(
        &mut self,
        backend_cap: u64,
        prefix: &[u8],
    ) -> Result<(), yarm_fs_servers::common::mount_admin::HandshakeFault> {
        use yarm_fs_servers::common::mount_admin::HandshakeFault;
        use yarm_user_rt::vfs_client::VfsClientError;
        // SAFETY: `backend_cap` is the received capability in VFS's CNode; the reply endpoint is
        // VFS's own. The kernel refuses the call before delivering anything if the capability is
        // stale, not an endpoint, or lacks SEND.
        match unsafe {
            yarm_user_rt::vfs_client::vfs_statx(backend_cap, self.reply_recv_cap, prefix)
        } {
            Ok(_) | Err(VfsClientError::Status(_)) => Ok(()),
            Err(VfsClientError::Send(e)) => Err(HandshakeFault::Refused(e as usize)),
            Err(_) => Err(HandshakeFault::NoValidCompletion),
        }
    }
}

/// VFS-CORRECT1C — the router's kernel: NR 6 + NR 4 to a backend on VFS's backend reply
/// endpoint, NR 7 to the client, NR 21 for the caller's session and a session's liveness.
struct KernelRouterIo {
    backend_reply_recv_cap: u64,
}

impl super::router::RouterIo for KernelRouterIo {
    fn call_backend(
        &mut self,
        backend: yarm_fs_servers::common::fd_space::BackendRef,
        label: &str,
        request: &yarm_user_rt::ipc::Message,
    ) -> super::router::CallResult {
        use super::router::CallResult;
        yarm_user_rt::user_log!(
            "VFS_FORWARD_CALL op={} target={} send_cap={} reply_cap={} instance={}",
            request.opcode,
            label,
            backend.cap,
            self.backend_reply_recv_cap,
            backend.instance
        );
        // SAFETY: VFS's own capabilities: the backend endpoint it routes to, its reply endpoint.
        if let Err(err) = unsafe {
            yarm_user_rt::syscall::ipc_call(backend.cap, self.backend_reply_recv_cap, request)
        } {
            // VFS-CORRECT1A: any NR 6 failure — `WouldBlock` included — delivered nothing.
            yarm_user_rt::user_log!("VFS_FORWARD_CALL_FAIL op={} err={:?}", request.opcode, err);
            return CallResult::NotDelivered;
        }
        // SAFETY: VFS's own backend reply endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(self.backend_reply_recv_cap) } {
            Ok(Some(received)) => {
                yarm_user_rt::user_log!(
                    "VFS_BACKEND_REPLY op={} len={} sender_tid={} transferred_cap={}",
                    request.opcode,
                    received.message.len,
                    received.sender_tid,
                    received.transferred_cap.unwrap_or(0)
                );
                CallResult::Reply {
                    message: received.message,
                    has_cap: received.transferred_cap.is_some() || received.reply_cap.is_some(),
                    transferred_cap: received.transferred_cap,
                }
            }
            Ok(None) => {
                yarm_user_rt::user_log!(
                    "VFS_BACKEND_REPLY_RECV_FAIL op={} err=would_block_or_timeout",
                    request.opcode
                );
                CallResult::NoReply
            }
            Err(err) => {
                yarm_user_rt::user_log!(
                    "VFS_BACKEND_REPLY_RECV_FAIL op={} err={:?} reply_cap={}",
                    request.opcode,
                    err,
                    self.backend_reply_recv_cap
                );
                CallResult::NoReply
            }
        }
    }

    fn reply_client(&mut self, reply_cap: u64, reply: &yarm_user_rt::ipc::Message) -> bool {
        // SAFETY: the one-shot reply capability of the request being answered.
        match unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, reply) } {
            Ok(()) => true,
            Err(err) => {
                yarm_user_rt::user_log!(
                    "VFS_CLIENT_REPLY_REFUSED reply_cap={} err={:?}",
                    reply_cap,
                    err
                );
                false
            }
        }
    }

    fn caller_session(
        &mut self,
        reply_cap: u64,
    ) -> Option<(yarm_fs_servers::common::fd_space::SessionId, u64)> {
        match yarm_user_rt::syscall::ipc_caller_session(reply_cap) {
            Ok((thread_group_id, incarnation, tid)) => Some((
                yarm_fs_servers::common::fd_space::SessionId {
                    thread_group_id,
                    incarnation,
                },
                tid,
            )),
            Err(err) => {
                yarm_user_rt::user_log!(
                    "VFS_CALLER_SESSION_FAIL reply_cap={} err={:?}",
                    reply_cap,
                    err
                );
                None
            }
        }
    }

    fn session_alive(&mut self, s: yarm_fs_servers::common::fd_space::SessionId) -> Option<bool> {
        match yarm_user_rt::syscall::ipc_session_live_threads(s.thread_group_id, s.incarnation) {
            Ok(n) => Some(n > 0),
            Err(err) => {
                yarm_user_rt::user_log!(
                    "VFS_SESSION_ALIVE_FAIL tgid={} incarnation={} err={:?}",
                    s.thread_group_id,
                    s.incarnation,
                    err
                );
                None
            }
        }
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
        initramfs_send,
        devfs_send
    );

    let mut mount_table = super::mount_table::VfsMountTable::new();
    if initramfs_send != 0 {
        mount_table.register(b"/initramfs/", "initramfs", initramfs_send);
    }
    if devfs_send != 0 {
        mount_table.register(b"/dev/", "devfs", devfs_send);
    }
    yarm_user_rt::user_log!("VFS_MOUNT_TABLE_READY entries={}", mount_table.len());

    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("VFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("VFS_SRV_RECV_CAP cap={}", recv_cap);
    let backend_reply_recv_cap = ctx.process_manager_reply_recv_cap.unwrap_or(recv_cap);
    yarm_user_rt::user_log!(
        "VFS_BACKEND_REPLY_CAP cap={} shared_with_main_recv={}",
        backend_reply_recv_cap,
        if backend_reply_recv_cap == recv_cap {
            1
        } else {
            0
        }
    );
    // VFS-CORRECT1B: the mount-admin endpoint (startup slot 17), provisioned by PM's spawn; its
    // only SEND capability went to the mount administrator.
    let admin_recv_cap = ctx.service_admin_recv_ep;
    yarm_user_rt::user_log!(
        "VFS_MOUNT_ADMIN_RECV_CAP present={} cap={}",
        admin_recv_cap.is_some() as u8,
        admin_recv_cap.unwrap_or(0)
    );
    let mut mount_admin = yarm_fs_servers::common::mount_admin::MountAdmin::new();
    let mut router = super::router::Router::new();
    #[cfg(feature = "vfs-fd-witness")]
    {
        router.fail_reply_path = Some(b"/ram/vc1c-failreply");
    }
    let mut io = KernelRouterIo {
        backend_reply_recv_cap,
    };
    yarm_user_rt::user_log!(
        "VFS_FD_SPACE_READY max_descriptors={} max_per_session={} first_fd={} fail_reply_injection={}",
        yarm_fs_servers::common::fd_space::MAX_DESCRIPTORS,
        yarm_fs_servers::common::fd_space::MAX_FDS_PER_SESSION,
        yarm_fs_servers::common::fd_space::FIRST_CLIENT_FD,
        router.fail_reply_path.is_some() as u8
    );
    yarm_user_rt::user_log!("VFS_SRV_BLOCKING_RECV_LOOP");

    loop {
        // Receive client request; recv_v2 opcode/payload are sourced from kernel
        // out-meta decode (no userspace raw-lane/opcode-prefix heuristics).
        let (msg, client_reply_cap) = match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) }
        {
            Ok(Some(received)) => {
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                (received.message, reply_cap)
            }
            _ => {
                let _ = yarm_user_rt::syscall::yield_now();
                continue;
            }
        };

        yarm_user_rt::user_log!(
            "VFS_RECV_GOT_MSG opcode={} reply_cap={}",
            msg.opcode,
            client_reply_cap
        );
        let client_id = msg.sender_tid.0;

        // ── Mount administration — handled locally, never forwarded (VFS-CORRECT1B) ────────
        //
        // The retired numeric form (`VFS_OP_MOUNT_REGISTER`, a sender-local capability number in
        // the payload) is refused outright: it carries no authority. A commit is accepted only
        // for a mount staged through the admin endpoint by the same (kernel-authenticated)
        // sender; see `yarm_fs_servers::common::mount_admin`.
        if msg.opcode == yarm_ipc_abi::vfs_abi::VFS_OP_MOUNT_REGISTER {
            yarm_user_rt::user_log!(
                "VFS_MOUNT_ADMIN_REFUSED op=register sender_tid={} status={} reason=numeric_cap_is_not_authority",
                client_id,
                yarm_ipc_abi::vfs_abi::VFS_MOUNT_STATUS_ERR_PERMISSION
            );
            mount_admin_reply(
                client_reply_cap,
                yarm_ipc_abi::vfs_abi::VFS_MOUNT_STATUS_ERR_PERMISSION,
            );
            continue;
        }
        if msg.opcode == yarm_ipc_abi::vfs_abi::VFS_OP_MOUNT_COMMIT {
            let prefix = msg.as_slice();
            let Some(admin_recv) = admin_recv_cap else {
                yarm_user_rt::user_log!(
                    "VFS_MOUNT_ADMIN_REFUSED op=commit sender_tid={} status={} reason=no_admin_endpoint",
                    client_id,
                    yarm_ipc_abi::vfs_abi::VFS_MOUNT_STATUS_ERR_PERMISSION
                );
                mount_admin_reply(
                    client_reply_cap,
                    yarm_ipc_abi::vfs_abi::VFS_MOUNT_STATUS_ERR_PERMISSION,
                );
                continue;
            };
            drain_mount_admin_endpoint(admin_recv, &mut mount_admin);
            let mut handshake = MountHandshake {
                reply_recv_cap: backend_reply_recv_cap,
            };
            let out = mount_admin.commit(client_id, prefix, &mut mount_table, &mut handshake);
            let prefix_str = alloc::string::String::from_utf8_lossy(prefix);
            match out.published_cap {
                Some(cap) => yarm_user_rt::user_log!(
                    "VFS_MOUNT_PUBLISHED prefix={} vfs_local_cap={} sender_local_cap={} entries={}",
                    prefix_str,
                    cap,
                    out.staged.map(|m| m.sender_local_cap).unwrap_or(0),
                    mount_table.len()
                ),
                None => {
                    let (retained, overflow) = mount_admin.retained();
                    yarm_user_rt::user_log!(
                        "VFS_MOUNT_ADMIN_REFUSED op=commit sender_tid={} prefix={} status={} retained_cap={} retained_total={} retained_overflow={} handshake={:?}",
                        client_id,
                        prefix_str,
                        out.status,
                        out.retained_cap.unwrap_or(0),
                        retained.len(),
                        overflow,
                        out.handshake_fault
                    );
                }
            }
            mount_admin_reply(client_reply_cap, out.status);
            continue;
        }

        // ── Everything else: the router (VFS-CORRECT1C) ───────────────────────────────────
        //
        // The caller's session comes from the kernel (NR 21), the client fd namespace is VFS's
        // own, paths are canonicalized once, and the backend sees only its own handles.
        router.handle(&mut io, &mount_table, &msg, client_reply_cap);
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
            openat_inline_message(
                0,
                path_bytes_from_id(DEV_NULL_PATH_PTR).unwrap_or(b"/invalid"),
                0,
                0,
            )
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
                        openat_inline_message(
                            0,
                            path_bytes_from_id(INITRAMFS_BOOT_MARKER_PATH_PTR)
                                .unwrap_or(b"/invalid"),
                            0,
                            0,
                        )
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
                            openat_inline_message(
                                0,
                                path_bytes_from_id(DEV_NULL_PATH_PTR).unwrap_or(b"/invalid"),
                                0,
                                0,
                            )
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
                            openat_inline_message(
                                0,
                                path_bytes_from_id(DEV_CONSOLE_PATH_PTR).unwrap_or(b"/invalid"),
                                0,
                                0,
                            )
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
                            openat_inline_message(
                                0,
                                path_bytes_from_id(0xCAFE).unwrap_or(b"/invalid"),
                                0,
                                0,
                            )
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
                        statx_inline_message(
                            0,
                            path_bytes_from_id(0xCAFE).unwrap_or(b"/invalid"),
                            0,
                            0,
                        )
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
                            openat_inline_message(
                                0,
                                path_bytes_from_id(INITRAMFS_BOOT_MARKER_PATH_PTR)
                                    .unwrap_or(b"/invalid"),
                                0,
                                0,
                            )
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
