// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::super::common::vfs_ipc::VfsError;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, read_message, statx_inline_message, write_message,
};
use super::super::common::service::FsService;
use yarm_srv_common::service_loop::run_typed_request_loop;
use super::tree::{RAMFS_BOOT_PATH, RamFsBackend, RamFsMetrics};
use yarm_srv_common::vfs_reply::VfsReply;

pub type RamFsService = FsService<RamFsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamFsLoopSummary {
    pub fd: u64,
    pub read_len: u64,
    pub statx_value: u64,
    pub handled: usize,
    pub metrics: RamFsMetrics,
}

fn decode_reply_u64(reply: Message) -> u64 {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .expect("decode vfs reply")
        .as_u64()
}

fn scripted_bootstrap_requests() -> Result<[Message; 1], VfsError> {
    Ok([openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0)?])
}

fn scripted_bootstrap_io(fd: u64) -> Result<[Message; 3], VfsError> {
    Ok([
        write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 64,
        })?,
        read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 32,
        })?,
        statx_inline_message(0, RAMFS_BOOT_PATH, 0, 0)?,
    ])
}

pub fn run_request_batch<const N: usize>(
    service: &mut RamFsService,
    requests: [Message; N],
) -> Result<[Message; N], VfsError> {
    run_typed_request_loop(service, requests)
}

pub fn run_request_loop(service: &mut RamFsService) -> Result<RamFsLoopSummary, VfsError> {
    let open = run_request_batch(service, scripted_bootstrap_requests()?)?[0];
    let fd = decode_reply_u64(open);

    let io = run_request_batch(service, scripted_bootstrap_io(fd)?)?;
    let read_len = decode_reply_u64(io[1]);
    let statx_value = decode_reply_u64(io[2]);

    Ok(RamFsLoopSummary {
        fd,
        read_len,
        statx_value,
        handled: service.handled_count(),
        metrics: service.backend().metrics(),
    })
}

pub fn run() {
    let mut svc = RamFsService::with_backend(RamFsBackend::new());
    let summary = run_request_loop(&mut svc).expect("ramfs loop");

    yarm_user_rt::user_log!(
        "ramfs.srv request-loop ready: fd={}, read_len={}, statx={}, handled={}, opens={}, reads={}, writes={}, bytes_read={}, bytes_written={}, errors={}",
        summary.fd,
        summary.read_len,
        summary.statx_value,
        summary.handled,
        summary.metrics.open_count,
        summary.metrics.read_count,
        summary.metrics.write_count,
        summary.metrics.bytes_read,
        summary.metrics.bytes_written,
        summary.metrics.error_count
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_user_rt::ipc::Message;
    use super::super::super::common::vfs_ipc::{
        CloseRequest, MountNamespacePolicy, MountRouter, close_message, openat_inline_message,
        statx_inline_message,
    };
    use super::super::super::common::vfs_service::VfsService;
    use super::super::super::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
    use super::super::tree::RAMFS_BOOT_PATH_PTR;
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_OPENAT};

    #[test]
    fn ramfs_service_supports_write_read_and_stat_with_metrics() {
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let summary = run_request_loop(&mut svc).expect("loop");
        assert_eq!(summary.fd, 100);
        assert_eq!(summary.read_len, 32);
        assert_eq!(summary.handled, 4);
        assert_eq!(summary.metrics.open_count, 1);
        assert_eq!(summary.metrics.write_count, 1);
        assert_eq!(summary.metrics.read_count, 1);
        assert_eq!(summary.metrics.statx_count, 1);
    }

    #[test]
    fn ramfs_protocol_vectors_match_frozen_vfs_codec() {
        let open = openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0)
        .expect("open");
        assert_eq!(open.opcode, VFS_OP_OPENAT);
        let decoded_open = OpenAtInlinePath::decode(open.as_slice()).expect("decode open");
        assert_eq!(decoded_open.path, RAMFS_BOOT_PATH);

        let write = write_message(ReadWriteRequest {
            fd: 100,
            buf_ptr: 0,
            len: 8,
        })
        .expect("write");
        assert_eq!(write.as_slice(), &ReadWriteArgs::new(100, 0, 8).encode());

        let stat = statx_inline_message(0, RAMFS_BOOT_PATH, 0, 0)
        .expect("stat");
        let decoded_stat = StatxInlinePath::decode(stat.as_slice()).expect("decode stat");
        assert_eq!(decoded_stat.path, RAMFS_BOOT_PATH);
    }

    #[test]
    fn ramfs_protocol_rejects_malformed_openat_payload() {
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let malformed = Message::with_header(0, VFS_OP_OPENAT, 0, None, &[1, 2, 3]).expect("msg");
        assert_eq!(svc.handle(malformed), Err(VfsError::Malformed));
    }

    #[test]
    fn ramfs_mount_gate_routes_with_policy_denial() {
        let router = MountRouter::new(0xB000, RamFsBackend::new(), InitramfsBackend::new(4096));
        let mut svc = VfsService::with_backend(router);
        svc.mount(RAMFS_BOOT_PATH_PTR, 1).expect("mount ramfs");
        svc.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2)
            .expect("mount initramfs");
        svc.set_policy(
            MountNamespacePolicy::deny_all()
                .with_range(RAMFS_BOOT_PATH_PTR, RAMFS_BOOT_PATH_PTR)
                .with_range(
                    INITRAMFS_BOOT_MARKER_PATH_PTR,
                    INITRAMFS_BOOT_MARKER_PATH_PTR,
                ),
        );

        let open_ramfs = svc
            .handle_request(
                openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"),
            )
            .expect("ramfs open");
        assert_eq!(open_ramfs.opcode, VFS_OP_OPENAT);

        let denied = svc.handle_request(
            openat_inline_message(0, b"denied", 0, 0)
            .expect("open"),
        );
        assert_eq!(denied, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn ramfs_lifecycle_gate_covers_mount_failure_recovery_and_close() {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.mount(RAMFS_BOOT_PATH_PTR, 1).expect("mount");
        let open = svc
            .handle_request(
                openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"),
            )
            .expect("open reply");
        let fd = decode_reply_u64(open);

        svc.mark_mount_failed(RAMFS_BOOT_PATH_PTR)
            .expect("mark failed");
        svc.recover_mount(RAMFS_BOOT_PATH_PTR).expect("recover");

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
                .expect("read"),
            ),
            Err(VfsError::BadFd)
        );
        svc.unmount(RAMFS_BOOT_PATH_PTR).expect("unmount");
    }

    #[test]
    fn ramfs_inflight_fd_survives_mount_failure_until_close() {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.mount(RAMFS_BOOT_PATH_PTR, 1).expect("mount");
        let open = svc
            .handle_request(
                openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"),
            )
            .expect("open reply");
        let fd = decode_reply_u64(open);
        svc.mark_mount_failed(RAMFS_BOOT_PATH_PTR)
            .expect("mark failed");

        let read_ok = svc
            .handle_request(
                read_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 1,
                })
                .expect("read"),
            )
            .expect("read while failed");
        assert_eq!(decode_reply_u64(read_ok), 0);

        let _ = svc
            .handle_request(close_message(CloseRequest { fd }).expect("close req"))
            .expect("close");
    }
}
