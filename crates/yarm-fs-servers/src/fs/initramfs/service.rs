// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm::kernel::ipc::Message;
use yarm::kernel::vfs::VfsError;
use yarm::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, openat_message, read_message, statx_message, write_message,
};
use yarm::services::common::service::{FsService, run_typed_request_loop};
use crate::fs::initramfs::archive::{
    INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend, InitramfsMetrics,
};
use yarm_srv_common::vfs_reply::VfsReply;

pub type InitramfsService = FsService<InitramfsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitramfsLoopSummary {
    pub fd: u64,
    pub read_len: u64,
    pub statx_value: u64,
    pub write_allowed: bool,
    pub handled: usize,
    pub metrics: InitramfsMetrics,
}

fn decode_reply_u64(reply: Message) -> u64 {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .expect("decode vfs reply")
        .as_u64()
}

fn scripted_bootstrap_requests() -> Result<[Message; 1], VfsError> {
    Ok([openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
        flags: 0,
        mode: 0,
    })?])
}

fn scripted_bootstrap_io(fd: u64) -> Result<[Message; 2], VfsError> {
    Ok([
        read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 512,
        })?,
        statx_message(yarm::kernel::vfs::StatxRequest {
            dirfd: 0,
            path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
            flags: 0,
            mask_or_buf: 0,
        })?,
    ])
}

pub fn run_request_batch<const N: usize>(
    service: &mut InitramfsService,
    requests: [Message; N],
) -> Result<[Message; N], VfsError> {
    run_typed_request_loop(service, requests)
}

pub fn run_request_loop(service: &mut InitramfsService) -> Result<InitramfsLoopSummary, VfsError> {
    let open_reply = run_request_batch(service, scripted_bootstrap_requests()?)?[0];
    let fd = decode_reply_u64(open_reply);

    let io = run_request_batch(service, scripted_bootstrap_io(fd)?)?;
    let read_len = decode_reply_u64(io[0]);
    let statx_value = decode_reply_u64(io[1]);

    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 1,
    })?;
    let write_allowed = service.handle(write).is_ok();

    Ok(InitramfsLoopSummary {
        fd,
        read_len,
        statx_value,
        write_allowed,
        handled: service.handled_count(),
        metrics: service.backend().metrics(),
    })
}

