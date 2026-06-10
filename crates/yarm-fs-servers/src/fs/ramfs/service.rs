// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{
    CloseRequest, ReadWriteRequest, VfsError, close_message, openat_inline_message, read_message,
    statx_inline_message, write_message,
};
use super::tree::{RAMFS_BOOT_PATH, RamFsBackend, RamFsLimits, RamFsMetrics};
use yarm_srv_common::service_loop::run_typed_request_loop;
use yarm_srv_common::vfs_reply::VfsReply;
use yarm_user_rt::ipc::Message;

pub type RamFsService = FsService<RamFsBackend>;

pub const RAMFS_DEFAULT_MOUNT_PREFIX: &[u8] = b"/ram";
pub const RAMFS_MOUNT_CONFIG_FLAG_READONLY: u16 = 1 << 0;
pub const RAMFS_MOUNT_CONFIG_SOURCE_STARTUP_WORDS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamFsMountConfig {
    pub prefix: [u8; 8],
    pub prefix_len: u8,
    pub readonly: bool,
    pub max_bytes: u32,
}

impl RamFsMountConfig {
    pub const fn default_compat() -> Self {
        Self {
            prefix: [b'/', b'r', b'a', b'm', 0, 0, 0, 0],
            prefix_len: 4,
            readonly: false,
            max_bytes: super::tree::RAMFS_DEFAULT_MAX_BYTES as u32,
        }
    }

    pub fn new(prefix: &[u8], readonly: bool, max_bytes: u32) -> Option<Self> {
        if prefix.is_empty() || prefix.len() > 8 || prefix[0] != b'/' {
            return None;
        }
        let mut out = [0u8; 8];
        out[..prefix.len()].copy_from_slice(prefix);
        Some(Self {
            prefix: out,
            prefix_len: prefix.len() as u8,
            readonly,
            max_bytes,
        })
    }

    pub fn prefix(&self) -> &[u8] {
        &self.prefix[..self.prefix_len as usize]
    }

    pub fn encode_startup_words(self) -> (u64, u64) {
        let mut prefix_word = 0u64;
        let mut idx = 0usize;
        while idx < self.prefix.len() {
            prefix_word |= (self.prefix[idx] as u64) << (idx * 8);
            idx += 1;
        }
        let flags = if self.readonly {
            RAMFS_MOUNT_CONFIG_FLAG_READONLY
        } else {
            0
        };
        let meta = (self.max_bytes as u64)
            | ((flags as u64) << 32)
            | ((self.prefix_len as u64) << 48)
            | ((RAMFS_MOUNT_CONFIG_SOURCE_STARTUP_WORDS as u64) << 56);
        (prefix_word, meta)
    }

