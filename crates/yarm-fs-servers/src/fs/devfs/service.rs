// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::super::common::vfs_ipc::VfsError;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, statx_inline_message, write_message,
};
use super::super::common::service::FsService;
use yarm_srv_common::service_loop::run_typed_request_loop;
use super::nodes::{DEV_CONSOLE_PATH, DEV_NULL_PATH, DevFsBackend, DevFsMetrics, DevNodeRegisterError};
#[cfg(test)]
use super::nodes::{DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH_PTR};
use yarm_srv_common::vfs_reply::VfsReply;

pub type DevFsService = FsService<DevFsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevFsLoopSummary {
    pub console_fd: u64,
    pub null_fd: u64,
    pub handled: usize,
    pub metrics: DevFsMetrics,
}

fn decode_reply_u64(reply: Message) -> u64 {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .expect("decode vfs reply")
        .as_u64()
}

fn scripted_bootstrap_requests() -> Result<[Message; 2], VfsError> {
    Ok([
        openat_inline_message(0, DEV_CONSOLE_PATH, 0, 0)?,
        openat_inline_message(0, DEV_NULL_PATH, 0, 0)?,
    ])
}

fn scripted_bootstrap_io(console_fd: u64, null_fd: u64) -> Result<[Message; 4], VfsError> {
    Ok([
        write_message(ReadWriteRequest {
            fd: console_fd,
            buf_ptr: 0,
            len: 12,
        })?,
        write_message(ReadWriteRequest {
            fd: null_fd,
            buf_ptr: 0,
            len: 12,
        })?,
        statx_inline_message(0, DEV_CONSOLE_PATH, 0, 0)?,
        statx_inline_message(0, DEV_NULL_PATH, 0, 0)?,
    ])
}

pub fn run_request_batch<const N: usize>(
    service: &mut DevFsService,
    requests: [Message; N],
) -> Result<[Message; N], VfsError> {
    run_typed_request_loop(service, requests)
}

pub fn run_request_loop(service: &mut DevFsService) -> Result<DevFsLoopSummary, VfsError> {
    let opens = run_request_batch(service, scripted_bootstrap_requests()?)?;
    let console_fd = decode_reply_u64(opens[0]);
    let null_fd = decode_reply_u64(opens[1]);

    let _ = run_request_batch(service, scripted_bootstrap_io(console_fd, null_fd)?)?;

    Ok(DevFsLoopSummary {
        console_fd,
        null_fd,
        handled: service.handled_count(),
        metrics: service.backend().metrics(),
    })
}

