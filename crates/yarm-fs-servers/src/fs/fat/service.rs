// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{
    CloseRequest, ReadWriteRequest, VfsError, close_message, openat_inline_message, read_message,
    statx_inline_message, write_inline_reply_message,
};
use super::fs::{FAT_HELLO_PATH, FatBackend, FatBackendKind, FatError};
use yarm_ipc_abi::vfs_abi::{
    VFS_OP_WRITE_INLINE, VFS_SHARED_IO_F_CURRENT_OFFSET, VFS_SHARED_IO_STATUS_OK,
    VfsWriteInlineReply, VfsWriteInlineRequest,
};
use yarm_srv_common::service_loop::RequestResponseService;
use yarm_srv_common::vfs_reply::VfsReply;
use yarm_user_rt::ipc::Message;

#[derive(Debug)]
pub struct FatService {
    inner: FsService<FatBackend>,
    inline_writes: usize,
}

impl FatService {
    pub fn with_backend(backend: FatBackend) -> Self {
        Self {
            inner: FsService::with_backend(backend),
            inline_writes: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.inner.handled_count() + self.inline_writes
    }

    pub const fn backend(&self) -> &FatBackend {
        self.inner.backend()
    }

    pub fn backend_mut(&mut self) -> &mut FatBackend {
        self.inner.backend_mut()
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsError> {
        if request.opcode != VFS_OP_WRITE_INLINE {
            return self.inner.handle(request);
        }
        let inline =
            VfsWriteInlineRequest::decode(request.as_slice()).map_err(|_| VfsError::Malformed)?;
        if self.backend().backend_kind() != FatBackendKind::MemoryImage
            || inline.flags & VFS_SHARED_IO_F_CURRENT_OFFSET == 0
        {
            return Err(VfsError::Unsupported);
        }
        let bytes_written = self.backend_mut().write_bytes(inline.fd, inline.bytes)?;
        self.inline_writes = self.inline_writes.saturating_add(1);
        write_inline_reply_message(VfsWriteInlineReply {
            request_id: inline.request_id,
            bytes_completed: bytes_written as u64,
            status: VFS_SHARED_IO_STATUS_OK,
            flags: 0,
        })
    }
}

impl RequestResponseService<Message, Message> for FatService {
    type Error = VfsError;

    fn service_name(&self) -> &'static str {
        "fat"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        FatService::handle(self, request)
    }
}

pub const FAT_DEFAULT_BLOCK_DEVICE_ID: u64 = 1;
pub const FAT_DEFAULT_MOUNT_PREFIX: &[u8] = b"/fat";
pub const FAT_MOUNT_CONFIG_FLAG_READONLY: u16 = 1 << 0;
pub const FAT_MOUNT_CONFIG_BLOCK_CAP_SOURCE_SERVICE_EXTRA_0: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatBlockCapSource {
    ServiceExtraCap0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatMountConfig {
    pub prefix: [u8; 8],
    pub prefix_len: u8,
    pub device_id: u32,
    pub readonly: bool,
    pub block_cap_source: FatBlockCapSource,
}

impl FatMountConfig {
    pub const fn default_compat() -> Self {
        Self {
            prefix: [b'/', b'f', b'a', b't', 0, 0, 0, 0],
            prefix_len: 4,
            device_id: FAT_DEFAULT_BLOCK_DEVICE_ID as u32,
            readonly: true,
            block_cap_source: FatBlockCapSource::ServiceExtraCap0,
        }
    }

    pub fn new(prefix: &[u8], device_id: u32, readonly: bool) -> Option<Self> {
        if prefix.is_empty() || prefix.len() > 8 || prefix[0] != b'/' {
            return None;
        }
        let mut out = [0u8; 8];
        out[..prefix.len()].copy_from_slice(prefix);
        Some(Self {
            prefix: out,
            prefix_len: prefix.len() as u8,
            device_id,
            readonly,
            block_cap_source: FatBlockCapSource::ServiceExtraCap0,
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
            FAT_MOUNT_CONFIG_FLAG_READONLY
        } else {
            0
        };
        let meta = (self.device_id as u64)
            | ((flags as u64) << 32)
            | ((self.prefix_len as u64) << 48)
            | ((FAT_MOUNT_CONFIG_BLOCK_CAP_SOURCE_SERVICE_EXTRA_0 as u64) << 56);
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
        if source != FAT_MOUNT_CONFIG_BLOCK_CAP_SOURCE_SERVICE_EXTRA_0 {
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
            device_id: (meta & 0xffff_ffff) as u32,
            readonly: (flags & FAT_MOUNT_CONFIG_FLAG_READONLY) != 0,
            block_cap_source: FatBlockCapSource::ServiceExtraCap0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatStartupConfig {
    pub block_send_cap: Option<u32>,
    pub reply_recv_cap: Option<u32>,
    pub mount_config: Option<FatMountConfig>,
    pub allow_sample_image: bool,
}

impl FatStartupConfig {
    pub fn production(
        block_send_cap: Option<u32>,
        reply_recv_cap: Option<u32>,
        device_id: u64,
    ) -> Self {
        Self {
            block_send_cap,
            reply_recv_cap,
            mount_config: FatMountConfig::new(FAT_DEFAULT_MOUNT_PREFIX, device_id as u32, true),
            allow_sample_image: false,
        }
    }

    pub const fn hosted_sample() -> Self {
        Self {
            block_send_cap: None,
            reply_recv_cap: None,
            mount_config: Some(FatMountConfig::default_compat()),
            allow_sample_image: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatServiceStartup {
    Mounted {
        backend_kind: FatBackendKind,
        mount_config: FatMountConfig,
    },
    NoBlockBackend,
    MountFailed(FatError),
}

pub fn startup_config_from_runtime() -> FatStartupConfig {
    let ctx = yarm_user_rt::runtime::startup_context();
    let raw_prefix = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SERVICE_EXTRA_CAP_1,
    )
    .unwrap_or(0);
    let raw_meta =
        yarm_user_rt::runtime::startup_arg_slot(yarm_user_rt::runtime::STARTUP_SLOT_INITRD_PTR)
            .unwrap_or(0);
    FatStartupConfig {
        block_send_cap: ctx.service_extra_cap_0,
        reply_recv_cap: ctx.process_manager_reply_recv_cap,
        mount_config: FatMountConfig::decode_startup_words(raw_prefix, raw_meta),
        allow_sample_image: cfg!(feature = "hosted-dev"),
    }
}

pub fn service_from_startup_config(
    config: FatStartupConfig,
) -> Result<FatService, FatServiceStartup> {
    match (config.block_send_cap, config.reply_recv_cap) {
        (Some(block_send_cap), Some(reply_recv_cap)) => {
            let mount_config = config.mount_config.unwrap_or_else(|| {
                yarm_user_rt::user_log!(
                    "FAT_CONFIG_DEFAULT_DEVICE_ID device_id={} reason=missing-config",
                    FAT_DEFAULT_BLOCK_DEVICE_ID
                );
                FatMountConfig::default_compat()
            });
            if config.mount_config.is_some() {
                yarm_user_rt::user_log!(
                    "FAT_CONFIG_FOUND prefix={} device_id={}",
                    alloc::string::String::from_utf8_lossy(mount_config.prefix()),
                    mount_config.device_id
                );
            }
            yarm_user_rt::user_log!("FAT_BLOCK_BACKEND_STARTUP_CAP cap={}", block_send_cap);
            match FatBackend::from_ipc_block(
                u64::from(mount_config.device_id),
                block_send_cap,
                reply_recv_cap,
            ) {
                Ok(backend) => Ok(FatService::with_backend(backend)),
                Err(err) => Err(FatServiceStartup::MountFailed(err)),
            }
        }
        _ if config.allow_sample_image => {
            yarm_user_rt::user_log!(
                "FAT_BLOCK_BACKEND_SAMPLE_IMAGE reason=no-startup-block-cap-hosted-dev"
            );
            Ok(FatService::with_backend(FatBackend::new()))
        }
        _ => Err(FatServiceStartup::NoBlockBackend),
    }
}

fn run_mount_smoke(svc: &mut FatService) -> Result<(), VfsError> {
    let open = openat_inline_message(0, FAT_HELLO_PATH, 0, 0)?;
    let rep = svc.handle(open)?;
    let fd = VfsReply::from_opcode_payload_checked(rep.opcode, rep.as_slice())
        .map_err(|_| VfsError::Malformed)?
        .as_u64();
    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 32,
    })?;
    let _ = svc.handle(read)?;
    let stat = statx_inline_message(0, FAT_HELLO_PATH, 0, 0)?;
    let stat_rep = svc.handle(stat)?;
    let len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .map_err(|_| VfsError::Malformed)?
        .as_u64();
    let _ = svc.handle(close_message(CloseRequest { fd })?);
    yarm_user_rt::user_log!(
        "fat.srv readonly ready: fd={}, len={}, handled={}",
        fd,
        len,
        svc.handled_count()
    );
    Ok(())
}

pub fn run_with_config(config: FatStartupConfig) -> FatServiceStartup {
    yarm_user_rt::user_log!("FAT_SRV_ENTRY");
    match service_from_startup_config(config) {
        Ok(mut svc) => {
            let backend_kind = svc.backend().backend_kind();
            let mount_config = config
                .mount_config
                .unwrap_or_else(FatMountConfig::default_compat);
            match run_mount_smoke(&mut svc) {
                Ok(()) => {
                    yarm_user_rt::user_log!(
                        "FAT_MOUNT_READY prefix={} device_id={}",
                        alloc::string::String::from_utf8_lossy(mount_config.prefix()),
                        mount_config.device_id
                    );
                    FatServiceStartup::Mounted {
                        backend_kind,
                        mount_config,
                    }
                }
                Err(err) => {
                    yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason={:?}", err);
                    FatServiceStartup::MountFailed(FatError::Malformed)
                }
            }
        }
        Err(FatServiceStartup::NoBlockBackend) => {
            yarm_user_rt::user_log!("FAT_NO_BLOCK_BACKEND");
            yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason=no-block-backend");
            FatServiceStartup::NoBlockBackend
        }
        Err(FatServiceStartup::MountFailed(err)) => {
            yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason={:?}", err);
            FatServiceStartup::MountFailed(err)
        }
        Err(other) => other,
    }
}

fn run_resident_service_loop(svc: &mut FatService) {
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("FAT_SRV_RECV_CAP cap={}", recv_cap);
        yarm_user_rt::user_log!("FAT_SRV_BLOCKING_RECV_LOOP");
        loop {
            // SAFETY: fat_srv owns its startup-provided service recv endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let Some(reply_cap) = received.reply_cap else {
                        continue;
                    };
                    let response = svc.handle(msg).unwrap_or_else(|_| {
                        yarm_user_rt::ipc::Message::new(1, &[]).expect("err-reply")
                    });
                    let _ =
                        unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                }
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                }
            }
        }
    } else {
        yarm_user_rt::user_log!("FAT_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
}

pub fn run_resident(config: FatStartupConfig) -> FatServiceStartup {
    yarm_user_rt::user_log!("FAT_SRV_ENTRY");
    match service_from_startup_config(config) {
        Ok(mut svc) => {
            let backend_kind = svc.backend().backend_kind();
            let mount_config = config
                .mount_config
                .unwrap_or_else(FatMountConfig::default_compat);
            match run_mount_smoke(&mut svc) {
                Ok(()) => {
                    yarm_user_rt::user_log!(
                        "FAT_MOUNT_READY prefix={} device_id={}",
                        alloc::string::String::from_utf8_lossy(mount_config.prefix()),
                        mount_config.device_id
                    );
                    yarm_user_rt::user_log!("FAT_SRV_READY");
                    run_resident_service_loop(&mut svc);
                    FatServiceStartup::Mounted {
                        backend_kind,
                        mount_config,
                    }
                }
                Err(err) => {
                    yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason={:?}", err);
                    FatServiceStartup::MountFailed(FatError::Malformed)
                }
            }
        }
        Err(FatServiceStartup::NoBlockBackend) => {
            yarm_user_rt::user_log!("FAT_NO_BLOCK_BACKEND");
            yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason=no-block-backend");
            FatServiceStartup::NoBlockBackend
        }
        Err(FatServiceStartup::MountFailed(err)) => {
            yarm_user_rt::user_log!("FAT_MOUNT_FAILED reason={:?}", err);
            FatServiceStartup::MountFailed(err)
        }
        Err(other) => other,
    }
}

pub fn run() {
    let _ = run_resident(startup_config_from_runtime());
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::fs::common::vfs_ipc::{VfsBackend, write_inline_message};
    use yarm_ipc_abi::vfs_abi::{
        VFS_OP_READ_SHARED_REPLY, VFS_OP_WRITE_SHARED_REQUEST, VFS_SHARED_IO_F_CURRENT_OFFSET,
        VfsWriteInlineReply,
    };

    #[test]
    fn fat_service_chooses_ipc_block_backend_when_config_present() {
        let err = service_from_startup_config(FatStartupConfig::production(Some(7), Some(8), 1))
            .expect_err(
                "test caps are not real kernel IPC caps, so mounting should try IPC then fail",
            );
        assert!(matches!(err, FatServiceStartup::MountFailed(_)));
    }

    #[test]
    fn fat_service_refuses_without_block_backend_in_production_config() {
        assert!(matches!(
            service_from_startup_config(FatStartupConfig::production(None, Some(8), 1)),
            Err(FatServiceStartup::NoBlockBackend)
        ));
        assert_eq!(
            run_with_config(FatStartupConfig::production(None, None, 1)),
            FatServiceStartup::NoBlockBackend
        );
    }

    #[test]
    fn hosted_sample_backend_still_mounts_and_reads() {
        let mut svc = service_from_startup_config(FatStartupConfig::hosted_sample())
            .expect("hosted sample image mounts");
        assert_eq!(svc.backend().backend_kind(), FatBackendKind::MemoryImage);
        let fd = svc.backend_mut().openat_path(FAT_HELLO_PATH).expect("open");
        let mut out = [0u8; 16];
        let (n, inline) = svc.backend_mut().read_into(fd, 16, &mut out).expect("read");
        assert_eq!(n, 13);
        assert_eq!(inline, 13);
        assert_eq!(svc.backend_mut().statx_path(FAT_HELLO_PATH), Ok(13));
        assert_eq!(svc.backend_mut().write(fd, 1), Ok(1));
        assert_eq!(svc.backend_mut().statx_path(FAT_HELLO_PATH), Ok(14));
    }

    #[test]
    fn fat_service_routes_exact_inline_overwrite_and_persists_image() {
        let mut svc = FatService::with_backend(FatBackend::new());
        let fd = svc.backend_mut().openat_path(FAT_HELLO_PATH).expect("open");
        let request = write_inline_message(VfsWriteInlineRequest {
            fd,
            file_offset: 0,
            request_id: 11,
            flags: VFS_SHARED_IO_F_CURRENT_OFFSET,
            bytes: b"Exact",
        })
        .expect("inline request");
        let reply = svc.handle(request).expect("inline write");
        assert_eq!(reply.opcode, VFS_OP_WRITE_INLINE);
        assert_eq!(
            VfsWriteInlineReply::decode(reply.as_slice()).expect("reply"),
            VfsWriteInlineReply {
                request_id: 11,
                bytes_completed: 5,
                status: VFS_SHARED_IO_STATUS_OK,
                flags: 0,
            }
        );

        let image = svc.backend().memory_image().expect("memory image").to_vec();
        let mut remounted = FatBackend::from_image(image).expect("remount");
        let fd = remounted.openat_path(FAT_HELLO_PATH).expect("reopen");
        let mut out = [0u8; 13];
        let (read, inline) = remounted.read_into(fd, 13, &mut out).expect("read back");
        assert_eq!((read, inline), (13, 13));
        assert_eq!(&out[..5], b"Exact");
        assert_eq!(remounted.statx_path(FAT_HELLO_PATH), Ok(13));
    }

    #[test]
    fn fat_service_inline_append_crosses_cluster_and_updates_size() {
        let mut backend = FatBackend::new();
        let seed = vec![0x31; 500];
        backend.write_path(FAT_HELLO_PATH, &seed).expect("seed");
        let fd = backend.openat_path(FAT_HELLO_PATH).expect("open");
        let mut consumed = vec![0u8; 500];
        assert_eq!(backend.read_into(fd, 500, &mut consumed), Ok((500, 500)));
        let mut svc = FatService::with_backend(backend);
        let suffix = [0x7au8; 30];
        let request = write_inline_message(VfsWriteInlineRequest {
            fd,
            file_offset: 0,
            request_id: 12,
            flags: VFS_SHARED_IO_F_CURRENT_OFFSET,
            bytes: &suffix,
        })
        .expect("inline append");
        svc.handle(request).expect("append");

        let image = svc.backend().memory_image().expect("memory image").to_vec();
        let mut remounted = FatBackend::from_image(image).expect("remount");
        assert_eq!(remounted.statx_path(FAT_HELLO_PATH), Ok(530));
        let fd = remounted.openat_path(FAT_HELLO_PATH).expect("reopen");
        let mut all = vec![0u8; 530];
        assert_eq!(remounted.read_into(fd, 530, &mut all), Ok((530, 530)));
        assert_eq!(&all[..500], seed.as_slice());
        assert_eq!(&all[500..], suffix.as_slice());
    }

    #[test]
    fn fat_inline_route_does_not_enable_shared_opcodes() {
        let mut svc = FatService::with_backend(FatBackend::new());
        for opcode in [VFS_OP_READ_SHARED_REPLY, VFS_OP_WRITE_SHARED_REQUEST] {
            let message = Message::with_header(0, opcode, 0, None, &[]).expect("message");
            assert_eq!(svc.handle(message), Err(VfsError::Unsupported));
        }
    }

    #[test]
    fn fat_config_parse_and_default_behavior() {
        let default = FatMountConfig::default_compat();
        assert_eq!(default.prefix(), b"/fat");
        assert_eq!(default.device_id, 1);
        assert!(default.readonly);
        assert_eq!(
            default.block_cap_source,
            FatBlockCapSource::ServiceExtraCap0
        );

        for (prefix, device_id, readonly) in [
            (b"/fat".as_slice(), 1u32, true),
            (b"/mnt/fat".as_slice(), 42u32, true),
            (b"/1234567".as_slice(), u32::MAX, false),
        ] {
            let configured = FatMountConfig::new(prefix, device_id, readonly).expect("config");
            let (prefix_word, meta) = configured.encode_startup_words();
            let decoded = FatMountConfig::decode_startup_words(prefix_word, meta).expect("decode");
            assert_eq!(decoded.prefix(), prefix);
            assert_eq!(decoded.device_id, device_id);
            assert_eq!(decoded.readonly, readonly);
            assert_eq!(
                decoded.block_cap_source,
                FatBlockCapSource::ServiceExtraCap0
            );
            assert_eq!(decoded, configured);
        }

        assert!(FatMountConfig::new(b"relative", 1, true).is_none());
        assert!(FatMountConfig::new(b"/too-long", 1, true).is_none());
        assert!(FatMountConfig::decode_startup_words(0, 0).is_none());
        let configured = FatMountConfig::new(b"/fat", 7, true).expect("config");
        let (prefix_word, mut meta) = configured.encode_startup_words();
        meta &= !(0xffu64 << 56);
        assert!(FatMountConfig::decode_startup_words(prefix_word, meta).is_none());
    }

    #[test]
    fn configured_device_id_overrides_default() {
        let configured = FatMountConfig::new(b"/mnt/fat", 42, true).expect("config");
        let cfg = FatStartupConfig {
            block_send_cap: Some(7),
            reply_recv_cap: Some(8),
            mount_config: Some(configured),
            allow_sample_image: false,
        };
        assert_eq!(cfg.mount_config.unwrap().device_id, 42);
        assert_eq!(cfg.mount_config.unwrap().prefix(), b"/mnt/fat");
    }

    #[test]
    fn production_block_cap_with_config_selects_ipc_backend_attempt() {
        let configured = FatMountConfig::new(b"/mnt/fat", 42, true).expect("config");
        let err = service_from_startup_config(FatStartupConfig {
            block_send_cap: Some(7),
            reply_recv_cap: Some(8),
            mount_config: Some(configured),
            allow_sample_image: false,
        })
        .expect_err("fake caps still force IPC mount failure in tests");
        assert!(matches!(err, FatServiceStartup::MountFailed(_)));
    }

    #[test]
    fn fat_userspace_integration_source_markers_are_present() {
        let init_src = include_str!(
            "../../../../../crates/yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        let pm_src = include_str!(
            "../../../../../crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs"
        );
        let fat_bin_src = include_str!("../../bin/fat_srv.rs");
        for marker in [
            "INIT_FAT_SPAWN_BEGIN",
            "INIT_FAT_SPAWN_OK",
            "VFS_MOUNT_REGISTER_FAT_OK prefix=",
        ] {
            assert!(init_src.contains(marker), "missing init marker: {marker}");
        }
        assert!(pm_src.contains("PM_IMAGE_ID_10_FAT_SRV"));
        assert!(pm_src.contains("/initramfs/sbin/fat_srv"));
        assert!(fat_bin_src.contains("FAT_BIN_ENTRY_START"));
        assert!(fat_bin_src.contains("FAT_BEFORE_RUN"));
    }

    #[test]
    fn fat_server_contract_docs_match_startup_backend_behavior() {
        let doc = include_str!("../../../../../doc/FAT_SERVER_CONTRACT.md");
        assert!(doc.contains("service_extra_cap_0"));
        assert!(doc.contains("/mnt/fat"));
        assert!(doc.contains("device id"));
        assert!(doc.contains("FAT_NO_BLOCK_BACKEND"));
        assert!(doc.contains("PM_IMAGE_ID_10_FAT_SRV"));
        assert!(doc.contains("VFS_MOUNT_REGISTER_FAT_OK prefix="));
        assert!(doc.contains("sample image"));
        assert!(doc.contains("read-only"));
        assert!(doc.contains("VfsError::Unsupported"));
        assert!(doc.contains("Production write-path audit"));
        assert!(doc.contains("VFS_OP_WRITE_INLINE = 28"));
        assert!(doc.contains("BLK_OP_WRITE = 0x0203"));
        assert!(doc.contains("1–96 payload bytes"));
        assert!(doc.contains("opcodes 26/27 remain unsupported"));
    }
}
