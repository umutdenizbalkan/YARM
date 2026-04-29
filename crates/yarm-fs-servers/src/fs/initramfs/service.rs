// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::super::common::vfs_ipc::VfsError;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, read_message, statx_inline_message, write_message,
};
use super::super::common::service::FsService;
use yarm_srv_common::service_loop::run_typed_request_loop;
use super::archive::{
    INITRAMFS_BOOT_MARKER_PATH, InitramfsBackend, InitramfsMetrics,
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
    Ok([openat_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)?])
}

fn scripted_bootstrap_io(fd: u64) -> Result<[Message; 2], VfsError> {
    Ok([
        read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 512,
        })?,
        statx_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)?,
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

    yarm_user_rt::user_log!(
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
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec::Vec;
    use super::super::archive::{INITRAMFS_BOOT_MARKER_PATH, INITRAMFS_BOOT_MARKER_PATH_PTR};
    use super::super::super::common::vfs_ipc::{
        CloseRequest, MountNamespacePolicy, MountRouter, close_message, openat_inline_message,
        read_message, statx_inline_message,
    };
    use super::super::super::common::vfs_service::VfsService;
    use super::super::super::devfs::{DEV_CONSOLE_PATH_PTR, DevFsBackend};
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_OPENAT, VFS_OP_READ};
    use yarm_srv_common::vfs_reply::VfsReply as DecodedReply;

    fn push_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let namesz = name.len() + 1;
        let mut h = [0u8; 110];
        h[0..6].copy_from_slice(b"070701");
        h[14..22].copy_from_slice(format!("{mode:08x}").as_bytes());
        h[54..62].copy_from_slice(format!("{:08x}", data.len()).as_bytes());
        h[94..102].copy_from_slice(format!("{namesz:08x}").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 { out.push(0); }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 { out.push(0); }
    }

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
        let open = openat_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)
        .expect("open");
        assert_eq!(open.opcode, VFS_OP_OPENAT);
        let decoded_open = OpenAtInlinePath::decode(open.as_slice()).expect("decode open");
        assert_eq!(decoded_open.path, INITRAMFS_BOOT_MARKER_PATH);

        let read = read_message(ReadWriteRequest {
            fd: 10,
            buf_ptr: 0,
            len: 32,
        })
        .expect("read");
        assert_eq!(read.opcode, VFS_OP_READ);
        assert_eq!(read.as_slice(), &ReadWriteArgs::new(10, 0, 32).encode());

        let statx = statx_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)
        .expect("statx");
        let decoded_statx = StatxInlinePath::decode(statx.as_slice()).expect("decode statx");
        assert_eq!(decoded_statx.path, INITRAMFS_BOOT_MARKER_PATH);
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
            .handle_request(openat_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0).expect("open"))
            .expect("initramfs open");
        assert_eq!(open_init.opcode, VFS_OP_OPENAT);

        let denied = svc.handle_request(openat_inline_message(0, b"denied", 0, 0).expect("open"));
        assert_eq!(denied, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn inline_statx_routes_to_initramfs_by_real_path_bytes() {
        let router = MountRouter::new(
            0x4800_0000_0000_0000,
            DevFsBackend::default(),
            InitramfsBackend::new(4096),
        );
        let mut svc = VfsService::with_backend(router);
        svc.set_policy(MountNamespacePolicy::deny_all());

        let reply = svc
            .handle_request(
                statx_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0).expect("statx inline"),
            )
            .expect("statx reply");

        assert_eq!(reply.opcode, yarm_ipc_abi::vfs_abi::VFS_OP_STATX);
        assert_eq!(
            decode_reply_u64(reply),
            0x1000_0000_0000_0000 | 0o400 | (4096 << 16)
        );
    }

    #[test]
    fn initramfs_lifecycle_gate_covers_mount_failure_recovery_and_close() {
        let mut svc = VfsService::with_backend(InitramfsBackend::new(4096));
        svc.mount(INITRAMFS_BOOT_MARKER_PATH_PTR, 2).expect("mount");

        let open = svc
            .handle_request(
                openat_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)
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
                openat_inline_message(0, INITRAMFS_BOOT_MARKER_PATH, 0, 0)
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

    #[test]
    fn initramfs_read_returns_init_bytes_when_cpio_backed() {
        let mut cpio = Vec::new();
        push_entry(&mut cpio, "init", 0o100755, b"\x7fELFinit-binary");
        push_entry(&mut cpio, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio.into_boxed_slice());
        let mut svc = InitramfsService::with_backend(InitramfsBackend::from_cpio_newc_static(leaked));
        let fd = decode_reply_u64(
            svc.handle(openat_inline_message(0, b"/initramfs/init", 0, 0).expect("open")).expect("open r")
        );
        let reply = svc.handle(read_message(ReadWriteRequest { fd, buf_ptr: 0, len: 64 }).expect("read")).expect("read r");
        let (status, n, bytes) = DecodedReply::decode_read_extended(reply.as_slice()).expect("decode");
        assert_eq!(status, 0);
        assert_eq!(n, 15);
        assert_eq!(&bytes[..n as usize], b"\x7fELFinit-binary");
    }
}