pub fn run() {
    let mut svc = InitramfsService::with_backend(InitramfsBackend::new(8192));
    let summary = run_request_loop(&mut svc).expect("initramfs loop");

    yarm::yarm_log!(
        "initramfs.srv request-loop ready: fd={}, read_len={}, statx={}, write_allowed={}, handled={}, opens={}, reads={}, statx_calls={}, errors={}",
        summary.fd,
        summary.read_len,
        summary.statx_value,
        summary.write_allowed,
        summary.handled,
        summary.metrics.open_count,
        summary.metrics.read_count,
        summary.metrics.statx_count,
        summary.metrics.error_count
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::kernel::vfs::{
        CloseRequest, MountNamespacePolicy, MountRouter, StatxRequest, close_message,
        openat_message, read_message, statx_message,
    };
    use yarm::services::common::vfs_service::VfsService;
    use crate::fs::devfs::{DEV_CONSOLE_PATH_PTR, DevFsBackend};
    use crate::fs::initramfs::INITRAMFS_INIT_PATH_PTR;
    use yarm_ipc_abi::vfs_abi::{OpenAtArgs, ReadWriteArgs, StatxArgs, VFS_OP_OPENAT, VFS_OP_READ};

    #[test]
    fn initramfs_is_read_only_with_metrics() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let summary = run_request_loop(&mut svc).expect("loop");
        assert!(!summary.write_allowed);
        assert_eq!(summary.fd, 10);
        assert_eq!(summary.read_len, 512);
        assert_eq!(summary.handled, 3);
        assert_eq!(summary.metrics.open_count, 1);
        assert_eq!(summary.metrics.read_count, 1);
        assert_eq!(summary.metrics.statx_count, 1);
    }

    #[test]
    fn initramfs_protocol_vectors_match_frozen_vfs_codec() {
        let open = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .expect("open");
        assert_eq!(open.opcode, VFS_OP_OPENAT);
        assert_eq!(
            open.as_slice(),
            &OpenAtArgs::new(0, INITRAMFS_BOOT_MARKER_PATH_PTR, 0, 0).encode()
        );

        let read = read_message(ReadWriteRequest {
            fd: 10,
            buf_ptr: 0,
            len: 32,
        })
        .expect("read");
        assert_eq!(read.opcode, VFS_OP_READ);
        assert_eq!(read.as_slice(), &ReadWriteArgs::new(10, 0, 32).encode());

        let statx = statx_message(StatxRequest {
            dirfd: 0,
            path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
            flags: 0,
            mask_or_buf: 0,
        })
        .expect("statx");
        assert_eq!(
            statx.as_slice(),
            &StatxArgs::new(0, INITRAMFS_BOOT_MARKER_PATH_PTR, 0, 0).encode()
        );
    }

    #[test]
    fn initramfs_mount_gate_routes_with_policy_denial() {
        let router = MountRouter::new(
            0x4800_0000_0000_0000,
            DevFsBackend::default(),
            InitramfsBackend::new(4096),
        );
        let mut svc = VfsService::with_backend(router);
        svc.mount(DEV_CONSOLE_PATH_PTR, 1).expect("mount devfs");
        svc.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2)
            .expect("mount initramfs");
        svc.set_policy(
            MountNamespacePolicy::deny_all()
                .with_range(DEV_CONSOLE_PATH_PTR, DEV_CONSOLE_PATH_PTR)
                .with_range(
                    INITRAMFS_BOOT_MARKER_PATH_PTR,
                    INITRAMFS_BOOT_MARKER_PATH_PTR,
                ),
        );

        let open_init = svc
            .handle_request(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
            )
            .expect("initramfs open");
        assert_eq!(open_init.opcode, VFS_OP_OPENAT);

        let denied = svc.handle_request(
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: INITRAMFS_INIT_PATH_PTR,
                flags: 0,
                mode: 0,
            })
            .expect("open"),
        );
        assert_eq!(denied, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn initramfs_lifecycle_gate_covers_mount_failure_recovery_and_close() {
        let mut svc = VfsService::with_backend(InitramfsBackend::new(4096));
        svc.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2).expect("mount");

        let open = svc
            .handle_request(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
            )
            .expect("open reply");
        let fd = decode_reply_u64(open);

        svc.mark_mount_failed(INITRAMFS_BOOT_MARKER_PATH_PTR)
            .expect("mark failed");
        svc.recover_mount(INITRAMFS_BOOT_MARKER_PATH_PTR)
            .expect("recover");

        let _ = svc
            .handle_request(close_message(CloseRequest { fd }).expect("close req"))
            .expect("close");
        assert_eq!(
            svc.handle_request(
                read_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 1,
                })
                .expect("read")
            ),
            Err(VfsError::BadFd)
        );

        svc.unmount(INITRAMFS_BOOT_MARKER_PATH_PTR)
            .expect("unmount");
        assert_eq!(svc.active_mounts(), 0);
    }

    #[test]
    fn initramfs_inflight_fd_survives_mount_failure_until_close() {
        let mut svc = VfsService::with_backend(InitramfsBackend::new(4096));
        svc.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2).expect("mount");
        let open = svc
            .handle_request(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
            )
            .expect("open reply");
        let fd = decode_reply_u64(open);

        svc.mark_mount_failed(INITRAMFS_BOOT_MARKER_PATH_PTR)
            .expect("mark failed");
        let read_failed = svc
            .handle_request(
                read_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 128,
                })
                .expect("read"),
            )
            .expect("read while failed");
        assert_eq!(decode_reply_u64(read_failed), 128);

        svc.recover_mount(INITRAMFS_BOOT_MARKER_PATH_PTR)
            .expect("recover");
        let _ = svc
            .handle_request(close_message(CloseRequest { fd }).expect("close req"))
            .expect("close");
        assert_eq!(
            svc.handle_request(
                read_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 1,
                })
                .expect("read")
            ),
            Err(VfsError::BadFd)
        );
    }
}
