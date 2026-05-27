// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::super::common::vfs_ipc::VfsError;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, read_message, statx_inline_message, write_message,
};
use yarm_ipc_abi::vfs_abi::{ReadWriteArgs, VFS_OP_READ};
use super::super::common::service::FsService;
use yarm_srv_common::cpio::CpioArchive;
use yarm_srv_common::service_loop::run_typed_request_loop;
use yarm_srv_common::vfs_core::VfsBackend;
use super::archive::{
    INITRAMFS_BOOT_MARKER_PATH, INITRAMFS_DRIVER_MANAGER_PATH, InitramfsBackend, InitramfsMetrics,
};
use super::install_boot_initrd_bytes;
use yarm_srv_common::vfs_reply::VfsReply;

/// Trace gate for hot-path initramfs bulk-read per-request logs.
/// Set to `true` only when debugging Phase 2B transfer-buffer bulk reads.
/// Default `false`: INITRAMFS_READ_BULK and INITRAMFS_READ_BULK_REPLY are suppressed.
const INITRAMFS_READ_BULK_TRACE: bool = false;

pub type InitramfsService = FsService<InitramfsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitramfsBackendSource {
    Cpio,
    Placeholder,
}

fn build_runtime_backend() -> (InitramfsBackend, InitramfsBackendSource, usize) {
    // Primary path: kernel mapped boot initrd read-only into our address space
    // at startup, passing the user VA and length via startup slots 15 and 16.
    let ctx = yarm_user_rt::runtime::startup_context();
    yarm_user_rt::user_log!(
        "INITRAMFS_STARTUP_ARGS initrd_ptr={} initrd_len={}",
        ctx.initrd_ptr.unwrap_or(0),
        ctx.initrd_len.unwrap_or(0)
    );
    if let (Some(initrd_ptr), Some(initrd_len)) = (ctx.initrd_ptr, ctx.initrd_len) {
        if initrd_len > 0 && initrd_len < (256 * 1024 * 1024) {
            yarm_user_rt::user_log!(
                "INITRAMFS_STARTUP_INITRD source=ro-mem ptr=0x{:x} len={}",
                initrd_ptr, initrd_len
            );
            // SAFETY: The kernel mapped this physical memory as read-only into our
            // address space at startup (startup slots 15/16). The mapping is permanent
            // for the lifetime of this process.
            let cpio: &'static [u8] = unsafe {
                core::slice::from_raw_parts(initrd_ptr as *const u8, initrd_len as usize)
            };
            // Populate global atomics so boot_initrd_bytes() works for other callers
            install_boot_initrd_bytes(cpio);
            let entries = CpioArchive::new(cpio)
                .entries()
                .flatten()
                .filter(|e| e.is_regular_file())
                .count();
            if entries == 0 {
                let mut first6 = [0u8; 6];
                let first6_len = core::cmp::min(cpio.len(), first6.len());
                first6[..first6_len].copy_from_slice(&cpio[..first6_len]);
                yarm_user_rt::user_log!(
                    "INITRAMFS_CPIO_EMPTY len={} first6={:?}",
                    cpio.len(),
                    first6
                );
            }
            return (
                InitramfsBackend::from_cpio_newc_static(cpio),
                InitramfsBackendSource::Cpio,
                entries,
            );
        }
    }
    // No kernel-provided initrd mapping: fall back to placeholder.
    // This means VFS exec (image_id 7-9) will be unavailable.
    (InitramfsBackend::new(8192), InitramfsBackendSource::Placeholder, 0)
}

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
    yarm_user_rt::user_log!("INITRAMFS_SRV_ENTRY");
    let (backend, backend_source, entries) = build_runtime_backend();
    let mut svc = InitramfsService::with_backend(backend);
    let driver_manager_size = svc
        .backend_mut()
        .statx_path(INITRAMFS_DRIVER_MANAGER_PATH)
        .unwrap_or(0);
    match backend_source {
        InitramfsBackendSource::Cpio => {
            yarm_user_rt::user_log!(
                "INITRAMFS_BACKEND_SOURCE source=cpio entries={} driver_manager_size={}",
                entries,
                driver_manager_size
            );
        }
        InitramfsBackendSource::Placeholder => {
            yarm_user_rt::user_log!(
                "INITRAMFS_BACKEND_SOURCE source=placeholder reason=missing_boot_initrd driver_manager_size={}",
                driver_manager_size
            );
            yarm_user_rt::user_log!(
                "INITRAMFS_RUNTIME_LIMITATION missing_real_cpio=true vfs_exec_unavailable=true"
            );
        }
    }
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

    // Become a long-lived resident server. Never return from run().
    yarm_user_rt::user_log!("INITRAMFS_SRV_RESIDENT_WAIT_BEGIN");
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("INITRAMFS_SRV_RECV_CAP cap={}", recv_cap);
        yarm_user_rt::user_log!("INITRAMFS_SRV_BLOCKING_RECV_LOOP");
        loop {
            // SAFETY: recv_cap is a kernel-provided startup receive endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let Some(reply_cap) = received.reply_cap else { continue; };
                    yarm_user_rt::user_log!(
                        "INITRAMFS_SRV_GOT_MSG opcode={} reply_cap={}",
                        msg.opcode, reply_cap
                    );
                    if msg.opcode == VFS_OP_READ {
                        let payload = msg.as_slice();
                        if let Ok(args) = ReadWriteArgs::decode(payload) {
                            yarm_user_rt::user_log!(
                                "INITRAMFS_READ fd={} requested={}",
                                args.fd, args.len
                            );
                        }
                    }
                    // Phase 3 readiness: log INITRAMFS_READ_BULK when VFS routes bulk reads here.
                    // In Phase 2 the PM uses kernel syscall nr=27 directly, so this branch is
                    // not exercised during normal boot. It will be activated in Phase 3 when
                    // the bulk-read IPC path is wired through VFS.
                    if msg.opcode == yarm_ipc_abi::vfs_abi::VFS_OP_READ_BULK {
                        let payload = msg.as_slice();
                        if let Ok(args) = yarm_ipc_abi::vfs_abi::BulkReadArgs::decode(payload) {
                            if INITRAMFS_READ_BULK_TRACE {
                                yarm_user_rt::user_log!(
                                    "INITRAMFS_READ_BULK fd={} requested={}",
                                    args.fd, args.requested_len
                                );
                            }
                            // Validate requested_len
                            if args.requested_len > 4096 {
                                yarm_user_rt::user_log!(
                                    "INITRAMFS_READ_BULK_BAD_LEN requested={}",
                                    args.requested_len
                                );
                                let _ = unsafe {
                                    yarm_user_rt::syscall::ipc_reply(
                                        reply_cap,
                                        &yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"),
                                    )
                                };
                                continue;
                            }
                            // Validate dst_ptr is non-zero
                            if args.dst_ptr == 0 {
                                yarm_user_rt::user_log!(
                                    "INITRAMFS_READ_BULK_BAD_BUFFER reason=null_dst_ptr"
                                );
                                let _ = unsafe {
                                    yarm_user_rt::syscall::ipc_reply(
                                        reply_cap,
                                        &yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"),
                                    )
                                };
                                continue;
                            }
                            // Phase 2B: look up CPIO name and file length for this fd.
                            // Kernel syscall nr=27 (arg5=PM_TID) copies data directly into PM's
                            // transfer buffer at dst_ptr via cross-ASID copy.
                            let cpio_name_opt = svc.backend().cpio_name_for_fd(args.fd);
                            let file_len_opt = svc.backend().file_len_for_fd(args.fd);
                            match (cpio_name_opt, file_len_opt) {
                                (Some(cpio_name), Some(file_len)) => {
                                    let max_len = core::cmp::min(
                                        args.requested_len as usize,
                                        4096,
                                    );
                                    // SAFETY: initramfs_write_to_pm_buf delegates to kernel
                                    // syscall nr=27 with arg5=PM_BOOTSTRAP_TID (3), which writes
                                    // to PM's address space at dst_ptr.  The kernel validates the
                                    // target ASID before performing the cross-task copy.
                                    let result = unsafe {
                                        yarm_user_rt::syscall::initramfs_write_to_pm_buf(
                                            cpio_name,
                                            args.offset,
                                            args.dst_ptr as usize,
                                            max_len,
                                        )
                                    };
                                    match result {
                                        Ok(copied_len) => {
                                            let eof = copied_len == 0
                                                || args.offset.saturating_add(copied_len as u64) >= file_len;
                                            let reply = yarm_ipc_abi::vfs_abi::BulkReadReply {
                                                copied_len: copied_len as u64,
                                                eof,
                                            };
                                            let reply_bytes = reply.encode();
                                            let response = yarm_user_rt::ipc::Message::new(0, &reply_bytes)
                                                .unwrap_or_else(|_| yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"));
                                            if INITRAMFS_READ_BULK_TRACE {
                                                yarm_user_rt::user_log!(
                                                    "INITRAMFS_READ_BULK_REPLY fd={} copied={} eof={}",
                                                    args.fd, copied_len, eof
                                                );
                                            }
                                            let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                                        }
                                        Err(e) => {
                                            yarm_user_rt::user_log!(
                                                "INITRAMFS_READ_BULK_FAIL fd={} reason={:?}",
                                                args.fd, e
                                            );
                                            let _ = unsafe {
                                                yarm_user_rt::syscall::ipc_reply(
                                                    reply_cap,
                                                    &yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"),
                                                )
                                            };
                                        }
                                    }
                                }
                                _ => {
                                    yarm_user_rt::user_log!(
                                        "INITRAMFS_READ_BULK_FAIL fd={} reason=bad_fd",
                                        args.fd
                                    );
                                    let _ = unsafe {
                                        yarm_user_rt::syscall::ipc_reply(
                                            reply_cap,
                                            &yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"),
                                        )
                                    };
                                }
                            }
                        } else {
                            // Malformed payload
                            let _ = unsafe {
                                yarm_user_rt::syscall::ipc_reply(
                                    reply_cap,
                                    &yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg"),
                                )
                            };
                        }
                    } else if msg.opcode == yarm_ipc_abi::vfs_abi::VFS_OP_FILE_GRANT_RO {
                        // Phase 3A: Caller requests a read-only MemoryObject cap for a CPIO file.
                        let payload = msg.as_slice();
                        match yarm_ipc_abi::vfs_abi::FileGrantRoArgs::decode(payload) {
                            Ok(args) => {
                                let cpio_name_opt = svc.backend().cpio_name_for_fd(args.fd);
                                match cpio_name_opt {
                                    Some(cpio_name) => {
                                        // Create InitramfsFileSlice MemoryObject via kernel syscall nr=28.
                                        // SAFETY: cpio_name is a static/long-lived byte slice from the CPIO archive.
                                        let result = unsafe {
                                            yarm_user_rt::syscall::create_initramfs_file_slice_mo(
                                                cpio_name,
                                                0,
                                            )
                                        };
                                        match result {
                                            Ok((cap_id, file_len)) => {
                                                yarm_user_rt::user_log!(
                                                    "INITRAMFS_FILE_GRANT_RO_REPLY path={} len={} cap={}",
                                                    core::str::from_utf8(cpio_name).unwrap_or("<utf8err>"),
                                                    file_len, cap_id
                                                );
                                                let reply_bytes = yarm_ipc_abi::vfs_abi::FileGrantRoReply {
                                                    file_len,
                                                    status: 0,
                                                }.encode();
                                                // Build reply with transferred MemoryObject cap.
                                                let response = Message::with_header(
                                                    0,
                                                    0,
                                                    Message::FLAG_CAP_TRANSFER,
                                                    Some(cap_id as u64),
                                                    &reply_bytes,
                                                ).unwrap_or_else(|_| Message::new(1, &[]).expect("err msg"));
                                                let _ = unsafe {
                                                    yarm_user_rt::syscall::ipc_reply(reply_cap, &response)
                                                };
                                            }
                                            Err(e) => {
                                                yarm_user_rt::user_log!(
                                                    "INITRAMFS_FILE_GRANT_RO_FAIL fd={} reason={:?}",
                                                    args.fd, e
                                                );
                                                let _ = unsafe {
                                                    yarm_user_rt::syscall::ipc_reply(
                                                        reply_cap,
                                                        &Message::new(1, &[]).expect("err msg"),
                                                    )
                                                };
                                            }
                                        }
                                    }
                                    None => {
                                        yarm_user_rt::user_log!(
                                            "INITRAMFS_FILE_GRANT_RO_FAIL fd={} reason=bad_fd",
                                            args.fd
                                        );
                                        let _ = unsafe {
                                            yarm_user_rt::syscall::ipc_reply(
                                                reply_cap,
                                                &Message::new(1, &[]).expect("err msg"),
                                            )
                                        };
                                    }
                                }
                            }
                            Err(_) => {
                                let _ = unsafe {
                                    yarm_user_rt::syscall::ipc_reply(
                                        reply_cap,
                                        &Message::new(1, &[]).expect("err msg"),
                                    )
                                };
                            }
                        }
                    } else {
                        let response = svc.handle(msg).unwrap_or_else(|_| {
                            yarm_user_rt::ipc::Message::new(1, &[]).expect("err msg")
                        });
                        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                    }
                }
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                }
            }
        }
    } else {
        yarm_user_rt::user_log!("INITRAMFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec;
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
                .with_prefix(b"/dev")
                .with_prefix(b"/initramfs"),
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
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/driver_manager", 0o100755, &[0xAA; 256]);
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());
        let router = MountRouter::new(
            0x4800_0000_0000_0000,
            DevFsBackend::default(),
            InitramfsBackend::from_cpio_newc_static(leaked),
        );
        let mut svc = VfsService::with_backend(router);
        svc.set_policy(MountNamespacePolicy::deny_all().with_prefix(b"/initramfs"));

        let reply = svc
            .handle_request(
                statx_inline_message(0, INITRAMFS_DRIVER_MANAGER_PATH, 0, 0).expect("statx inline"),
            )
            .expect("statx reply");

        assert_eq!(reply.opcode, yarm_ipc_abi::vfs_abi::VFS_OP_STATX);
        assert_eq!(decode_reply_u64(reply), 256);
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

    #[test]
    fn build_runtime_backend_uses_placeholder_when_boot_initrd_missing() {
        // Explicitly clear slots to avoid races with other tests that set slots 15/16
        yarm_user_rt::runtime::install_startup_arg_slots([0u64; 18]);
        let (_backend, source, _entries) = build_runtime_backend();
        assert_eq!(source, InitramfsBackendSource::Placeholder);
    }

    #[test]
    fn build_runtime_backend_uses_placeholder_when_no_initrd_slots() {
        // Reset slots to all zeros (no initrd)
        yarm_user_rt::runtime::install_startup_arg_slots([0u64; 18]);
        let (_backend, source, _entries) = build_runtime_backend();
        assert_eq!(source, InitramfsBackendSource::Placeholder);
    }

    #[test]
    fn build_runtime_backend_uses_cpio_when_startup_slots_set() {
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/driver_manager", 0o100755, b"\x7fELFdriver");
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());

        let mut slots = [0u64; 18];
        slots[15] = leaked.as_ptr() as u64;  // initrd_ptr
        slots[16] = leaked.len() as u64;     // initrd_len
        yarm_user_rt::runtime::install_startup_arg_slots(slots);

        let (_backend, source, entries) = build_runtime_backend();
        assert_eq!(source, InitramfsBackendSource::Cpio);
        assert!(entries >= 1);

        // Clean up: reset slots
        yarm_user_rt::runtime::install_startup_arg_slots([0u64; 18]);
    }

    #[test]
    fn build_runtime_backend_keeps_cpio_source_for_present_but_invalid_initrd() {
        let invalid: &'static [u8] = Box::leak(vec![1u8, 2, 3, 4, 5, 6].into_boxed_slice());
        let mut slots = [0u64; 18];
        slots[15] = invalid.as_ptr() as u64;
        slots[16] = invalid.len() as u64;
        yarm_user_rt::runtime::install_startup_arg_slots(slots);

        let (mut backend, source, entries) = build_runtime_backend();
        assert_eq!(source, InitramfsBackendSource::Cpio);
        assert_eq!(entries, 0);
        assert_eq!(backend.statx_path(INITRAMFS_DRIVER_MANAGER_PATH), Ok(1536));

        yarm_user_rt::runtime::install_startup_arg_slots([0u64; 18]);
    }

    #[test]
    fn initramfs_read_returns_elf_magic_for_cpio_backed_driver_manager() {
        let mut cpio_data = Vec::new();
        push_entry(&mut cpio_data, "sbin/driver_manager", 0o100755, b"\x7fELFdm-binary");
        push_entry(&mut cpio_data, "TRAILER!!!", 0, &[]);
        let leaked: &'static [u8] = Box::leak(cpio_data.into_boxed_slice());

        let mut svc = InitramfsService::with_backend(
            InitramfsBackend::from_cpio_newc_static(leaked)
        );
        use super::super::archive::INITRAMFS_DRIVER_MANAGER_PATH;
        let fd = decode_reply_u64(
            svc.handle(
                openat_inline_message(0, INITRAMFS_DRIVER_MANAGER_PATH, 0, 0).expect("open")
            ).expect("open r")
        );
        let reply = svc.handle(
            read_message(ReadWriteRequest { fd, buf_ptr: 0, len: 64 }).expect("read")
        ).expect("read r");
        let (status, n, bytes) = DecodedReply::decode_read_extended(reply.as_slice()).expect("decode");
        assert_eq!(status, 0);
        assert!(n >= 4);
        assert_eq!(&bytes[..4], b"\x7fELF");
    }
}
