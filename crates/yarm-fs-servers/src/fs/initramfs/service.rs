// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::super::common::vfs_ipc::VfsError;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, read_message, statx_inline_message, write_message,
};
use super::super::common::service::{FsService, ServiceResponse};
use yarm_srv_common::service_loop::run_typed_request_loop;
use super::archive::{
    INITRAMFS_BOOT_MARKER_PATH, InitramfsBackend, InitramfsMetrics,
};
use yarm_srv_common::vfs_reply::VfsReply;
use yarm_user_rt::runtime::startup_context;
use yarm_user_rt::syscall::{
    IpcV2Response, SyscallError, cap_release, ipc_recv_v2, ipc_reply_v2_msg, vm_unmap, yield_now,
};
use super::super::common::service::ServiceCleanup;

pub type InitramfsService = FsService<InitramfsBackend>;
const INITRAMFS_REAL_IPC_LOOP_STAGED: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitramfsLoopSummary {
    pub fd: u64,
    pub read_len: u64,
    pub statx_value: u64,
    pub write_allowed: bool,
    pub handled: usize,
    pub metrics: InitramfsMetrics,
}

fn decode_reply_u64(reply: impl Into<Message>) -> u64 {
    let reply = reply.into();
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
    let responses: [ServiceResponse; N] = run_typed_request_loop(service, requests)?;
    // NOTE: scripted/in-memory loop only. Do not run deferred cleanup here;
    // cleanup must execute only after a successful IPC reply/send syscall.
    Ok(responses.map(|response| response.message))
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
    if INITRAMFS_REAL_IPC_LOOP_STAGED {
        let startup = startup_context();
        if let Some(recv_cap) = resolve_initramfs_recv_cap(startup) {
            yarm_user_rt::user_log!(
                "INITRAMFS_RUNTIME_MODE mode=real_ipc recv_cap={}",
                recv_cap
            );
            run_ipc_loop(&mut svc, &mut RuntimeIpcOps, recv_cap);
        } else {
            yarm_user_rt::user_log!(
                "INITRAMFS_RUNTIME_MODE mode=no_loop reason=no_recv_cap"
            );
            return;
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
}

fn resolve_initramfs_recv_cap(ctx: yarm_user_rt::runtime::StartupContext) -> Option<u32> {
    ctx.initramfs_startup_caps_v1_from_startup_args()
        .and_then(|caps| {
            if caps.version == yarm_ipc_abi::process_abi::ServiceStartupCapsV1::VERSION
                && caps.request_recv_cap <= (u32::MAX as u64)
                && caps.request_recv_cap != 0
            {
                Some(caps.request_recv_cap as u32)
            } else {
                None
            }
        })
        .or_else(|| ctx.initramfs_request_recv_cap_from_slot11())
}

trait InitramfsIpcOps {
    fn recv_v2(&mut self, recv_cap: u32) -> Result<Option<IpcV2Response>, SyscallError>;
    fn reply_v2_msg(
        &mut self,
        reply_cap: u32,
        opcode: u16,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> Result<(), SyscallError>;
    fn yield_now(&mut self) -> Result<(), SyscallError>;
    fn vm_unmap(&mut self, base: usize, len: usize) -> Result<(), SyscallError>;
    fn cap_release(&mut self, cap: u64) -> Result<(), SyscallError>;
}

struct RuntimeIpcOps;

impl InitramfsIpcOps for RuntimeIpcOps {
    fn recv_v2(&mut self, recv_cap: u32) -> Result<Option<IpcV2Response>, SyscallError> {
        // SAFETY: syscall wrapper.
        unsafe { ipc_recv_v2(recv_cap) }
    }
    fn reply_v2_msg(
        &mut self,
        reply_cap: u32,
        opcode: u16,
        payload: &[u8],
        transfer_cap: Option<u64>,
    ) -> Result<(), SyscallError> {
        // SAFETY: syscall wrapper.
        unsafe { ipc_reply_v2_msg(reply_cap, opcode, payload, transfer_cap) }
    }
    fn yield_now(&mut self) -> Result<(), SyscallError> {
        yield_now()
    }
    fn vm_unmap(&mut self, base: usize, len: usize) -> Result<(), SyscallError> {
        // SAFETY: syscall wrapper.
        unsafe { vm_unmap(base, len) }
    }
    fn cap_release(&mut self, cap: u64) -> Result<(), SyscallError> {
        // SAFETY: syscall wrapper.
        unsafe { cap_release(cap) }
    }
}

fn run_cleanup(ops: &mut impl InitramfsIpcOps, cleanup: ServiceCleanup) {
    match cleanup {
        ServiceCleanup::VmAnonMapProducer { base, len, mem_cap } => {
            if let Err(err) = ops.vm_unmap(base, len) {
                yarm_user_rt::user_log!(
                    "initramfs.srv cleanup vm_unmap failed: base={:#x}, len={}, err={:?}",
                    base,
                    len,
                    err
                );
            }
            if let Err(err) = ops.cap_release(mem_cap) {
                yarm_user_rt::user_log!(
                    "initramfs.srv cleanup cap_release failed: cap={}, err={:?}",
                    mem_cap,
                    err
                );
            }
        }
    }
}

fn handle_one_ipc_request(
    service: &mut InitramfsService,
    ops: &mut impl InitramfsIpcOps,
    req: IpcV2Response,
) {
    let Some(reply_cap) = req.transfer_cap else {
        return;
    };
    if req.len > req.payload.len() {
        yarm_user_rt::user_log!(
            "initramfs.srv drop malformed request: len={} > payload_cap={}",
            req.len,
            req.payload.len()
        );
        return;
    }
    let request = match Message::with_header(
        0,
        req.opcode(),
        if req.transfer_cap.is_some() {
            Message::FLAG_CAP_TRANSFER
        } else {
            0
        },
        req.transfer_cap,
        &req.payload[..req.len],
    ) {
        Ok(msg) => msg,
        Err(_) => return,
    };
    let Ok(response) = service.handle_response(request) else {
        return;
    };
    let reply_msg = response.message;
    if ops
        .reply_v2_msg(
            reply_cap as u32,
            reply_msg.opcode,
            reply_msg.as_slice(),
            reply_msg.transferred_cap().map(|cap| cap.0),
        )
        .is_ok()
    {
        if let Some(cleanup) = response.cleanup {
            run_cleanup(ops, cleanup);
        }
    }
}

fn run_ipc_loop(service: &mut InitramfsService, ops: &mut impl InitramfsIpcOps, recv_cap: u32) -> ! {
    yarm_user_rt::user_log!("INITRAMFS_IPC_LOOP_READY recv_cap={}", recv_cap);
    loop {
        match ops.recv_v2(recv_cap) {
            Ok(Some(req)) => handle_one_ipc_request(service, ops, req),
            Ok(None) => {
                let _ = ops.yield_now();
            }
            Err(_) => {
                let _ = ops.yield_now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use yarm_user_rt::runtime::{install_startup_arg_slots, startup_context};
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

    #[derive(Default)]
    struct MockIpcOps {
        reply_ok: bool,
        reply_calls: usize,
        unmap_calls: usize,
        cap_release_calls: usize,
        unmap_fail: bool,
        cap_release_fail: bool,
    }

    impl InitramfsIpcOps for MockIpcOps {
        fn recv_v2(&mut self, _recv_cap: u32) -> Result<Option<IpcV2Response>, SyscallError> {
            Ok(None)
        }
        fn reply_v2_msg(
            &mut self,
            _reply_cap: u32,
            _opcode: u16,
            _payload: &[u8],
            _transfer_cap: Option<u64>,
        ) -> Result<(), SyscallError> {
            self.reply_calls += 1;
            if self.reply_ok { Ok(()) } else { Err(SyscallError::Internal) }
        }
        fn yield_now(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn vm_unmap(&mut self, _base: usize, _len: usize) -> Result<(), SyscallError> {
            self.unmap_calls += 1;
            if self.unmap_fail { Err(SyscallError::Internal) } else { Ok(()) }
        }
        fn cap_release(&mut self, _cap: u64) -> Result<(), SyscallError> {
            self.cap_release_calls += 1;
            if self.cap_release_fail { Err(SyscallError::Internal) } else { Ok(()) }
        }
    }

    fn recv_req(opcode: u16, payload: &[u8], len: usize, cap: Option<u64>) -> IpcV2Response {
        let mut buf = [0u8; Message::MAX_PAYLOAD];
        let copy_len = core::cmp::min(payload.len(), buf.len());
        buf[..copy_len].copy_from_slice(&payload[..copy_len]);
        IpcV2Response {
            status: opcode as u64,
            len,
            transfer_cap: cap,
            payload: buf,
        }
    }

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
    fn ipc_missing_reply_cap_skips_safely() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let mut ops = MockIpcOps { reply_ok: true, ..Default::default() };
        let req = recv_req(VFS_OP_OPENAT, &[], 0, None);
        handle_one_ipc_request(&mut svc, &mut ops, req);
        assert_eq!(ops.reply_calls, 0);
        assert_eq!(svc.handled_count(), 0);
    }

    #[test]
    fn ipc_malformed_len_does_not_dispatch_or_panic() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let mut ops = MockIpcOps { reply_ok: true, ..Default::default() };
        let req = recv_req(VFS_OP_OPENAT, &[], Message::MAX_PAYLOAD + 1, Some(11));
        handle_one_ipc_request(&mut svc, &mut ops, req);
        assert_eq!(ops.reply_calls, 0);
        assert_eq!(svc.handled_count(), 0);
    }

    #[test]
    fn ipc_reply_success_sends_reply() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let mut ops = MockIpcOps { reply_ok: true, ..Default::default() };
        let open = scripted_bootstrap_requests().expect("open req")[0];
        let req = recv_req(open.opcode, open.as_slice(), open.as_slice().len(), Some(11));
        handle_one_ipc_request(&mut svc, &mut ops, req);
        assert_eq!(ops.reply_calls, 1);
        assert_eq!(ops.unmap_calls, 0);
        assert_eq!(ops.cap_release_calls, 0);
    }

    #[test]
    fn cleanup_failure_attempts_both_steps_and_continues() {
        let mut ops = MockIpcOps { reply_ok: true, unmap_fail: true, cap_release_fail: true, ..Default::default() };
        let cleanup = ServiceCleanup::VmAnonMapProducer { base: 0x4000_0000, len: 4096, mem_cap: 77 };
        run_cleanup(&mut ops, cleanup);
        run_cleanup(&mut ops, cleanup);
        assert_eq!(ops.unmap_calls, 2);
        assert_eq!(ops.cap_release_calls, 2);
    }

    #[test]
    fn ipc_reply_failure_does_not_cleanup() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let mut ops = MockIpcOps { reply_ok: false, ..Default::default() };
        let open = scripted_bootstrap_requests().expect("open req")[0];
        let req = recv_req(open.opcode, open.as_slice(), open.as_slice().len(), Some(11));
        handle_one_ipc_request(&mut svc, &mut ops, req);
        assert_eq!(ops.reply_calls, 1);
        assert_eq!(ops.unmap_calls, 0);
        assert_eq!(ops.cap_release_calls, 0);
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

    #[test]
    fn structured_startup_caps_are_preferred_over_slot11_fallback() {
        install_startup_arg_slots([((1u64) << 48) | ((9u64) << 32) | 0x5354_4350, 123, 0, 0, 0, 0, 0, 0, 0, 0, 0, 77]);
        assert_eq!(resolve_initramfs_recv_cap(startup_context()), Some(123));
    }

    #[test]
    fn slot11_fallback_still_works_as_compatibility_path() {
        install_startup_arg_slots([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 77]);
        assert_eq!(resolve_initramfs_recv_cap(startup_context()), Some(77));
    }

    #[test]
    fn missing_structured_and_legacy_caps_yields_no_loop_cap() {
        install_startup_arg_slots([0; 12]);
        assert_eq!(resolve_initramfs_recv_cap(startup_context()), None);
    }
}