    pub fn decode_startup_words(prefix_word: u64, meta: u64) -> Option<Self> {
        if prefix_word == 0 || meta == 0 {
            return None;
        }
        let prefix_len = ((meta >> 48) & 0xff) as u8;
        if prefix_len == 0 || prefix_len > 8 {
            return None;
        }
        let source = ((meta >> 56) & 0xff) as u8;
        if source != RAMFS_MOUNT_CONFIG_SOURCE_STARTUP_WORDS {
            return None;
        }
        let mut prefix = [0u8; 8];
        let mut idx = 0usize;
        while idx < 8 {
            prefix[idx] = ((prefix_word >> (idx * 8)) & 0xff) as u8;
            idx += 1;
        }
        if prefix[0] != b'/' {
            return None;
        }
        let flags = ((meta >> 32) & 0xffff) as u16;
        Some(Self {
            prefix,
            prefix_len,
            readonly: (flags & RAMFS_MOUNT_CONFIG_FLAG_READONLY) != 0,
            max_bytes: (meta & 0xffff_ffff) as u32,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamFsStartupConfig {
    pub mount_config: Option<RamFsMountConfig>,
}

impl RamFsStartupConfig {
    pub const fn default_compat() -> Self {
        Self {
            mount_config: Some(RamFsMountConfig::default_compat()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RamFsServiceStartup {
    Mounted { mount_config: RamFsMountConfig },
    MountFailed(VfsError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamFsLoopSummary {
    pub fd: u64,
    pub write_len: u64,
    pub read_len: u64,
    pub statx_value: u64,
    pub handled: usize,
    pub metrics: RamFsMetrics,
}

pub fn startup_config_from_runtime() -> RamFsStartupConfig {
    let raw_prefix = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SERVICE_EXTRA_CAP_1,
    )
    .unwrap_or(0);
    let raw_meta =
        yarm_user_rt::runtime::startup_arg_slot(yarm_user_rt::runtime::STARTUP_SLOT_INITRD_PTR)
            .unwrap_or(0);
    RamFsStartupConfig {
        mount_config: RamFsMountConfig::decode_startup_words(raw_prefix, raw_meta),
    }
}

pub fn service_from_startup_config(
    config: RamFsStartupConfig,
) -> Result<(RamFsService, RamFsMountConfig), RamFsServiceStartup> {
    let mount_config = config.mount_config.unwrap_or_else(|| {
        yarm_user_rt::user_log!("RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config");
        RamFsMountConfig::default_compat()
    });
    if config.mount_config.is_some() {
        yarm_user_rt::user_log!(
            "RAMFS_CONFIG_FOUND prefix={}",
            alloc::string::String::from_utf8_lossy(mount_config.prefix())
        );
    }
    let backend = RamFsBackend::with_limits(RamFsLimits {
        max_bytes: mount_config.max_bytes as usize,
        max_nodes: super::tree::RAMFS_DEFAULT_MAX_NODES,
    });
    Ok((RamFsService::with_backend(backend), mount_config))
}

fn decode_reply_u64(reply: Message) -> u64 {
    VfsReply::from_opcode_payload_checked(reply.opcode, reply.as_slice())
        .expect("decode vfs reply")
        .as_u64()
}

fn scripted_bootstrap_requests() -> Result<[Message; 1], VfsError> {
    Ok([openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0)?])
}

fn scripted_bootstrap_io(fd: u64) -> Result<[Message; 4], VfsError> {
    Ok([
        write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 64,
        })?,
        close_message(CloseRequest { fd })?,
        openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0)?,
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
    let write_len = decode_reply_u64(io[0]);
    let reopened_fd = decode_reply_u64(io[2]);
    let read_reply = run_request_batch(
        service,
        [read_message(ReadWriteRequest {
            fd: reopened_fd,
            buf_ptr: 0,
            len: 32,
        })?],
    )?[0];
    let read_len = decode_reply_u64(read_reply);
    let statx_value = decode_reply_u64(io[3]);

    Ok(RamFsLoopSummary {
        fd: reopened_fd,
        write_len,
        read_len,
        statx_value,
        handled: service.handled_count(),
        metrics: service.backend().metrics(),
    })
}

pub fn run_with_config(config: RamFsStartupConfig) -> RamFsServiceStartup {
    yarm_user_rt::user_log!("RAMFS_SRV_ENTRY");
    match service_from_startup_config(config) {
        Ok((mut svc, mount_config)) => match run_request_loop(&mut svc) {
            Ok(summary) => {
                yarm_user_rt::user_log!(
                    "RAMFS_MOUNT_READY prefix={} fd={} read_len={} write_len={} handled={}",
                    alloc::string::String::from_utf8_lossy(mount_config.prefix()),
                    summary.fd,
                    summary.read_len,
                    summary.write_len,
                    summary.handled
                );
                RamFsServiceStartup::Mounted { mount_config }
            }
            Err(err) => {
                yarm_user_rt::user_log!("RAMFS_MOUNT_FAILED reason={:?}", err);
                RamFsServiceStartup::MountFailed(err)
            }
        },
        Err(err) => err,
    }
}

fn run_resident_service_loop(svc: &mut RamFsService) {
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("RAMFS_SRV_RECV_CAP cap={}", recv_cap);
        yarm_user_rt::user_log!("RAMFS_SRV_BLOCKING_RECV_LOOP");
        loop {
            // SAFETY: ramfs_srv owns its startup-provided service recv endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let Some(reply_cap) = received.reply_cap else {
                        continue;
                    };
                    let response = svc.handle(msg).unwrap_or_else(|_| {
                        yarm_user_rt::ipc::Message::new(1, &[]).expect("err-reply")
                    });
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                }
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                }
            }
        }
    } else {
        yarm_user_rt::user_log!("RAMFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
}

pub fn run_resident(config: RamFsStartupConfig) {
    yarm_user_rt::user_log!("RAMFS_SRV_ENTRY");
    match service_from_startup_config(config) {
        Ok((mut svc, mount_config)) => match run_request_loop(&mut svc) {
            Ok(summary) => {
                yarm_user_rt::user_log!(
                    "RAMFS_MOUNT_READY prefix={} fd={} read_len={} write_len={} handled={}",
                    alloc::string::String::from_utf8_lossy(mount_config.prefix()),
                    summary.fd,
                    summary.read_len,
                    summary.write_len,
                    summary.handled
                );
                yarm_user_rt::user_log!("RAMFS_SRV_READY");
                run_resident_service_loop(&mut svc);
            }
            Err(err) => {
                yarm_user_rt::user_log!("RAMFS_MOUNT_FAILED reason={:?}", err);
            }
        },
        Err(RamFsServiceStartup::MountFailed(err)) => {
            yarm_user_rt::user_log!("RAMFS_MOUNT_FAILED startup reason={:?}", err);
        }
        Err(_) => {
            yarm_user_rt::user_log!("RAMFS_MOUNT_FAILED startup reason=unknown");
        }
    }
}

pub fn run() {
    run_resident(startup_config_from_runtime());
}

#[cfg(test)]
mod tests {
    use super::super::super::common::vfs_ipc::{
        CloseRequest, MountNamespacePolicy, MountRouter, close_message, openat_inline_message,
        read_message, statx_inline_message, write_message,
    };
    use super::super::super::common::vfs_service::VfsService;
    use super::super::super::initramfs::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};
    use super::super::tree::{RAMFS_BOOT_PATH_PTR, RamFsNodeKind};
    use super::*;
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_OP_OPENAT};
    use yarm_srv_common::vfs_core::VfsBackend;
    use yarm_user_rt::ipc::Message;

    #[test]
    fn ramfs_service_supports_write_read_and_stat_with_metrics() {
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let summary = run_request_loop(&mut svc).expect("loop");
        assert_eq!(summary.fd, 101);
        assert_eq!(summary.write_len, 64);
        assert_eq!(summary.read_len, 32);
        assert_eq!(summary.handled, 6);
        assert_eq!(summary.metrics.open_count, 2);
        assert_eq!(summary.metrics.write_count, 1);
        assert_eq!(summary.metrics.read_count, 1);
        assert_eq!(summary.metrics.statx_count, 1);
    }

    #[test]
    fn ramfs_protocol_vectors_match_frozen_vfs_codec() {
        let open = openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open");
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

        let stat = statx_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("stat");
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
                .with_prefix(b"/ram")
                .with_prefix(b"/initramfs"),
        );

        let open_ramfs = svc
            .handle_request(openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"))
            .expect("ramfs open");
        assert_eq!(open_ramfs.opcode, VFS_OP_OPENAT);

        let denied = svc.handle_request(openat_inline_message(0, b"denied", 0, 0).expect("open"));
        assert_eq!(denied, Err(VfsError::PermissionDenied));
    }

    #[test]
    fn ramfs_lifecycle_gate_covers_mount_failure_recovery_and_close() {
        let mut svc = VfsService::with_backend(RamFsBackend::new());
        svc.mount(RAMFS_BOOT_PATH_PTR, 1).expect("mount");
        let open = svc
            .handle_request(openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"))
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
            .handle_request(openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"))
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

    #[test]
    fn ramfs_config_pack_unpack_contract() {
        let default = RamFsMountConfig::default_compat();
        assert_eq!(default.prefix(), b"/ram");
        assert!(!default.readonly);

        for (prefix, readonly, max_bytes) in [
            (b"/ram".as_slice(), false, 4096u32),
            (b"/mnt/ram".as_slice(), true, 8192u32),
            (b"/1234567".as_slice(), false, u32::MAX),
        ] {
            let config = RamFsMountConfig::new(prefix, readonly, max_bytes).expect("config");
            let (prefix_word, meta) = config.encode_startup_words();
            let decoded =
                RamFsMountConfig::decode_startup_words(prefix_word, meta).expect("decode");
            assert_eq!(decoded.prefix(), prefix);
            assert_eq!(decoded.readonly, readonly);
            assert_eq!(decoded.max_bytes, max_bytes);
            assert_eq!(decoded, config);
        }

        assert!(RamFsMountConfig::new(b"relative", false, 1).is_none());
        assert!(RamFsMountConfig::new(b"/too-long", false, 1).is_none());
        assert!(RamFsMountConfig::decode_startup_words(0, 0).is_none());

        let (mut prefix_word, meta) = default.encode_startup_words();
        prefix_word &= !0xff;
        assert!(RamFsMountConfig::decode_startup_words(prefix_word, meta).is_none());

        let (prefix_word, mut meta) = default.encode_startup_words();
        meta &= !(0xffu64 << 56);
        assert!(RamFsMountConfig::decode_startup_words(prefix_word, meta).is_none());

        let (_, fallback) = service_from_startup_config(RamFsStartupConfig { mount_config: None })
            .expect("fallback service");
        assert_eq!(fallback, default);
    }

    #[test]
    fn run_with_config_mounts_configured_prefix() {
        let config = RamFsMountConfig::new(b"/mnt/ram", false, 4096).expect("config");
        let startup = RamFsStartupConfig {
            mount_config: Some(config),
        };
        assert_eq!(
            run_with_config(startup),
            RamFsServiceStartup::Mounted {
                mount_config: config
            }
        );
    }

    #[test]
    fn ramfs_vfs_write_read_close_and_statx() {
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let open = svc
            .handle(openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open"))
            .expect("open reply");
        let fd = decode_reply_u64(open);
        let written = svc
            .handle(
                write_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 16,
                })
                .expect("write"),
            )
            .expect("write reply");
        assert_eq!(decode_reply_u64(written), 16);
        svc.handle(close_message(CloseRequest { fd }).expect("close"))
            .expect("close reply");
        let fd = decode_reply_u64(
            svc.handle(openat_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("open2"))
                .expect("open2 reply"),
        );
        let read = svc
            .handle(
                read_message(ReadWriteRequest {
                    fd,
                    buf_ptr: 0,
                    len: 32,
                })
                .expect("read"),
            )
            .expect("read reply");
        assert_eq!(decode_reply_u64(read), 16);
        let stat = svc
            .handle(statx_inline_message(0, RAMFS_BOOT_PATH, 0, 0).expect("stat"))
            .expect("stat reply");
        assert_ne!(decode_reply_u64(stat), 0);
    }

    #[test]
    fn unsupported_non_file_ops_are_clear() {
        let mut backend = RamFsBackend::new();
        assert_eq!(backend.node_kind(b"/ram"), Ok(RamFsNodeKind::Directory));
        assert_eq!(backend.ioctl(100, 0, 0), Err(VfsError::Unsupported));
    }

    #[test]
    fn ramfs_userspace_integration_source_markers_are_present() {
        let init_src = include_str!(
            "../../../../../crates/yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        let pm_src = include_str!(
            "../../../../../crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs"
        );
        let initramfs_src =
            include_str!("../../../../../crates/yarm-fs-servers/src/fs/initramfs/archive.rs");
        let ramfs_bin_src = include_str!("../../bin/ramfs_srv.rs");
        for marker in [
            "INIT_RAMFS_SPAWN_BEGIN",
            "INIT_RAMFS_SPAWN_OK",
            "VFS_MOUNT_REGISTER_RAMFS_OK prefix=",
        ] {
            assert!(init_src.contains(marker), "missing init marker: {marker}");
        }
        assert!(pm_src.contains("PM_IMAGE_ID_11_RAMFS_SRV"));
        assert!(pm_src.contains("/initramfs/sbin/ramfs_srv"));
        assert!(initramfs_src.contains("INITRAMFS_RAMFS_SRV_PATH"));
        assert!(initramfs_src.contains("sbin/ramfs_srv"));
        assert!(ramfs_bin_src.contains("RAMFS_BIN_ENTRY_START"));
        assert!(ramfs_bin_src.contains("RAMFS_BEFORE_RUN"));
    }

    #[test]
    fn ramfs_server_contract_docs_match_behavior() {
        let doc = include_str!("../../../../../doc/RAMFS_SERVER_CONTRACT.md");
        assert!(doc.contains("/ram"));
        for marker in [
            "INIT_RAMFS_SPAWN_BEGIN",
            "INIT_RAMFS_SPAWN_OK",
            "PM_IMAGE_ID_11_RAMFS_SRV",
            "RAMFS_BIN_ENTRY_START",
            "RAMFS_BEFORE_RUN",
            "RAMFS_CONFIG_FOUND prefix=...",
            "RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config",
            "RAMFS_MOUNT_READY prefix=...",
            "RAMFS_MOUNT_FAILED reason=...",
            "VFS_MOUNT_REGISTER_RAMFS_OK prefix=...",
        ] {
            assert!(doc.contains(marker), "missing doc marker: {marker}");
        }
        assert!(doc.contains("memory-only"));
        assert!(doc.contains("VFS_OP_WRITE"));
    }
}
