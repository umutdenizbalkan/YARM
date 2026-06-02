// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{
    CloseRequest, ReadWriteRequest, VfsError, close_message, openat_inline_message, read_message,
    statx_inline_message,
};
use super::fs::{FAT_HELLO_PATH, FatBackend, FatBackendKind, FatError};
use yarm_srv_common::vfs_reply::VfsReply;

pub type FatService = FsService<FatBackend>;

pub const FAT_DEFAULT_BLOCK_DEVICE_ID: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatStartupConfig {
    pub block_send_cap: Option<u32>,
    pub reply_recv_cap: Option<u32>,
    pub device_id: u64,
    pub allow_sample_image: bool,
}

impl FatStartupConfig {
    pub const fn production(
        block_send_cap: Option<u32>,
        reply_recv_cap: Option<u32>,
        device_id: u64,
    ) -> Self {
        Self {
            block_send_cap,
            reply_recv_cap,
            device_id,
            allow_sample_image: false,
        }
    }

    pub const fn hosted_sample() -> Self {
        Self {
            block_send_cap: None,
            reply_recv_cap: None,
            device_id: FAT_DEFAULT_BLOCK_DEVICE_ID,
            allow_sample_image: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatServiceStartup {
    Mounted { backend_kind: FatBackendKind },
    NoBlockBackend,
    MountFailed(FatError),
}

pub fn startup_config_from_runtime() -> FatStartupConfig {
    let ctx = yarm_user_rt::runtime::startup_context();
    FatStartupConfig {
        block_send_cap: ctx.service_extra_cap_0,
        reply_recv_cap: ctx.process_manager_reply_recv_cap,
        device_id: FAT_DEFAULT_BLOCK_DEVICE_ID,
        allow_sample_image: cfg!(feature = "hosted-dev"),
    }
}

pub fn service_from_startup_config(
    config: FatStartupConfig,
) -> Result<FatService, FatServiceStartup> {
    match (config.block_send_cap, config.reply_recv_cap) {
        (Some(block_send_cap), Some(reply_recv_cap)) => {
            yarm_user_rt::user_log!("FAT_BLOCK_BACKEND_STARTUP_CAP cap={}", block_send_cap);
            match FatBackend::from_ipc_block(config.device_id, block_send_cap, reply_recv_cap) {
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
            match run_mount_smoke(&mut svc) {
                Ok(()) => {
                    yarm_user_rt::user_log!("FAT_MOUNT_READY");
                    FatServiceStartup::Mounted { backend_kind }
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
    let _ = run_with_config(startup_config_from_runtime());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::common::vfs_ipc::VfsBackend;

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
        assert_eq!(svc.backend_mut().write(fd, 1), Err(VfsError::Unsupported));
    }

    #[test]
    fn fat_server_contract_docs_match_startup_backend_behavior() {
        let doc = include_str!("../../../../../doc/FAT_SERVER_CONTRACT.md");
        assert!(doc.contains("service_extra_cap_0"));
        assert!(doc.contains("FAT_NO_BLOCK_BACKEND"));
        assert!(doc.contains("sample image"));
        assert!(doc.contains("read-only"));
        assert!(doc.contains("VfsError::Unsupported"));
    }
}