pub fn run() {
    yarm_user_rt::user_log!("DEVFS_SRV_ENTRY");
    let mut svc = DevFsService::with_backend(DevFsBackend::default());
    let summary = run_request_loop(&mut svc).expect("devfs loop");

    yarm_user_rt::user_log!(
        "devfs.srv request-loop ready: console_fd={}, null_fd={}, handled={}, opens={}, writes={}, statx={}, errors={}",
        summary.console_fd,
        summary.null_fd,
        summary.handled,
        summary.metrics.open_count,
        summary.metrics.write_count,
        summary.metrics.statx_count,
        summary.metrics.error_count
    );

    // Become a long-lived resident server. Never return from run().
    yarm_user_rt::user_log!("DEVFS_SRV_RESIDENT_WAIT_BEGIN");
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("DEVFS_SRV_RECV_CAP cap={}", recv_cap);
        yarm_user_rt::user_log!("DEVFS_SRV_BLOCKING_RECV_LOOP");
        loop {
            // SAFETY: recv_cap is a kernel-provided startup receive endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let Some(reply_cap) = received.reply_cap else { continue; };
                    yarm_user_rt::user_log!(
                        "DEVFS_SRV_GOT_MSG opcode={} reply_cap={}",
                        msg.opcode, reply_cap
                    );

                    // ── DEVFS_OP_REGISTER_NODE — handled locally ─────────
                    if msg.opcode == yarm_ipc_abi::devfs_abi::DEVFS_OP_REGISTER_NODE {
                        use yarm_ipc_abi::devfs_abi::{
                            NodeRegistrationArgs,
                            DEVFS_REGISTER_STATUS_OK,
                            DEVFS_REGISTER_STATUS_ERR_DUPLICATE,
                            DEVFS_REGISTER_STATUS_ERR_FULL,
                            DEVFS_REGISTER_STATUS_ERR_INVALID_KIND,
                            DEVFS_REGISTER_STATUS_ERR_INVALID_PATH,
                        };
                        let status: u32 = match NodeRegistrationArgs::decode(msg.as_slice()) {
                            None => DEVFS_REGISTER_STATUS_ERR_INVALID_PATH,
                            Some(args) => {
                                match svc.backend_mut().register_dynamic_node(
                                    args.name,
                                    args.kind,
                                    args.flags,
                                    args.backend_cap,
                                ) {
                                    Ok(()) => {
                                        yarm_user_rt::user_log!(
                                            "DEVFS_SRV_NODE_REGISTERED kind={} cap={}",
                                            args.kind, args.backend_cap
                                        );
                                        DEVFS_REGISTER_STATUS_OK
                                    }
                                    Err(DevNodeRegisterError::Duplicate) => DEVFS_REGISTER_STATUS_ERR_DUPLICATE,
                                    Err(DevNodeRegisterError::TableFull) => DEVFS_REGISTER_STATUS_ERR_FULL,
                                    Err(DevNodeRegisterError::InvalidKind) => DEVFS_REGISTER_STATUS_ERR_INVALID_KIND,
                                    Err(_) => DEVFS_REGISTER_STATUS_ERR_INVALID_PATH,
                                }
                            }
                        };
                        let reply_opcode: u64 = if status == DEVFS_REGISTER_STATUS_OK { 0 } else { 1 };
                        let reply = yarm_user_rt::ipc::Message::new(reply_opcode, &status.to_le_bytes())
                            .unwrap_or_else(|_| yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"));
                        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                        continue;
                    }

                    let response = svc.handle(msg).unwrap_or_else(|_| {
                        yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg")
                    });
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                }
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                }
            }
        }
    } else {
        yarm_user_rt::user_log!("DEVFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::super::common::vfs_ipc::{
        CloseRequest, MountNamespacePolicy, MountRouter, close_message, openat_inline_message,
        statx_inline_message, write_message,
    };
    use super::super::super::common::vfs_service::VfsService;
    use super::super::super::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_OPENAT, VFS_OP_STATX, VFS_OP_WRITE};

    #[test]
    fn devfs_service_supports_console_and_null() {
        let mut svc = DevFsService::with_backend(DevFsBackend::default());
        let summary = run_request_loop(&mut svc).expect("loop");
        assert_eq!(summary.console_fd, 3);
        assert_eq!(summary.null_fd, 4);
        assert_eq!(summary.handled, 6);
        assert_eq!(summary.metrics.open_count, 2);
        assert_eq!(summary.metrics.write_count, 2);
        assert_eq!(summary.metrics.statx_count, 2);
    }

    #[test]
    fn devfs_protocol_vectors_match_frozen_vfs_codec() {
        let open_console = openat_inline_message(0, DEV_CONSOLE_PATH, 0, 0)
        .expect("open console");
        assert_eq!(open_console.opcode, VFS_OP_OPENAT);
        let decoded_open = OpenAtInlinePath::decode(open_console.as_slice()).expect("decode open");
        assert_eq!(decoded_open.path, DEV_CONSOLE_PATH);

        let write_console = write_message(ReadWriteRequest {
            fd: 3,
            buf_ptr: 0,
            len: 12,
        })
        .expect("write");
        assert_eq!(write_console.opcode, VFS_OP_WRITE);
        assert_eq!(
            write_console.as_slice(),
            &ReadWriteArgs::new(3, 0, 12).encode()
        );

        let stat_null = statx_inline_message(0, DEV_NULL_PATH, 0, 0)
        .expect("stat");
        assert_eq!(stat_null.opcode, VFS_OP_STATX);
        let decoded_stat = StatxInlinePath::decode(stat_null.as_slice()).expect("decode stat");
        assert_eq!(decoded_stat.path, DEV_NULL_PATH);
    }

    #[test]
    fn devfs_mount_gate_routes_devfs_and_initramfs_with_policy_denial() {
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
                .with_prefix(b"/dev")
                .with_prefix(b"/initramfs"),
        );

        let open_dev = svc
            .handle_request(
                openat_inline_message(0, DEV_CONSOLE_PATH, 0, 0).expect("open dev"),
            )
            .expect("dev open reply");
        assert_eq!(open_dev.opcode, VFS_OP_OPENAT);

        let open_initramfs = svc
            .handle_request(
                openat_inline_message(0, b"/initramfs/boot-marker", 0, 0).expect("open initramfs"),
            )
            .expect("initramfs open reply");
        assert_eq!(open_initramfs.opcode, VFS_OP_OPENAT);

        let denied = svc.handle_request(
            openat_inline_message(0, b"denied", 0, 0)
            .expect("denied request"),
        );
        assert_eq!(denied, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn devfs_lifecycle_gate_covers_mount_failure_recovery_and_fd_close() {
        let mut svc = VfsService::with_backend(DevFsBackend::default());
        svc.mount(DEV_CONSOLE_PATH_PTR, 1).expect("mount");
        svc.mark_mount_failed(DEV_CONSOLE_PATH_PTR)
            .expect("mark failed");
        let failed = svc
            .mount_record(DEV_CONSOLE_PATH_PTR)
            .expect("failed mount");
        assert!(failed.failed);
        assert!(!failed.active);

        svc.recover_mount(DEV_CONSOLE_PATH_PTR).expect("recover");
        let recovered = svc.mount_record(DEV_CONSOLE_PATH_PTR).expect("recovered");
        assert!(!recovered.failed);
        assert!(recovered.active);

        let open_console = svc
            .handle_request(
                openat_inline_message(0, DEV_CONSOLE_PATH, 0, 0).expect("open"),
            )
            .expect("open reply");
        let console_fd =
            VfsReply::from_opcode_payload_checked(open_console.opcode, open_console.as_slice())
                .expect("decode open")
                .as_u64();
        assert_eq!(console_fd, 3);

        let _ = svc
            .handle_request(close_message(CloseRequest { fd: console_fd }).expect("close req"))
            .expect("close");

        let write_after_close = svc.handle_request(
            write_message(ReadWriteRequest {
                fd: console_fd,
                buf_ptr: 0,
                len: 4,
            })
            .expect("write"),
        );
        assert_eq!(write_after_close, Err(VfsError::BadFd));

        svc.recover_mount(DEV_CONSOLE_PATH_PTR).expect("recover");
        svc.unmount(DEV_CONSOLE_PATH_PTR).expect("unmount");
        assert_eq!(svc.active_mounts(), 0);
    }

    #[test]
    fn devfs_inflight_fd_survives_mount_failure_until_explicit_close() {
        let mut svc = VfsService::with_backend(DevFsBackend::default());
        svc.mount(DEV_CONSOLE_PATH_PTR, 1).expect("mount");
        let open_console = svc
            .handle_request(
                openat_inline_message(0, DEV_CONSOLE_PATH, 0, 0).expect("open"),
            )
            .expect("open reply");
        let console_fd = decode_reply_u64(open_console);

        svc.mark_mount_failed(DEV_CONSOLE_PATH_PTR)
            .expect("mark failed");
        let write_while_failed = svc
            .handle_request(
                write_message(ReadWriteRequest {
                    fd: console_fd,
                    buf_ptr: 0,
                    len: 9,
                })
                .expect("write"),
            )
            .expect("write on in-flight fd");
        assert_eq!(decode_reply_u64(write_while_failed), 9);

        svc.recover_mount(DEV_CONSOLE_PATH_PTR).expect("recover");
        let write_after_recover = svc
            .handle_request(
                write_message(ReadWriteRequest {
                    fd: console_fd,
                    buf_ptr: 0,
                    len: 3,
                })
                .expect("write"),
            )
            .expect("write after recover");
        assert_eq!(decode_reply_u64(write_after_recover), 3);

        let _ = svc
            .handle_request(close_message(CloseRequest { fd: console_fd }).expect("close req"))
            .expect("close");
        assert_eq!(
            svc.handle_request(
                write_message(ReadWriteRequest {
                    fd: console_fd,
                    buf_ptr: 0,
                    len: 1,
                })
                .expect("write"),
            ),
            Err(VfsError::BadFd)
        );
    }
}
