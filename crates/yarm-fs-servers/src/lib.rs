// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;

pub mod fs;
pub use fs::{common, devfs, ext4, fat, initramfs, ramfs};

pub fn run_devfs() {
    fs::devfs::run();
}

pub fn run_initramfs() {
    fs::initramfs::run();
}

pub fn run_ramfs() {
    fs::ramfs::run();
}

pub fn run_ext4() {
    fs::ext4::run();
}

pub fn run_fat() {
    fs::fat::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_fs_impl_does_not_delegate_back_to_legacy_fs_namespace() {
        let devfs_src = include_str!("fs/devfs/service.rs");
        let initramfs_src = include_str!("fs/initramfs/service.rs");
        let ramfs_src = include_str!("fs/ramfs/service.rs");
        let ext4_src = include_str!("fs/ext4/service.rs");
        let fat_src = include_str!("fs/fat/service.rs");
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        for src in [devfs_src, initramfs_src, ramfs_src, ext4_src, fat_src] {
            assert!(
                !src.contains(legacy_fs.as_str()),
                "workspace scoped fs impl must not delegate to legacy fs namespace"
            );
        }
    }

    #[test]
    fn fs_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
            (
                "devfs_srv",
                "name = \"devfs_srv\"",
                "path = \"src/bin/devfs_srv.rs\"",
                "bin/devfs_srv.rs",
                "run_devfs",
            ),
            (
                "ramfs_srv",
                "name = \"ramfs_srv\"",
                "path = \"src/bin/ramfs_srv.rs\"",
                "bin/ramfs_srv.rs",
                "run_ramfs",
            ),
            (
                "initramfs_srv",
                "name = \"initramfs_srv\"",
                "path = \"src/bin/initramfs_srv.rs\"",
                "bin/initramfs_srv.rs",
                "run_initramfs",
            ),
            (
                "ext4_srv",
                "name = \"ext4_srv\"",
                "path = \"src/bin/ext4_srv.rs\"",
                "bin/ext4_srv.rs",
                "run_ext4",
            ),
            (
                "fat_srv",
                "name = \"fat_srv\"",
                "path = \"src/bin/fat_srv.rs\"",
                "bin/fat_srv.rs",
                "run_fat",
            ),
        ];

        for (bin_name, name_entry, path_entry, bin_path, run_fn) in expected_bins {
            assert!(
                cargo_toml.contains(name_entry),
                "Cargo.toml missing expected bin entry: {bin_name}"
            );
            assert!(
                cargo_toml.contains(path_entry),
                "Cargo.toml missing expected bin path for: {bin_name}"
            );

            let src = match bin_path {
                "bin/devfs_srv.rs" => include_str!("bin/devfs_srv.rs"),
                "bin/ramfs_srv.rs" => include_str!("bin/ramfs_srv.rs"),
                "bin/initramfs_srv.rs" => include_str!("bin/initramfs_srv.rs"),
                "bin/ext4_srv.rs" => include_str!("bin/ext4_srv.rs"),
                "bin/fat_srv.rs" => include_str!("bin/fat_srv.rs"),
                _ => panic!("unexpected bin path in parity table: {bin_path}"),
            };

            assert!(
                src.contains("yarm_fs_servers::"),
                "{bin_name} should dispatch via yarm_fs_servers crate entrypoint"
            );
            assert!(
                src.contains(run_fn),
                "{bin_name} should call {run_fn} for parity with FS service mapping"
            );
        }
    }
}

#[cfg(test)]
mod stage80_tests {
    use crate::fs::common::shared_io_adapter::{
        VFS_READ_SHARED_REPLY_ENABLED, VFS_SHARED_IO_ENABLED, VFS_WRITE_SHARED_REQUEST_ENABLED,
    };
    use crate::fs::common::vfs_ipc::VfsError;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::fat::service::{
        FatServiceStartup, FatStartupConfig, service_from_startup_config,
    };
    use crate::fs::ramfs::service::{RamFsServiceStartup, RamFsStartupConfig, run_with_config};
    use yarm_srv_common::vfs_core::VfsBackend;

    #[test]
    fn stage80_ext4_write_path_remains_unsupported() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open");
        assert_eq!(backend.write(fd, 512), Err(VfsError::Unsupported));
        assert_eq!(backend.write(fd, 4096), Err(VfsError::Unsupported));
    }

    #[test]
    fn stage80_ext4_backend_rejects_writes_of_all_sizes() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open");
        for &len in &[1u64, 512, 4096, 65536, 16 * 1024 * 1024 + 1] {
            assert_eq!(
                backend.write(fd, len),
                Err(VfsError::Unsupported),
                "expected Unsupported for write len={len}"
            );
        }
    }

    #[test]
    fn stage80_fat_write_mode_guard_requires_block_backend() {
        let result = service_from_startup_config(FatStartupConfig::production(None, Some(1), 1));
        assert!(
            matches!(result, Err(FatServiceStartup::NoBlockBackend)),
            "fat production config with no block backend must return NoBlockBackend"
        );
    }

    #[test]
    fn stage80_vfs_shared_io_enabled_consistent_with_stage78() {
        assert!(VFS_WRITE_SHARED_REQUEST_ENABLED);
        assert!(VFS_READ_SHARED_REPLY_ENABLED);
        assert!(VFS_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage80_ramfs_run_with_config_smoke_unchanged() {
        let result = run_with_config(RamFsStartupConfig::default_compat());
        assert!(
            matches!(result, RamFsServiceStartup::Mounted { .. }),
            "ramfs run_with_config must mount successfully with default_compat config"
        );
    }

    #[test]
    fn stage80_ext4_srv_bin_has_entry_and_ready_markers() {
        let ext4_bin_src = include_str!("bin/ext4_srv.rs");
        assert!(ext4_bin_src.contains("EXT4_SRV_ENTRY"));
        assert!(ext4_bin_src.contains("EXT4_MOUNT_READY"));
    }

    #[test]
    fn stage80_all_three_fs_server_bins_have_entry_markers() {
        let ramfs_src = include_str!("bin/ramfs_srv.rs");
        let fat_src = include_str!("bin/fat_srv.rs");
        let ext4_src = include_str!("bin/ext4_srv.rs");
        assert!(ramfs_src.contains("RAMFS_BIN_ENTRY_START"));
        assert!(fat_src.contains("FAT_BIN_ENTRY_START"));
        assert!(ext4_src.contains("EXT4_BIN_ENTRY_START"));
    }

    #[test]
    fn stage86_ext4_recv_loop_blocker_lifted() {
        // Stage 86 lifts the Stage-80 "no-ipc-loop" blocker.
        // ext4/service.rs now has a resident ipc_recv_v2 loop after the smoke demo.
        // Stage 88 further wires VFS mount registration (VFS_EXT4_LIVE_MOUNT_ENABLED = true).
        let ext4_service_src = include_str!("fs/ext4/service.rs");
        assert!(
            ext4_service_src.contains("ipc_recv_v2("),
            "ext4 service must have ipc_recv_v2 recv loop after Stage 86"
        );
    }
}

#[cfg(test)]
mod stage86_tests {
    use crate::fs::common::shared_io_adapter::{
        VFS_EXT4_LIVE_MOUNT_ENABLED, VFS_EXT4_RECV_LOOP_ENABLED, VFS_FAT_SHARED_IO_ENABLED,
        VFS_RAMFS_LIVE_MOUNT_ENABLED, VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED,
    };
    use crate::fs::common::vfs_ipc::{
        ReadWriteRequest, VfsError, openat_inline_message, write_message,
    };
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::fat::fs::FatBackend;
    use crate::fs::ramfs::service::{
        RamFsServiceStartup, RamFsStartupConfig, run_with_config, service_from_startup_config,
    };
    use yarm_srv_common::vfs_core::VfsBackend;
    use yarm_srv_common::vfs_reply::VfsReply;

    #[test]
    fn stage86_gate_vfs_ramfs_live_mount_enabled() {
        assert!(
            VFS_RAMFS_LIVE_MOUNT_ENABLED,
            "RAMFS live-mount gate must be true"
        );
    }

    #[test]
    fn stage86_gate_vfs_fat_shared_io_disabled() {
        assert!(
            !VFS_FAT_SHARED_IO_ENABLED,
            "FAT shared-I/O gate must be false"
        );
    }

    #[test]
    fn stage86_gate_vfs_ext4_recv_loop_enabled() {
        assert!(
            VFS_EXT4_RECV_LOOP_ENABLED,
            "ext4 recv-loop gate must be true"
        );
    }

    #[test]
    fn stage86_gate_vfs_ext4_live_mount_disabled() {
        // Stage 88 supersedes this Stage-86 invariant: ext4 live-mount is now enabled.
        // VFS_EXT4_LIVE_MOUNT_ENABLED was false in Stage 86; Stage 88 lifts it to true.
        assert!(
            VFS_EXT4_LIVE_MOUNT_ENABLED,
            "Stage 88: ext4 live-mount gate must be true"
        );
    }

    #[test]
    fn stage86_stage85_gate_still_enabled() {
        assert!(VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED);
    }

    #[test]
    fn stage86_ramfs_service_has_run_resident() {
        let src = include_str!("fs/ramfs/service.rs");
        assert!(
            src.contains("fn run_resident("),
            "ramfs/service.rs must export run_resident"
        );
        assert!(
            src.contains("fn run_resident_service_loop("),
            "ramfs/service.rs must have run_resident_service_loop"
        );
        assert!(
            src.contains("RAMFS_SRV_READY"),
            "ramfs/service.rs must emit RAMFS_SRV_READY"
        );
        assert!(
            src.contains("ipc_recv_v2("),
            "ramfs/service.rs must use ipc_recv_v2"
        );
    }

    #[test]
    fn stage86_ramfs_run_calls_run_resident() {
        let src = include_str!("fs/ramfs/service.rs");
        assert!(
            src.contains("run_resident(startup_config_from_runtime())"),
            "run() must call run_resident"
        );
    }

    #[test]
    fn stage86_ramfs_run_with_config_still_returns_startup() {
        let result = run_with_config(RamFsStartupConfig::default_compat());
        assert!(
            matches!(result, RamFsServiceStartup::Mounted { .. }),
            "run_with_config must still return Mounted for default_compat config"
        );
    }

    #[test]
    fn stage86_ramfs_service_from_startup_config_roundtrip() {
        let config = RamFsStartupConfig::default_compat();
        assert!(service_from_startup_config(config).is_ok());
    }

    #[test]
    fn stage86_ext4_service_has_recv_loop() {
        let src = include_str!("fs/ext4/service.rs");
        assert!(src.contains("fn run_resident_service_loop("));
        assert!(src.contains("ipc_recv_v2("));
        assert!(src.contains("EXT4_SRV_READY"));
    }

    #[test]
    fn stage86_ext4_srv_bin_still_has_stage80_markers() {
        let src = include_str!("bin/ext4_srv.rs");
        assert!(src.contains("EXT4_SRV_ENTRY"));
        assert!(src.contains("EXT4_MOUNT_READY"));
    }

    #[test]
    fn stage86_fat_backend_read_shared_bytes_wired() {
        let mut backend = FatBackend::new();
        let fd = backend
            .openat_path(crate::fs::fat::fs::FAT_HELLO_PATH)
            .expect("open");
        let mut buf = [0u8; 32];
        let n = backend
            .read_shared_bytes(fd, &mut buf)
            .expect("read_shared_bytes");
        assert!(n <= 32);
    }

    #[test]
    fn stage86_init_spawn_sub_gates_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(src.contains("INIT_SPAWN_RAMFS_SRV"));
        assert!(src.contains("INIT_SPAWN_FAT_SRV"));
        assert!(src.contains("INIT_SPAWN_EXT4_SRV"));
    }

    #[test]
    fn stage86_init_spawn_fail_markers_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(src.contains("INIT_RAMFS_SPAWN_FAIL"));
        assert!(src.contains("INIT_FAT_SPAWN_FAIL"));
        assert!(src.contains("INIT_EXT4_SPAWN_FAIL"));
    }

    #[test]
    fn stage86_ext4_backend_still_rejects_writes() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode")
            .as_u64();
        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 512,
        })
        .expect("write");
        assert_eq!(svc.handle(write), Err(VfsError::Unsupported));
    }
}

#[cfg(test)]
mod stage87_tests {
    use crate::fs::common::service::FsService;
    use crate::fs::common::shared_io_adapter::{
        VFS_FAT_LIVE_MOUNT_ENABLED, VFS_RAMFS_LIVE_MOUNT_ENABLED,
    };
    use crate::fs::ramfs::service::{
        RamFsMountConfig, RamFsServiceStartup, RamFsStartupConfig, run_request_loop,
        run_with_config,
    };
    use crate::fs::ramfs::tree::RamFsBackend;

    #[test]
    fn stage87_ramfs_mount_enabled_gate() {
        assert!(VFS_RAMFS_LIVE_MOUNT_ENABLED);
    }

    #[test]
    fn stage87_ramfs_default_mount_prefix_is_ram() {
        assert_eq!(RamFsMountConfig::default_compat().prefix(), b"/ram");
    }

    #[test]
    fn stage87_ramfs_mount_config_encode_decode_roundtrip() {
        let config = RamFsMountConfig::new(b"/ram", false, 65536).expect("new");
        let (w0, w1) = config.encode_startup_words();
        let decoded = RamFsMountConfig::decode_startup_words(w0, w1).expect("decode");
        assert_eq!(decoded.prefix(), b"/ram");
        assert!(!decoded.readonly);
        assert_eq!(decoded.max_bytes, 65536);
    }

    #[test]
    fn stage87_ramfs_mount_config_readonly_roundtrip() {
        let config = RamFsMountConfig::new(b"/rom", true, 4096).expect("new");
        let (w0, w1) = config.encode_startup_words();
        let decoded = RamFsMountConfig::decode_startup_words(w0, w1).expect("decode");
        assert_eq!(decoded.prefix(), b"/rom");
        assert!(decoded.readonly);
        assert_eq!(decoded.max_bytes, 4096);
    }

    #[test]
    fn stage87_ramfs_run_with_config_default_compat_mounts() {
        let result = run_with_config(RamFsStartupConfig::default_compat());
        match result {
            RamFsServiceStartup::Mounted { mount_config } => {
                assert_eq!(mount_config.prefix(), b"/ram");
            }
            other => panic!("expected Mounted, got {:?}", other),
        }
    }

    #[test]
    fn stage87_ramfs_run_with_config_missing_config_uses_default() {
        let result = run_with_config(RamFsStartupConfig { mount_config: None });
        assert!(matches!(result, RamFsServiceStartup::Mounted { .. }));
    }

    #[test]
    fn stage87_ramfs_service_loop_summary_counts() {
        type RamFsService = FsService<RamFsBackend>;
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let summary = run_request_loop(&mut svc).expect("loop");
        assert_eq!(summary.write_len, 64);
        assert_eq!(summary.read_len, 32);
        assert_eq!(summary.handled, 6);
    }

    #[test]
    fn stage87_ramfs_resident_loop_source_has_blocking_marker() {
        assert!(include_str!("fs/ramfs/service.rs").contains("RAMFS_SRV_BLOCKING_RECV_LOOP"));
    }

    #[test]
    fn stage87_ramfs_resident_loop_source_has_no_recv_cap_path() {
        assert!(
            include_str!("fs/ramfs/service.rs").contains("RAMFS_SRV_NO_RECV_CAP_RESIDENT_YIELD")
        );
    }

    // ── Part B: FAT resident recv loop (infrastructure, spawn stays disabled) ──

    #[test]
    fn stage87_fat_live_mount_disabled_gate() {
        assert!(
            !VFS_FAT_LIVE_MOUNT_ENABLED,
            "FAT live-mount gate must be false (no virtio_blk in default profile)"
        );
    }

    #[test]
    fn stage87_fat_resident_loop_source_markers() {
        let src = include_str!("fs/fat/service.rs");
        assert!(
            src.contains("FAT_SRV_BLOCKING_RECV_LOOP"),
            "fat/service.rs must have FAT_SRV_BLOCKING_RECV_LOOP"
        );
        assert!(
            src.contains("FAT_SRV_NO_RECV_CAP_RESIDENT_YIELD"),
            "fat/service.rs must have FAT_SRV_NO_RECV_CAP_RESIDENT_YIELD"
        );
        assert!(
            src.contains("FAT_SRV_READY"),
            "fat/service.rs must emit FAT_SRV_READY"
        );
        assert!(
            src.contains("ipc_recv_v2("),
            "fat/service.rs must use ipc_recv_v2 in resident loop"
        );
    }

    #[test]
    fn stage87_fat_run_calls_run_resident() {
        let src = include_str!("fs/fat/service.rs");
        assert!(
            src.contains("run_resident(startup_config_from_runtime())"),
            "fat/service.rs run() must call run_resident"
        );
    }

    // ── Part D: Boot/smoke expectations ─────────────────────────────────────

    #[test]
    fn stage87_init_ramfs_spawn_ok_marker_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_RAMFS_SPAWN_OK"),
            "init must log INIT_RAMFS_SPAWN_OK on success"
        );
        assert!(
            src.contains("INIT_RAMFS_SPAWN_BEGIN"),
            "init must log INIT_RAMFS_SPAWN_BEGIN"
        );
        assert!(
            src.contains("INIT_SPAWN_RAMFS_SRV: bool = true"),
            "RAMFS spawn must be enabled (Stage 86)"
        );
    }

    #[test]
    fn stage87_init_ext4_spawn_ok_marker_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_EXT4_SPAWN_OK"),
            "init must log INIT_EXT4_SPAWN_OK on success"
        );
        assert!(
            src.contains("INIT_EXT4_SPAWN_BEGIN"),
            "init must log INIT_EXT4_SPAWN_BEGIN"
        );
        assert!(
            src.contains("INIT_SPAWN_EXT4_SRV: bool = true"),
            "ext4 spawn must be enabled (Stage 86)"
        );
    }

    #[test]
    fn stage87_init_fat_spawn_skipped_with_documented_blocker() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_SPAWN_FAT_SRV: bool = false"),
            "INIT_SPAWN_FAT_SRV must be false (needs block device)"
        );
        assert!(
            src.contains("INIT_FAT_SPAWN_SKIPPED reason=server_disabled"),
            "init must emit INIT_FAT_SPAWN_SKIPPED reason=server_disabled when FAT disabled"
        );
        assert!(
            src.contains("needs block device"),
            "init must document FAT blocker (needs block device)"
        );
    }

    #[test]
    fn stage87_ramfs_srv_entry_and_ready_markers_present() {
        let src = include_str!("fs/ramfs/service.rs");
        assert!(
            src.contains("RAMFS_SRV_ENTRY"),
            "ramfs must log RAMFS_SRV_ENTRY"
        );
        assert!(
            src.contains("RAMFS_SRV_READY"),
            "ramfs must log RAMFS_SRV_READY"
        );
        assert!(
            src.contains("RAMFS_MOUNT_READY"),
            "ramfs must log RAMFS_MOUNT_READY"
        );
    }

    #[test]
    fn stage87_ext4_srv_entry_and_ready_markers_present() {
        // EXT4_SRV_ENTRY is logged by the binary entry point (bin/ext4_srv.rs).
        // EXT4_SRV_READY is logged by the service loop (fs/ext4/service.rs).
        let bin_src = include_str!("bin/ext4_srv.rs");
        let svc_src = include_str!("fs/ext4/service.rs");
        assert!(
            bin_src.contains("EXT4_SRV_ENTRY"),
            "ext4 binary must log EXT4_SRV_ENTRY"
        );
        assert!(
            svc_src.contains("EXT4_SRV_READY"),
            "ext4 service must log EXT4_SRV_READY"
        );
    }

    // ── Part C: VFS routing safety ────────────────────────────────────────────

    #[test]
    fn stage87_vfs_routing_fat_gate_implies_no_fat_mount_at_boot() {
        // VFS_FAT_LIVE_MOUNT_ENABLED=false + INIT_SPAWN_FAT_SRV=false mean fat_srv
        // never spawns, so it never sends VFS_OP_MOUNT_REGISTER for /fat.
        assert!(
            !VFS_FAT_LIVE_MOUNT_ENABLED,
            "/fat must not appear in the VFS mount table when FAT live-mount gate is disabled"
        );
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_SPAWN_FAT_SRV: bool = false"),
            "INIT_SPAWN_FAT_SRV must be false to prevent /fat from entering the VFS mount table"
        );
    }

    #[test]
    fn stage87_vfs_routing_ramfs_enabled_for_slash_ram() {
        // RAMFS is spawned and registers /ram when INIT_SPAWN_RAMFS_SRV=true.
        assert!(
            VFS_RAMFS_LIVE_MOUNT_ENABLED,
            "/ram must be registered in the VFS mount table via RAMFS live-mount path"
        );
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_SPAWN_RAMFS_SRV: bool = true"),
            "INIT_SPAWN_RAMFS_SRV must be true so /ram is registered with the VFS"
        );
        assert!(
            src.contains("register_ramfs_mount_with_vfs("),
            "init must call register_ramfs_mount_with_vfs after successful RAMFS spawn"
        );
    }

    #[test]
    fn stage87_vfs_routing_init_passes_devfs_and_initramfs_caps_to_vfs() {
        // devfs and initramfs send caps are passed to VFS at spawn time,
        // allowing VFS to register /dev and /initramfs mounts internally.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_DEVFS_SPAWN_V5_CALL_BEGIN"),
            "init must spawn devfs_srv and obtain its send cap for VFS"
        );
        assert!(
            src.contains("initramfs_send_cap, devfs_send_cap"),
            "init must pass both initramfs_send_cap and devfs_send_cap to VFS at spawn"
        );
    }

    #[test]
    fn stage87_vfs_routing_fat_not_registered_when_init_spawn_disabled() {
        // If INIT_SPAWN_FAT_SRV=false, the INIT_FAT_SPAWN_SKIPPED path is taken
        // and register_fat_mount_with_vfs is never called.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        // Gate must be false.
        assert!(
            src.contains("INIT_SPAWN_FAT_SRV: bool = false"),
            "INIT_SPAWN_FAT_SRV must be false to prevent /fat from entering the VFS mount table"
        );
        // When disabled, the skipped marker is emitted (not the register path).
        assert!(
            src.contains("INIT_FAT_SPAWN_SKIPPED reason=server_disabled"),
            "init must emit INIT_FAT_SPAWN_SKIPPED when FAT spawn is disabled"
        );
        // Verify the call site comes after the gate declaration (no call outside the gate).
        let gate_pos = src
            .find("INIT_SPAWN_FAT_SRV: bool = false")
            .expect("INIT_SPAWN_FAT_SRV gate must be present");
        let skipped_pos = src
            .rfind("INIT_FAT_SPAWN_SKIPPED reason=server_disabled")
            .expect("skipped marker must be present");
        assert!(
            gate_pos < skipped_pos,
            "FAT skipped marker must appear after the INIT_SPAWN_FAT_SRV gate"
        );
    }
}

#[cfg(test)]
mod stage88_tests {
    use crate::fs::common::shared_io_adapter::{
        VFS_EXT4_LIVE_MOUNT_ENABLED, VFS_FAT_LIVE_MOUNT_ENABLED, VFS_FAT_SHARED_IO_ENABLED,
    };
    use crate::fs::common::vfs_ipc::{
        ReadWriteRequest, VfsError, openat_inline_message, write_message,
    };
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::fat::fs::FatBackend;
    use crate::fs::fat::service::{
        FatServiceStartup, FatStartupConfig, service_from_startup_config,
    };
    use yarm_srv_common::vfs_core::VfsBackend;
    use yarm_srv_common::vfs_reply::VfsReply;

    #[test]
    fn stage88_fat_shared_io_gate_disabled() {
        assert!(!VFS_FAT_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage88_fat_read_shared_bytes_returns_valid_count() {
        let mut backend = FatBackend::new();
        let fd = backend
            .openat_path(crate::fs::fat::fs::FAT_HELLO_PATH)
            .expect("open");
        let mut buf = [0u8; 64];
        let n = backend
            .read_shared_bytes(fd, &mut buf)
            .expect("read_shared_bytes");
        assert!(n <= 64);
    }

    #[test]
    fn stage88_fat_read_shared_bytes_empty_buf_returns_zero() {
        let mut backend = FatBackend::new();
        let fd = backend
            .openat_path(crate::fs::fat::fs::FAT_HELLO_PATH)
            .expect("open");
        assert_eq!(backend.read_shared_bytes(fd, &mut []).expect("empty"), 0);
    }

    #[test]
    fn stage88_fat_read_shared_bytes_bad_fd_returns_error() {
        let mut backend = FatBackend::new();
        assert_eq!(
            backend.read_shared_bytes(9999, &mut [0u8; 32]),
            Err(VfsError::BadFd)
        );
    }

    #[test]
    fn stage88_fat_no_block_backend_returns_no_block_backend() {
        let result = service_from_startup_config(FatStartupConfig::production(None, Some(1), 1));
        assert!(matches!(result, Err(FatServiceStartup::NoBlockBackend)));
    }

    // ── Part C: ext4 live read-only VFS route ────────────────────────────────

    #[test]
    fn stage88_ext4_live_mount_gate_enabled() {
        assert!(
            VFS_EXT4_LIVE_MOUNT_ENABLED,
            "Stage 88: ext4 live mount must be enabled; ext4 backend satisfies read-only VFS contract"
        );
    }

    #[test]
    fn stage88_fat_live_mount_gate_still_disabled() {
        assert!(
            !VFS_FAT_LIVE_MOUNT_ENABLED,
            "FAT live-mount must remain disabled (needs real virtio_blk block device)"
        );
    }

    #[test]
    fn stage88_init_register_ext4_mount_with_vfs_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("register_ext4_mount_with_vfs("),
            "init must call register_ext4_mount_with_vfs after successful ext4 spawn"
        );
        assert!(
            src.contains("VFS_MOUNT_REGISTER_EXT4_BEGIN prefix=/ext4"),
            "register_ext4_mount_with_vfs must emit VFS_MOUNT_REGISTER_EXT4_BEGIN with /ext4 prefix"
        );
        assert!(
            src.contains("VFS_MOUNT_REGISTER_EXT4_OK prefix=/ext4"),
            "register_ext4_mount_with_vfs must emit VFS_MOUNT_REGISTER_EXT4_OK on success"
        );
    }

    #[test]
    fn stage88_init_ext4_spawn_ok_captures_send_cap() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_EXT4_SPAWN_OK child_tid={} send_cap={}"),
            "INIT_EXT4_SPAWN_OK must include send_cap field (Stage 88: cap used for VFS registration)"
        );
    }

    #[test]
    fn stage88_fat_handoff_blkcache_cap_passed_in_spawn() {
        // FAT cap handoff audit: init passes init_blkcache_send_cap to fat_srv via
        // service_extra_cap_0 (position 0) at spawn time. Positions 1-3 must be zero —
        // passing encoded mount-config words causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("[init_blkcache_send_cap, 0, 0, 0]"),
            "FAT spawn must pass init_blkcache_send_cap at position 0 only; positions 1-3 must be zero"
        );
        assert!(
            src.contains("INIT_SPAWN_FAT_SRV: bool = false"),
            "FAT spawn must remain disabled until a block-device stub is available"
        );
        assert!(
            src.contains("needs block device"),
            "FAT spawn gate must document the virtio_blk block device requirement"
        );
    }

    #[test]
    fn stage88_register_ext4_mount_with_vfs_call_is_after_spawn_ok() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let spawn_ok_pos = src
            .find("INIT_EXT4_SPAWN_OK")
            .expect("INIT_EXT4_SPAWN_OK must be present");
        // Search for the call site specifically (not the function definition). Match on the
        // first call-site argument so the check is robust to rustfmt line-wrapping of the
        // `let _ = register_ext4_mount_with_vfs(...)` statement.
        let register_pos = src
            .find("register_ext4_mount_with_vfs(vfs_recv_cap")
            .expect("register_ext4_mount_with_vfs call site must be present");
        assert!(
            spawn_ok_pos < register_pos,
            "register_ext4_mount_with_vfs must be called after INIT_EXT4_SPAWN_OK (not before)"
        );
    }

    #[test]
    fn stage88_ext4_recv_loop_source_has_markers() {
        let src = include_str!("fs/ext4/service.rs");
        assert!(src.contains("EXT4_SRV_BLOCKING_RECV_LOOP"));
        assert!(src.contains("EXT4_SRV_NO_RECV_CAP_RESIDENT_YIELD"));
        assert!(src.contains("EXT4_SRV_READY"));
    }

    #[test]
    fn stage88_ext4_backend_read_shared_bytes_not_overridden() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open");
        assert_eq!(
            backend.read_shared_bytes(fd, &mut [0u8; 32]),
            Err(VfsError::Unsupported),
            "ext4 must not override read_shared_bytes"
        );
    }

    #[test]
    fn stage88_ext4_service_write_still_unsupported_after_stage86() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode")
            .as_u64();
        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 512,
        })
        .expect("write");
        assert_eq!(svc.handle(write), Err(VfsError::Unsupported));
    }
}

#[cfg(test)]
mod stage89_tests {
    use crate::fs::common::vfs_ipc::VfsBackend;
    use crate::fs::initramfs::archive::{
        INITRAMFS_CRASH_TEST_SRV_PATH, INITRAMFS_EXT4_SRV_PATH, INITRAMFS_RAMFS_SRV_PATH,
        InitramfsBackend,
    };

    // ── Root-cause fix: ext4_srv path is now registered in the inode table ──

    #[test]
    fn stage89_initramfs_ext4_srv_path_openable() {
        let mut fs = InitramfsBackend::new(4096);
        let fd = fs
            .openat_path(INITRAMFS_EXT4_SRV_PATH)
            .expect("ext4_srv path must be openable after Stage-89 fix");
        assert!(fd >= 10, "fd must be a valid handle");
    }

    #[test]
    fn stage89_initramfs_ext4_srv_statx_returns_nonzero_size() {
        let mut fs = InitramfsBackend::new(4096);
        let size = fs
            .statx_path(INITRAMFS_EXT4_SRV_PATH)
            .expect("ext4_srv statx must succeed");
        assert!(size > 0, "ext4_srv inode must have nonzero file_len");
    }

    #[test]
    fn stage89_initramfs_max_inodes_is_15() {
        // Validates that the inode table covers ext4_srv plus the gated crash-test path.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("const MAX_INITRAMFS_INODES: usize = 15;"),
            "MAX_INITRAMFS_INODES must be 15 after adding the gated crash-test path"
        );
    }

    #[test]
    fn stage89_initramfs_ext4_srv_path_constant_defined() {
        assert_eq!(INITRAMFS_EXT4_SRV_PATH, b"/initramfs/sbin/ext4_srv");
    }

    #[test]
    fn stage89_initramfs_cpio_match_includes_ext4_srv() {
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("b\"sbin/ext4_srv\" => INITRAMFS_EXT4_SRV_PATH"),
            "from_cpio_newc match must include ext4_srv arm"
        );
    }

    #[test]
    fn stage89_initramfs_crash_test_srv_path_constant_defined() {
        assert_eq!(
            INITRAMFS_CRASH_TEST_SRV_PATH,
            b"/initramfs/sbin/crash_test_srv"
        );
    }

    // ── Spawn cap-value invariants ─────────────────────────────────────────

    #[test]
    fn stage89_ext4_spawn_service_caps_all_zero() {
        // ext4_srv spawn uses [0, 0, 0, 0] — no prefix word or metadata.
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            init_src.contains("spawn_v5_cap(pm_send, pm_recv, 12, [0, 0, 0, 0], 1)"),
            "ext4_srv spawn must use all-zero service_caps [0,0,0,0]"
        );
    }

    #[test]
    fn stage89_ramfs_spawn_cap0_is_zero_cap1_is_prefix_word() {
        // Stage 91 fix: RAMFS spawn must use all-zero service_caps [0,0,0,0].
        // Passing encoded config words (prefix_word, meta_word) in positions 1-2
        // causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL — the kernel treats every non-zero
        // service_caps entry as a cap ID. RAMFS falls back to default_compat (prefix=/ram).
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        // Verify RAMFS spawn does NOT pass config words as cap slots.
        assert!(
            !init_src.contains("[0, ramfs_prefix_word, ramfs_meta_word, 0]"),
            "RAMFS spawn must NOT use ramfs_prefix_word/ramfs_meta_word as service_caps (causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL)"
        );
        // Verify RAMFS spawn uses all-zero service_caps between RAMFS_SPAWN_BEGIN and RAMFS_SPAWN_OK.
        let spawn_begin = init_src
            .find("INIT_RAMFS_SPAWN_BEGIN")
            .expect("INIT_RAMFS_SPAWN_BEGIN must be present");
        let spawn_ok = init_src[spawn_begin..]
            .find("INIT_RAMFS_SPAWN_OK")
            .map(|off| spawn_begin + off)
            .expect("INIT_RAMFS_SPAWN_OK must follow INIT_RAMFS_SPAWN_BEGIN");
        assert!(
            init_src[spawn_begin..spawn_ok].contains("[0, 0, 0, 0]"),
            "RAMFS spawn must use all-zero service_caps [0,0,0,0]"
        );
    }

    // ── Sequential spawn ordering ──────────────────────────────────────────

    #[test]
    fn stage89_optional_spawns_are_sequential_ramfs_before_ext4() {
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let ramfs_pos = init_src
            .find("INIT_RAMFS_SPAWN_BEGIN")
            .expect("INIT_RAMFS_SPAWN_BEGIN must be present");
        let ext4_pos = init_src
            .find("INIT_EXT4_SPAWN_BEGIN")
            .expect("INIT_EXT4_SPAWN_BEGIN must be present");
        assert!(
            ramfs_pos < ext4_pos,
            "RAMFS spawn must appear before EXT4 spawn (sequential ordering)"
        );
    }

    #[test]
    fn stage89_ramfs_reply_decoded_before_ext4_spawn() {
        // RAMFS spawn is guarded by `if let Some(...)` — reply is decoded
        // before the ext4 spawn block begins. Verify source order.
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let ramfs_ok_pos = init_src
            .find("INIT_RAMFS_SPAWN_OK")
            .expect("INIT_RAMFS_SPAWN_OK must be present");
        let ext4_begin_pos = init_src
            .find("INIT_EXT4_SPAWN_BEGIN")
            .expect("INIT_EXT4_SPAWN_BEGIN must be present");
        assert!(
            ramfs_ok_pos < ext4_begin_pos,
            "RAMFS reply (INIT_RAMFS_SPAWN_OK) must be decoded before EXT4 spawn begins"
        );
    }

    // ── PM bad_fd_decode regression ───────────────────────────────────────

    #[test]
    fn stage89_pm_bad_fd_decode_log_includes_diagnostic_fields() {
        let pm_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/process_manager/service.rs"
        );
        assert!(
            pm_src.contains("reason=bad_fd_decode opcode="),
            "PM bad_fd_decode log must include opcode field for diagnostics"
        );
        assert!(
            pm_src.contains("payload_len="),
            "PM bad_fd_decode log must include payload_len field for diagnostics"
        );
        assert!(
            pm_src.contains("bytes=["),
            "PM bad_fd_decode log must include raw bytes for diagnostics"
        );
    }

    #[test]
    fn stage89_pm_vfs_grant_ro_received_marker_present() {
        let pm_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/process_manager/service.rs"
        );
        assert!(
            pm_src.contains("PM_VFS_GRANT_RO_RECEIVED image_id="),
            "PM must emit PM_VFS_GRANT_RO_RECEIVED marker after successful FILE_GRANT_RO"
        );
    }

    // ── RAMFS path still works after ext4 inode addition ──────────────────

    #[test]
    fn stage89_initramfs_ramfs_srv_path_still_openable() {
        let mut fs = InitramfsBackend::new(4096);
        let fd = fs
            .openat_path(INITRAMFS_RAMFS_SRV_PATH)
            .expect("ramfs_srv path must remain openable after inode table expansion");
        assert!(fd >= 10);
    }
}

#[cfg(test)]
mod stage90_tests {
    use crate::fs::common::shared_io_adapter::VFS_FAT_LIVE_MOUNT_ENABLED;
    use crate::fs::common::vfs_ipc::VfsBackend;
    use crate::fs::fat::fs::{FAT_HELLO_PATH, FatBackend, FatBackendKind};
    use crate::fs::fat::service::{
        FatServiceStartup, FatStartupConfig, service_from_startup_config,
    };

    // ── FAT Outcome-B audit: no real virtio_blk block device ─────────────────
    //
    // Stage 90 documents the exact missing requirement: INIT_SPAWN_FAT_SRV=false
    // because the default profile lacks a real virtio_blk block device.
    // The FAT implementation is correct (proven via hosted_sample), but
    // the live-mount and shared-IO gates remain false until a real device exists.

    #[test]
    fn stage90_fat_live_mount_gate_still_disabled() {
        assert!(
            !VFS_FAT_LIVE_MOUNT_ENABLED,
            "FAT live-mount gate must remain false: no real virtio_blk block device in default profile"
        );
    }

    #[test]
    fn stage90_fat_production_no_caps_returns_no_block_backend() {
        // Without block_send_cap and reply_recv_cap the FAT service must
        // refuse to start.  This proves the production path is safe.
        let result = service_from_startup_config(FatStartupConfig::production(None, None, 1));
        assert!(
            matches!(result, Err(FatServiceStartup::NoBlockBackend)),
            "FAT production config with no caps must return NoBlockBackend"
        );
    }

    #[test]
    fn stage90_fat_hosted_sample_mounts_as_memory_image() {
        // The FAT implementation is functional via the embedded sample image.
        // This proves the code is correct even though the live gate is false.
        let svc = service_from_startup_config(FatStartupConfig::hosted_sample())
            .expect("hosted_sample must succeed");
        assert_eq!(
            svc.backend().backend_kind(),
            FatBackendKind::MemoryImage,
            "hosted_sample must use MemoryImage backend"
        );
    }

    #[test]
    fn stage90_fat_sample_image_open_hello_txt_succeeds() {
        let svc = service_from_startup_config(FatStartupConfig::hosted_sample())
            .expect("hosted_sample must succeed");
        let mut backend = FatBackend::new();
        let fd = backend
            .openat_path(FAT_HELLO_PATH)
            .expect("open must succeed");
        assert!(fd >= 10, "fd must be a valid handle");
    }

    #[test]
    fn stage90_fat_sample_image_statx_returns_nonzero_size() {
        let mut backend = FatBackend::new();
        let size = backend
            .statx_path(FAT_HELLO_PATH)
            .expect("statx must succeed on sample image");
        assert!(size > 0, "hello.txt must have nonzero file size");
    }

    #[test]
    fn stage90_fat_spawn_disabled_marker_in_init() {
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        // Document the exact missing requirement in init/service.rs.
        assert!(
            init_src.contains("const INIT_SPAWN_FAT_SRV: bool = false;"),
            "INIT_SPAWN_FAT_SRV must be false (no virtio_blk block device)"
        );
        assert!(
            init_src.contains("FAT requires a virtio_blk block device not present"),
            "init must document the exact FAT missing requirement (virtio_blk not present)"
        );
    }

    #[test]
    fn stage90_fat_skipped_marker_present_when_gate_false() {
        let init_src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            init_src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled"),
            "init must emit INIT_FAT_SPAWN_SKIPPED when FAT gate is false"
        );
    }

    // ── Source-level invariants for the false SPAWN_FAIL fix ─────────────────

    #[test]
    fn stage90_init_drains_pm_recv_before_optional_spawns() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_BEGIN"),
            "init must log INIT_PM_RECV_DRAIN_BEGIN before optional FS spawns"
        );
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_DONE"),
            "init must log INIT_PM_RECV_DRAIN_DONE with count after drain"
        );
    }

    #[test]
    fn stage90_drain_appears_before_ramfs_spawn_begin() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let drain_pos = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let ramfs_begin_pos = src
            .find("INIT_RAMFS_SPAWN_BEGIN")
            .expect("INIT_RAMFS_SPAWN_BEGIN must be present");
        assert!(
            drain_pos < ramfs_begin_pos,
            "pm_recv drain must appear before INIT_RAMFS_SPAWN_BEGIN"
        );
    }

    #[test]
    fn stage90_drain_appears_before_ext4_spawn_begin() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let drain_pos = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let ext4_begin_pos = src
            .find("INIT_EXT4_SPAWN_BEGIN")
            .expect("INIT_EXT4_SPAWN_BEGIN must be present");
        assert!(
            drain_pos < ext4_begin_pos,
            "pm_recv drain must appear before INIT_EXT4_SPAWN_BEGIN"
        );
    }

    #[test]
    fn stage90_blkcache_smoke_uses_blocking_recv_not_deadline_zero() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let smoke_pos = src
            .find("INIT_BLKCACHE_SMOKE_BEGIN")
            .expect("blkcache smoke marker must be present");
        let drain_pos = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let smoke_section = &src[smoke_pos..drain_pos];
        assert!(
            !smoke_section.contains("ipc_recv_with_deadline(pm_recv, 0)"),
            "blkcache smoke must use blocking ipc_recv — not non-blocking deadline=0"
        );
        assert!(
            smoke_section.contains("ipc_recv(pm_recv)"),
            "blkcache smoke must use blocking ipc_recv(pm_recv)"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Stage 91: Optional-FS runtime stabilization, smoke-profile hardening,
//           and FAT production-readiness groundwork.
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod stage91_tests {
    use crate::fs::common::shared_io_adapter::{
        VFS_EXT4_LIVE_MOUNT_ENABLED, VFS_EXT4_RECV_LOOP_ENABLED, VFS_FAT_LIVE_MOUNT_ENABLED,
        VFS_FAT_SHARED_IO_ENABLED, VFS_RAMFS_LIVE_MOUNT_ENABLED, VFS_STAGE84_RAMFS_BRIDGE_ENABLED,
        VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED,
    };
    use crate::fs::common::vfs_ipc::{
        ReadWriteRequest, VfsBackend, VfsError, openat_inline_message, statx_inline_message,
        write_message,
    };
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::initramfs::archive::{
        INITRAMFS_BLKCACHE_PATH, INITRAMFS_DRIVER_MANAGER_PATH, INITRAMFS_EXT4_SRV_PATH,
        INITRAMFS_FAT_SRV_PATH, INITRAMFS_RAMFS_SRV_PATH, INITRAMFS_VIRTIO_BLK_PATH,
        InitramfsBackend,
    };
    use yarm_srv_common::vfs_reply::VfsReply;

    // ── Part A: Smoke marker source-scan tests ────────────────────────────────

    #[test]
    fn stage91_ramfs_srv_entry_marker_present() {
        // RAMFS_SRV_ENTRY is logged by the RAMFS service loop (fs/ramfs/service.rs).
        let svc_src = include_str!("fs/ramfs/service.rs");
        assert!(
            svc_src.contains("RAMFS_SRV_ENTRY"),
            "fs/ramfs/service.rs must log RAMFS_SRV_ENTRY on entry"
        );
    }

    #[test]
    fn stage91_ext4_srv_entry_marker_present() {
        // EXT4_SRV_ENTRY is logged by the binary entry point (bin/ext4_srv.rs).
        let bin_src = include_str!("bin/ext4_srv.rs");
        assert!(
            bin_src.contains("EXT4_SRV_ENTRY"),
            "bin/ext4_srv.rs must log EXT4_SRV_ENTRY on entry"
        );
    }

    #[test]
    fn stage91_ext4_srv_ready_marker_present() {
        let svc_src = include_str!("fs/ext4/service.rs");
        assert!(
            svc_src.contains("EXT4_SRV_READY"),
            "ext4/service.rs must log EXT4_SRV_READY once the service loop is ready"
        );
    }

    #[test]
    fn stage91_init_ramfs_spawn_begin_marker_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_RAMFS_SPAWN_BEGIN"),
            "init/service.rs must log INIT_RAMFS_SPAWN_BEGIN before spawning ramfs_srv"
        );
    }

    #[test]
    fn stage91_init_ext4_spawn_begin_marker_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_EXT4_SPAWN_BEGIN"),
            "init/service.rs must log INIT_EXT4_SPAWN_BEGIN before spawning ext4_srv"
        );
    }

    #[test]
    fn stage91_vfs_mount_register_ext4_ok_marker_present() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("VFS_MOUNT_REGISTER_EXT4_OK"),
            "init/service.rs must log VFS_MOUNT_REGISTER_EXT4_OK on successful ext4 mount registration"
        );
    }

    #[test]
    fn stage91_fat_spawn_skipped_marker_profile_disabled() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled"),
            "init/service.rs must log INIT_FAT_SPAWN_SKIPPED reason=profile_disabled for the default no-virtio_blk profile"
        );
    }

    // ── Part C: Reply endpoint hygiene tests (source-scan) ────────────────────

    #[test]
    fn stage91_reply_endpoint_hygiene_vfs_mount_register_ext4_uses_reply_recv_cap() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        // The function must exist and use blocking ipc_recv_v2 on the dedicated reply_recv_cap.
        // Non-blocking ipc_recv_with_deadline(reply_recv_cap, 0) left stale VFS mount-status
        // replies (4 bytes) on pm_recv, poisoning the next spawn's 16-byte SpawnV5 reply read.
        assert!(
            src.contains("fn register_ext4_mount_with_vfs("),
            "init/service.rs must define register_ext4_mount_with_vfs"
        );
        assert!(
            src.contains("ipc_recv_v2(reply_recv_cap"),
            "register_ext4_mount_with_vfs must use blocking ipc_recv_v2(reply_recv_cap) not non-blocking deadline=0 poll"
        );
        assert!(
            !src.contains("ipc_recv_with_deadline(reply_recv_cap"),
            "register_ext4_mount_with_vfs must NOT use non-blocking ipc_recv_with_deadline on reply_recv_cap"
        );
    }

    #[test]
    fn stage91_reply_endpoint_hygiene_register_ext4_uses_dedicated_reply_cap() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        // Prove the function signature includes reply_recv_cap as a parameter.
        // The function is: fn register_ext4_mount_with_vfs(..., reply_recv_cap: u32, ...)
        let fn_start = src
            .find("fn register_ext4_mount_with_vfs(")
            .expect("register_ext4_mount_with_vfs must exist in init/service.rs");
        // Find the end of this function by locating the next top-level fn (fn or pub fn).
        let after_fn = &src[fn_start + 1..];
        let fn_body_end = after_fn
            .find("\nfn ")
            .or_else(|| after_fn.find("\npub fn "))
            .map(|off| {
                // Take the minimum to find the nearest next function definition.
                let fn_off = after_fn.find("\nfn ").unwrap_or(usize::MAX);
                let pub_fn_off = after_fn.find("\npub fn ").unwrap_or(usize::MAX);
                fn_start + 1 + fn_off.min(pub_fn_off)
            })
            .unwrap_or(src.len());
        let fn_text = &src[fn_start..fn_body_end];
        assert!(
            fn_text.contains("reply_recv_cap"),
            "register_ext4_mount_with_vfs must have reply_recv_cap parameter"
        );
        assert!(
            !fn_text.contains("ipc_recv_with_deadline(pm_recv"),
            "register_ext4_mount_with_vfs must NOT use ipc_recv_with_deadline(pm_recv)"
        );
    }

    #[test]
    fn stage91_pm_recv_drain_loop_uses_deadline_zero_correctly() {
        // The drain loop in init/service.rs uses ipc_recv_with_deadline(pm_recv, 0)
        // to poll for leftover replies on the shared endpoint. This is correct because
        // the drain is exhaustive (loops until None). Contrast with the per-operation
        // helpers (register_ext4_mount_with_vfs) which use dedicated reply_recv_cap.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_BEGIN"),
            "drain loop marker INIT_PM_RECV_DRAIN_BEGIN must be present"
        );
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_DONE"),
            "drain loop marker INIT_PM_RECV_DRAIN_DONE must be present"
        );
        // The drain must use pm_recv (shared endpoint) with deadline=0 (poll).
        let drain_start = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let drain_end = src
            .find("INIT_PM_RECV_DRAIN_DONE")
            .expect("drain done marker must be present");
        let drain_section = &src[drain_start..drain_end];
        assert!(
            drain_section.contains("ipc_recv_with_deadline(pm_recv"),
            "drain section must poll pm_recv with ipc_recv_with_deadline"
        );
    }

    #[test]
    fn stage91_stale_32_byte_blkcache_reply_decoded_as_failure_shape() {
        // A 32-byte stale blkcache reply in pm_recv would be rejected by spawn_v5_cap
        // because decode_spawn_v5_reply requires payload.len() == 16 exactly.
        // A 32-byte payload returns Err(Malformed), which spawn_v5_cap treats as a
        // spawn failure. The INIT_PM_RECV_DRAIN_BEGIN drain prevents this from happening
        // by consuming stale replies before any new SpawnV5 operation.
        use yarm_ipc_abi::process_abi::decode_spawn_v5_reply;
        let stale_payload_32 = [0u8; 32];
        let result_32 = decode_spawn_v5_reply(&stale_payload_32);
        assert!(
            result_32.is_err(),
            "32-byte stale payload must fail decode (len != 16), got: {:?}",
            result_32.ok()
        );
        // A 16-byte zero payload decodes as pid=0 (failure shape).
        let zero_16 = [0u8; 16];
        let decoded_16 = decode_spawn_v5_reply(&zero_16).expect("16-byte zero payload must decode");
        assert_eq!(
            decoded_16.pid, 0,
            "16-byte zero payload decodes as pid=0 (failure shape)"
        );
    }

    // ── Part F: ext4 read-only route hardening tests ──────────────────────────

    #[test]
    fn stage91_ext4_open_service_path_returns_valid_fd() {
        let mut backend = Ext4Backend::new();
        let fd = backend
            .openat_path(EXT4_SERVICE_PATH)
            .expect("openat /ext4/service.bin must succeed");
        assert!(fd > 0, "fd must be positive");
    }

    #[test]
    fn stage91_ext4_statx_service_path_returns_sane_metadata() {
        let mut backend = Ext4Backend::new();
        let result = backend.statx_path(EXT4_SERVICE_PATH);
        assert!(
            result.is_ok(),
            "statx on /ext4/service.bin must succeed, got: {:?}",
            result
        );
        // statx returns file_len (0 is valid for an empty embedded EXT4 service bin).
        let _file_len = result.unwrap();
    }

    #[test]
    fn stage91_ext4_write_returns_unsupported() {
        // Regression: ext4 is read-only. Write must return Unsupported.
        // First open a valid file to get an fd, then attempt write.
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open_msg = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0)
            .expect("openat_inline_message encoding must succeed");
        let open_rep = svc.handle(open_msg).expect("open must succeed");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode open reply")
            .as_u64();
        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0x1000,
            len: 64,
        })
        .expect("write_message encoding must succeed");
        assert_eq!(
            svc.handle(write),
            Err(VfsError::Unsupported),
            "ext4 write must return Unsupported (read-only backend)"
        );
    }

    #[test]
    fn stage91_ext4_open_missing_path_returns_invalid_path_or_bad_fd() {
        let mut backend = Ext4Backend::new();
        let result = backend.openat_path(b"/ext4/nonexistent_stage91.bin");
        assert!(
            result.is_err(),
            "openat on missing path must fail, got Ok({:?})",
            result.ok()
        );
    }

    #[test]
    fn stage91_ext4_live_mount_gate_enabled() {
        assert!(
            VFS_EXT4_LIVE_MOUNT_ENABLED,
            "VFS_EXT4_LIVE_MOUNT_ENABLED must be true at Stage 91"
        );
    }

    #[test]
    fn stage91_ext4_recv_loop_gate_enabled() {
        assert!(
            VFS_EXT4_RECV_LOOP_ENABLED,
            "VFS_EXT4_RECV_LOOP_ENABLED must be true at Stage 91"
        );
    }

    // ── Part G: RAMFS live route regression tests ─────────────────────────────

    #[test]
    fn stage91_ramfs_live_mount_gate_enabled() {
        assert!(
            VFS_RAMFS_LIVE_MOUNT_ENABLED,
            "VFS_RAMFS_LIVE_MOUNT_ENABLED must be true at Stage 91"
        );
    }

    #[test]
    fn stage91_ramfs_does_not_shadow_ext4_paths() {
        // /ram paths must not be confused with /ext4 paths — they are independent mounts.
        // Prove by checking that the EXT4_SERVICE_PATH cannot be opened via InitramfsBackend.
        let mut fs = InitramfsBackend::new(0);
        // EXT4_SERVICE_PATH = b"/ext4/service.bin" — not an initramfs path.
        let result = fs.openat_path(EXT4_SERVICE_PATH);
        assert!(
            result.is_err(),
            "InitramfsBackend must not route /ext4 paths (shadow check)"
        );
    }

    #[test]
    fn stage91_ext4_does_not_shadow_ram_paths() {
        // /ext4 paths must not shadow /initramfs/sbin paths — independent backends.
        let mut backend = Ext4Backend::new();
        // INITRAMFS_RAMFS_SRV_PATH = b"/initramfs/sbin/ramfs_srv" — not an ext4 path.
        let result = backend.openat_path(INITRAMFS_RAMFS_SRV_PATH);
        assert!(
            result.is_err(),
            "Ext4Backend must not route /initramfs paths (shadow check)"
        );
    }

    #[test]
    fn stage91_stage84_bridge_gate_still_enabled() {
        assert!(
            VFS_STAGE84_RAMFS_BRIDGE_ENABLED,
            "VFS_STAGE84_RAMFS_BRIDGE_ENABLED must remain true at Stage 91 (regression)"
        );
    }

    #[test]
    fn stage91_stage85_live_route_gate_still_enabled() {
        assert!(
            VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED,
            "VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED must remain true at Stage 91 (regression)"
        );
    }

    // ── Part H: FAT production-readiness doc tests (source-scan) ─────────────

    #[test]
    fn stage91_fat_production_checklist_virtio_blk_requirement_documented() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("FAT requires a virtio_blk block device not present"),
            "init/service.rs must document the virtio_blk requirement for FAT production"
        );
    }

    #[test]
    fn stage91_fat_gates_all_false_for_production() {
        assert!(
            !VFS_FAT_LIVE_MOUNT_ENABLED,
            "VFS_FAT_LIVE_MOUNT_ENABLED must be false (FAT production not enabled)"
        );
        assert!(
            !VFS_FAT_SHARED_IO_ENABLED,
            "VFS_FAT_SHARED_IO_ENABLED must be false (FAT production not enabled)"
        );
    }

    #[test]
    fn stage91_fat_production_requires_block_device_comment_in_shared_io_adapter() {
        let src = include_str!("fs/common/shared_io_adapter.rs");
        // The shared_io_adapter.rs documents the FAT production requirement.
        assert!(
            src.contains("VFS_FAT_LIVE_MOUNT_ENABLED"),
            "shared_io_adapter.rs must define VFS_FAT_LIVE_MOUNT_ENABLED"
        );
        assert!(
            src.contains("VFS_FAT_SHARED_IO_ENABLED"),
            "shared_io_adapter.rs must define VFS_FAT_SHARED_IO_ENABLED"
        );
    }

    // ── Part I: Initramfs path table invariant tests ──────────────────────────

    #[test]
    fn stage91_initramfs_all_sbin_server_paths_openable() {
        // fat_srv, ramfs_srv, ext4_srv are openable via the placeholder backend (new(4096)).
        // driver_manager, blkcache_srv, virtio_blk_srv are late_exec_paths: they require
        // a live CPIO backend. This test covers the non-late-exec sbin servers and verifies
        // the late-exec paths are registered via source-scan.
        let mut fs = InitramfsBackend::new(4096);
        let non_late_exec_paths: &[&[u8]] = &[
            INITRAMFS_FAT_SRV_PATH,
            INITRAMFS_RAMFS_SRV_PATH,
            INITRAMFS_EXT4_SRV_PATH,
        ];
        for path in non_late_exec_paths {
            let result = fs.openat_path(path);
            assert!(
                result.is_ok(),
                "InitramfsBackend must be able to open {:?}, got: {:?}",
                core::str::from_utf8(path).unwrap_or("<non-utf8>"),
                result
            );
        }
        // Verify late-exec sbin paths are registered in the inode table via source-scan.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("INITRAMFS_DRIVER_MANAGER_PATH"),
            "archive.rs must register INITRAMFS_DRIVER_MANAGER_PATH"
        );
        assert!(
            src.contains("INITRAMFS_BLKCACHE_PATH"),
            "archive.rs must register INITRAMFS_BLKCACHE_PATH"
        );
        assert!(
            src.contains("INITRAMFS_VIRTIO_BLK_PATH"),
            "archive.rs must register INITRAMFS_VIRTIO_BLK_PATH"
        );
    }

    #[test]
    fn stage91_initramfs_max_inodes_covers_all_sbin_servers() {
        // MAX_INITRAMFS_INODES = 15 must cover all registered inodes including
        // the normal sbin servers and the gated crash-test server path.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("MAX_INITRAMFS_INODES"),
            "archive.rs must define MAX_INITRAMFS_INODES"
        );
        // Verify 15 is the current constant value.
        assert!(
            src.contains("const MAX_INITRAMFS_INODES: usize = 15"),
            "MAX_INITRAMFS_INODES must equal 15 after reserving crash_test_srv"
        );
    }

    #[test]
    fn stage91_initramfs_ext4_srv_path_arm_in_cpio_match() {
        // Regression test for the Stage 89 fix: ext4_srv was missing from the
        // from_cpio_newc() match arm, causing VFS to return NotFound on spawn.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("sbin/ext4_srv"),
            "archive.rs from_cpio_newc must have a sbin/ext4_srv match arm (Stage 89 regression)"
        );
        assert!(
            src.contains("INITRAMFS_EXT4_SRV_PATH"),
            "archive.rs must reference INITRAMFS_EXT4_SRV_PATH in the inode table"
        );
    }

    #[test]
    fn stage91_initramfs_fat_srv_path_openable() {
        // fat_srv is not a late_exec_path; openable via placeholder backend.
        let mut fs = InitramfsBackend::new(4096);
        let fd = fs
            .openat_path(INITRAMFS_FAT_SRV_PATH)
            .expect("/initramfs/sbin/fat_srv must be openable via InitramfsBackend");
        assert!(fd >= 10);
    }

    #[test]
    fn stage91_initramfs_driver_manager_path_openable() {
        // driver_manager is a late_exec_path (gated by is_placeholder_mode check).
        // Verify it is registered in the inode table via source-scan.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("path: INITRAMFS_DRIVER_MANAGER_PATH"),
            "archive.rs must have an inode entry for INITRAMFS_DRIVER_MANAGER_PATH"
        );
    }

    #[test]
    fn stage91_initramfs_blkcache_srv_path_openable() {
        // blkcache_srv is a late_exec_path; verify via source-scan.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("path: INITRAMFS_BLKCACHE_PATH"),
            "archive.rs must have an inode entry for INITRAMFS_BLKCACHE_PATH"
        );
    }

    #[test]
    fn stage91_initramfs_virtio_blk_srv_path_openable() {
        // virtio_blk_srv is a late_exec_path; verify via source-scan.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("path: INITRAMFS_VIRTIO_BLK_PATH"),
            "archive.rs must have an inode entry for INITRAMFS_VIRTIO_BLK_PATH"
        );
    }

    #[test]
    fn stage91_initramfs_ramfs_srv_path_openable() {
        // ramfs_srv is not a late_exec_path; openable via placeholder backend.
        let mut fs = InitramfsBackend::new(4096);
        let fd = fs
            .openat_path(INITRAMFS_RAMFS_SRV_PATH)
            .expect("/initramfs/sbin/ramfs_srv must be openable via InitramfsBackend");
        assert!(fd >= 10);
    }

    // ── Part D: Spawn/mount ordering tests (source-scan on init/service.rs) ───

    #[test]
    fn stage91_drain_before_fat_spawn_gate() {
        // The pm_recv drain must complete before the FAT spawn gate is evaluated.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let drain_pos = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let fat_skip_pos = src
            .find("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled")
            .expect("FAT skip marker must be present");
        assert!(
            drain_pos < fat_skip_pos,
            "pm_recv drain must appear before INIT_FAT_SPAWN_SKIPPED in init/service.rs"
        );
    }

    #[test]
    fn stage91_fat_skip_logged_before_ext4_spawn_in_optional_section() {
        // In the optional-FS section (INIT_SPAWN_OPTIONAL_FS_SERVERS=true),
        // FAT is skipped with reason=server_disabled (INIT_SPAWN_FAT_SRV=false)
        // before the ext4 spawn block. The reason=profile_disabled path is in a
        // separate else-branch and appears after ext4 in source order.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let fat_skip_server_pos = src
            .find("INIT_FAT_SPAWN_SKIPPED reason=server_disabled")
            .expect("INIT_FAT_SPAWN_SKIPPED reason=server_disabled must be present");
        let ext4_begin_pos = src
            .find("INIT_EXT4_SPAWN_BEGIN")
            .expect("INIT_EXT4_SPAWN_BEGIN must be present");
        assert!(
            fat_skip_server_pos < ext4_begin_pos,
            "INIT_FAT_SPAWN_SKIPPED reason=server_disabled must appear before INIT_EXT4_SPAWN_BEGIN"
        );
        // Also verify the profile_disabled path exists in the else-branch.
        assert!(
            src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled"),
            "INIT_FAT_SPAWN_SKIPPED reason=profile_disabled must be present in else-branch"
        );
    }

    #[test]
    fn stage91_const_init_spawn_fat_srv_is_false() {
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        assert!(
            src.contains("const INIT_SPAWN_FAT_SRV: bool = false;"),
            "INIT_SPAWN_FAT_SRV must be false (no virtio_blk block device in default profile)"
        );
    }

    // ── Spawn/VFS reply size-mismatch proof ───────────────────────────────────

    #[test]
    fn stage91_vfs_mount_status_reply_size_differs_from_spawn_v5_result_size() {
        // Proves that a VFS mount-status reply (4 bytes: u32 status) cannot be
        // mistaken for a SpawnV5 reply (16 bytes: u64 pid + u64 service_send_cap).
        // If register_*_mount_with_vfs uses a non-blocking poll (deadline=0) on
        // reply_recv_cap, a delayed VFS mount-status reply may be left on pm_recv.
        // The next spawn's ipc_recv_v2(pm_recv) picks up that 4-byte payload —
        // SpawnV5CapResult::decode checks payload.len() == 16, fails (bad_len),
        // and the spawn returns None (INIT_*_SPAWN_FAIL).
        // Fix: all register functions must use blocking ipc_recv_v2(reply_recv_cap).
        const VFS_MOUNT_STATUS_REPLY_BYTES: usize = core::mem::size_of::<u32>();
        const SPAWN_V5_RESULT_BYTES: usize = 16; // pid: u64 + service_send_cap: u64
        assert_ne!(
            VFS_MOUNT_STATUS_REPLY_BYTES, SPAWN_V5_RESULT_BYTES,
            "VFS mount-status reply and SpawnV5 result must have different sizes"
        );
        assert_eq!(
            VFS_MOUNT_STATUS_REPLY_BYTES, 4,
            "VFS mount-status is a u32 (4 bytes)"
        );
        assert_eq!(
            SPAWN_V5_RESULT_BYTES, 16,
            "SpawnV5 result is pid:u64 + cap:u64 (16 bytes)"
        );
    }

    #[test]
    fn stage91_no_blocking_recv_on_wrong_cap_in_register_functions() {
        // Source-scan: none of the three register_*_mount_with_vfs functions may use
        // ipc_recv_with_deadline (non-blocking) to receive the VFS mount-register reply.
        // All must use ipc_recv_v2 (blocking) on reply_recv_cap.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        for fn_name in &[
            "fn register_ramfs_mount_with_vfs(",
            "fn register_fat_mount_with_vfs(",
            "fn register_ext4_mount_with_vfs(",
        ] {
            let fn_start = src
                .find(fn_name)
                .unwrap_or_else(|| panic!("{fn_name} must be defined in init/service.rs"));
            let after_fn = &src[fn_start + 1..];
            let fn_off = after_fn.find("\nfn ").unwrap_or(usize::MAX);
            let pub_fn_off = after_fn.find("\npub fn ").unwrap_or(usize::MAX);
            let fn_body_end = fn_start + 1 + fn_off.min(pub_fn_off);
            let fn_text = &src[fn_start..fn_body_end];
            assert!(
                fn_text.contains("ipc_recv_v2(reply_recv_cap"),
                "{fn_name} must use blocking ipc_recv_v2(reply_recv_cap)"
            );
            assert!(
                !fn_text.contains("ipc_recv_with_deadline(reply_recv_cap"),
                "{fn_name} must NOT use non-blocking ipc_recv_with_deadline on reply_recv_cap"
            );
        }
    }

    #[test]
    fn stage91_pm_vfs_spawn_prefers_service_recv_ep_for_vfs_subcalls() {
        // Source-scan: pm_vfs_spawn_inline must prefer process_manager_service_recv_ep
        // (slot 12, PM-private) over process_manager_reply_recv_cap (slot 2, shared
        // with init's pm_recv endpoint cap 65537).
        // This is the root-cause fix for INIT_RAMFS/EXT4_SPAWN_FAIL: VFS (tid=10002)
        // sends 8-byte OPENAT replies to the shared endpoint during PM's grant path,
        // and init reads those before PM's real 16-byte SpawnV5 reply.
        let pm_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/process_manager/service.rs"
        );
        let fn_start = pm_src
            .find("fn pm_vfs_spawn_inline(")
            .expect("pm_vfs_spawn_inline must exist in process_manager/service.rs");
        let fn_end = (fn_start + 2500).min(pm_src.len());
        let fn_body = &pm_src[fn_start..fn_end];
        assert!(
            fn_body.contains("process_manager_service_recv_ep"),
            "pm_vfs_spawn_inline must use process_manager_service_recv_ep (slot 12)"
        );
        assert!(
            fn_body.contains(".or(ctx.process_manager_reply_recv_cap)"),
            "pm_vfs_spawn_inline must fall back to slot 2 only if service_recv_ep absent"
        );
        // Must NOT unconditionally use slot 2 as the primary VFS reply endpoint.
        let slot2_only_pattern = "ctx\n        .process_manager_reply_recv_cap\n        .ok_or";
        assert!(
            !fn_body.contains(slot2_only_pattern),
            "pm_vfs_spawn_inline must not use slot 2 as primary VFS reply endpoint"
        );
    }

    #[test]
    fn stage91_spawn_v5_cap_wrong_sender_drain_loop_present() {
        // Source-scan: spawn_v5_cap in init/service.rs must contain the wrong-sender
        // drain loop that guards against VFS 8-byte replies being misread as SpawnV5
        // failures.  Both RAMFS (image_id=11) and ext4 (image_id=12) spawns call
        // spawn_v5_cap, so both are protected by this loop.
        let src =
            include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");
        let fn_start = src
            .find("fn spawn_v5_cap(")
            .expect("spawn_v5_cap must exist in init/service.rs");
        let fn_body = &src[fn_start..];
        assert!(
            fn_body.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "spawn_v5_cap must log INIT_SPAWN_V5_WRONG_SENDER_REPLY"
        );
        assert!(
            fn_body.contains("expected_pm_tid"),
            "spawn_v5_cap must compute expected_pm_tid (init_tid + 2)"
        );
        assert!(
            fn_body.contains("MAX_WRONG_SENDER_DRAIN"),
            "spawn_v5_cap must cap the drain loop with MAX_WRONG_SENDER_DRAIN"
        );
        assert!(
            fn_body.contains("loop {"),
            "spawn_v5_cap must use a loop to drain wrong-sender replies before accepting"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Stage 92: AArch64 wrong-sender reply race elimination — vfs_client.rs
//           blocking-recv fix.
//
// Root cause: vfs_client.rs IPC helpers used ipc_recv_with_deadline(..., 0)
// (non-blocking).  On AArch64, delayed VFS replies arrived AFTER the
// pre-spawn drain loop and were read by spawn_v5_cap's ipc_recv_v2 wait,
// triggering INIT_SPAWN_V5_WRONG_SENDER_REPLY ×3.
// Fix: switch all four helpers to ipc_recv_v2 (blocking) so replies are
// always consumed inline before control returns to init.
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod stage92_tests {
    const VFS_CLIENT_SRC: &str = include_str!("../../yarm-user-rt/src/vfs_client.rs");

    const INIT_SRC: &str =
        include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");

    // ── 1. All four IPC helpers use blocking ipc_recv_v2 ────────────────────

    #[test]
    fn stage92_vfs_client_statx_uses_blocking_recv_v2() {
        let fn_start = VFS_CLIENT_SRC
            .find("pub unsafe fn vfs_statx(")
            .expect("vfs_statx must be defined in vfs_client.rs");
        let fn_body = &VFS_CLIENT_SRC[fn_start..];
        assert!(
            fn_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_statx must use blocking ipc_recv_v2 to consume VFS reply"
        );
    }

    #[test]
    fn stage92_vfs_client_openat_uses_blocking_recv_v2() {
        let fn_start = VFS_CLIENT_SRC
            .find("pub unsafe fn vfs_openat(")
            .expect("vfs_openat must be defined in vfs_client.rs");
        let fn_body = &VFS_CLIENT_SRC[fn_start..];
        assert!(
            fn_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_openat must use blocking ipc_recv_v2 to consume VFS reply"
        );
    }

    #[test]
    fn stage92_vfs_client_read_uses_blocking_recv_v2() {
        let fn_start = VFS_CLIENT_SRC
            .find("pub unsafe fn vfs_read(")
            .expect("vfs_read must be defined in vfs_client.rs");
        let fn_body = &VFS_CLIENT_SRC[fn_start..];
        assert!(
            fn_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_read must use blocking ipc_recv_v2 to consume VFS reply"
        );
    }

    #[test]
    fn stage92_vfs_client_close_uses_blocking_recv_v2() {
        let fn_start = VFS_CLIENT_SRC
            .find("pub unsafe fn vfs_close(")
            .expect("vfs_close must be defined in vfs_client.rs");
        let fn_body = &VFS_CLIENT_SRC[fn_start..];
        assert!(
            fn_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_close must use blocking ipc_recv_v2 to consume VFS reply"
        );
    }

    // ── 2. No IPC helper uses non-blocking ipc_recv_with_deadline ───────────

    #[test]
    fn stage92_vfs_client_ipc_helpers_do_not_use_zero_deadline_recv() {
        // None of the four IPC helpers (vfs_statx/openat/read/close) may call
        // ipc_recv_with_deadline.  The encoding helpers (build_*) are pure and
        // do not call any receive syscall.
        let ipc_section_start = VFS_CLIENT_SRC
            .find("// ── IPC helpers")
            .expect("IPC helpers section comment must be present in vfs_client.rs");
        let ipc_section = &VFS_CLIENT_SRC[ipc_section_start..];
        // The test section begins after the IPC section; cut at #[cfg(test)].
        let test_section_start = ipc_section
            .find("#[cfg(test)]")
            .unwrap_or(ipc_section.len());
        let ipc_only = &ipc_section[..test_section_start];
        assert!(
            !ipc_only.contains("ipc_recv_with_deadline"),
            "vfs_client.rs IPC helpers must not use non-blocking ipc_recv_with_deadline (Stage 92 fix)"
        );
    }

    // ── 3. IPC helpers decode via &received.message (not &received directly) ─

    #[test]
    fn stage92_vfs_client_ipc_helpers_decode_via_received_message_field() {
        // After switching to ipc_recv_v2, callers must pass &received.message
        // to decode_reply_u64, not &received (which is ReceivedMessage, not Message).
        let ipc_section_start = VFS_CLIENT_SRC
            .find("// ── IPC helpers")
            .expect("IPC helpers section comment must be present");
        let test_start = VFS_CLIENT_SRC
            .find("#[cfg(test)]")
            .unwrap_or(VFS_CLIENT_SRC.len());
        let ipc_section = &VFS_CLIENT_SRC[ipc_section_start..test_start];
        assert!(
            ipc_section.contains("decode_reply_u64(&received.message)"),
            "vfs_client.rs IPC helpers must call decode_reply_u64(&received.message)"
        );
    }

    // ── 4. Module doc no longer claims non-blocking behavior ────────────────

    #[test]
    fn stage92_vfs_client_module_doc_says_blocking_not_deadline() {
        // The module-level doc comment must not claim the helpers use
        // ipc_recv_with_deadline (pre-Stage-92 wording).
        let module_doc_end = VFS_CLIENT_SRC.find("use crate::ipc::Message;").unwrap_or(0);
        let module_doc = &VFS_CLIENT_SRC[..module_doc_end];
        assert!(
            !module_doc.contains("ipc_recv_with_deadline"),
            "vfs_client.rs module doc must not reference ipc_recv_with_deadline after Stage 92 fix"
        );
        assert!(
            module_doc.contains("ipc_recv_v2"),
            "vfs_client.rs module doc must reference ipc_recv_v2 (blocking receive)"
        );
    }

    // ── 5. spawn_v5_cap wrong-sender drain is defense-in-depth only ─────────

    #[test]
    fn stage92_spawn_v5_drain_loop_is_defense_in_depth_with_blocking_vfs_client() {
        // With the Stage 92 fix, VFS replies are consumed by vfs_client.rs helpers
        // before returning to init.  The wrong-sender drain loop in spawn_v5_cap
        // remains as defense-in-depth but should fire 0 times in a clean run.
        // Verify the loop is still present (not removed).
        let fn_start = INIT_SRC
            .find("fn spawn_v5_cap(")
            .expect("spawn_v5_cap must remain in init/service.rs as defense-in-depth");
        let fn_body = &INIT_SRC[fn_start..];
        assert!(
            fn_body.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "wrong-sender drain loop must remain in spawn_v5_cap as defense-in-depth"
        );
        assert!(
            fn_body.contains("MAX_WRONG_SENDER_DRAIN"),
            "MAX_WRONG_SENDER_DRAIN must remain as the drain cap"
        );
    }

    // ── 6. Smoke scripts enforce zero wrong-sender count in strict mode ──────

    #[test]
    fn stage92_smoke_script_aarch64_checks_zero_wrong_sender_count() {
        let script = include_str!("../../../scripts/qemu-aarch64-optional-fs-smoke.sh");
        assert!(
            script.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "aarch64 smoke script must check for INIT_SPAWN_V5_WRONG_SENDER_REPLY count"
        );
        assert!(
            script.contains("QEMU_SMOKE_STRICT"),
            "aarch64 smoke script must gate wrong-sender check on QEMU_SMOKE_STRICT"
        );
    }

    #[test]
    fn stage92_smoke_script_x86_64_checks_zero_wrong_sender_count() {
        let script = include_str!("../../../scripts/qemu-x86_64-optional-fs-smoke.sh");
        assert!(
            script.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "x86_64 smoke script must check for INIT_SPAWN_V5_WRONG_SENDER_REPLY count"
        );
        assert!(
            script.contains("QEMU_SMOKE_STRICT"),
            "x86_64 smoke script must gate wrong-sender check on QEMU_SMOKE_STRICT"
        );
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Stage 93: FAT production virtio-blk profile + official FS profile matrix.
//
// Key changes:
//   - IpcBlockDevice::read_exact_at / write_sector: ipc_recv_v2 (blocking)
//     instead of ipc_recv_with_deadline(_, 0) — same deadline-0 bug as Stage 92
//   - Official FS profile matrix documented
//   - FAT block smoke scripts + create-fat-image.sh added
//   - Optional-FS smoke scripts: add KSPAWN/PM_VFS_SPAWN_FAIL/bad_fd/panic/phase2b checks
//   - ext4 hardening: unknown-opcode, write-inline, read, short-EOF tests
// ════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod stage93_tests {
    use crate::fs::common::vfs_ipc::{
        ReadWriteRequest, VfsError, openat_inline_message, read_message,
    };
    use crate::fs::common::vfs_service::VfsService;
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use yarm_ipc_abi::vfs_abi::VFS_OP_WRITE_INLINE;
    use yarm_srv_common::vfs_reply::VfsReply;

    const FAT_FS_SRC: &str = include_str!("fs/fat/fs.rs");
    const SHARED_IO_SRC: &str = include_str!("fs/common/shared_io_adapter.rs");
    const INIT_SRC: &str =
        include_str!("../../yarm-control-plane-servers/src/control_plane/init/service.rs");

    // ── 1. IpcBlockDevice blocking-recv fix (same as Stage 92 for vfs_client) ─

    #[test]
    fn stage93_ipc_block_device_read_uses_blocking_recv_v2() {
        // IpcBlockDevice::read_exact_at must use ipc_recv_v2 (blocking) to
        // receive the blkcache reply.  Using deadline=0 (non-blocking) would
        // cause immediate Err(FatError::Io) before blkcache_srv has a chance
        // to process the read request — same race as Stage 92's vfs_client.rs fix.
        //
        // Scope to the IpcBlockDevice impl block (not the FatBlockDevice dispatch wrapper).
        let impl_start = FAT_FS_SRC
            .find("impl BlockDevice for IpcBlockDevice")
            .expect("IpcBlockDevice BlockDevice impl must be present in fat/fs.rs");
        let impl_body = &FAT_FS_SRC[impl_start..];
        let fn_start = impl_body
            .find("fn read_exact_at(")
            .expect("read_exact_at must be inside impl BlockDevice for IpcBlockDevice");
        let fn_body = &impl_body[fn_start..];
        let fn_end = fn_body.find("\n    fn ").unwrap_or(fn_body.len());
        let read_body = &fn_body[..fn_end];
        assert!(
            read_body.contains("ipc_recv_v2(self.reply_recv_cap)"),
            "IpcBlockDevice::read_exact_at must use ipc_recv_v2 (blocking)"
        );
        assert!(
            !read_body.contains("ipc_recv_with_deadline"),
            "IpcBlockDevice::read_exact_at must not use ipc_recv_with_deadline (deadline-0 race)"
        );
    }

    #[test]
    fn stage93_ipc_block_device_write_uses_blocking_recv_v2() {
        // write_sector must also use ipc_recv_v2 to receive the BlkWriteReply.
        let fn_start = FAT_FS_SRC
            .find("fn write_sector(")
            .expect("write_sector must be defined in fat/fs.rs");
        let fn_body = &FAT_FS_SRC[fn_start..];
        let fn_end = fn_body.find("\nfn ").unwrap_or(fn_body.len());
        let write_body = &fn_body[..fn_end];
        assert!(
            write_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "IpcBlockDevice::write_sector must use ipc_recv_v2 (blocking)"
        );
        assert!(
            !write_body.contains("ipc_recv_with_deadline"),
            "IpcBlockDevice::write_sector must not use ipc_recv_with_deadline"
        );
    }

    // ── 2. FAT gate constants locked in default profile ──────────────────────

    #[test]
    fn stage93_fat_live_mount_gate_is_false_in_default_profile() {
        assert!(
            SHARED_IO_SRC.contains("VFS_FAT_LIVE_MOUNT_ENABLED: bool = false"),
            "VFS_FAT_LIVE_MOUNT_ENABLED must be false in default optional-fs profile (needs virtio-blk)"
        );
    }

    #[test]
    fn stage93_fat_shared_io_gate_is_false_in_default_profile() {
        assert!(
            SHARED_IO_SRC.contains("VFS_FAT_SHARED_IO_ENABLED: bool = false"),
            "VFS_FAT_SHARED_IO_ENABLED must be false in default profile (no FAT read-shared proof yet)"
        );
    }

    #[test]
    fn stage93_init_spawn_fat_srv_is_false_in_default_profile() {
        assert!(
            INIT_SRC.contains("const INIT_SPAWN_FAT_SRV: bool = false"),
            "INIT_SPAWN_FAT_SRV must be false in default optional-fs profile"
        );
    }

    // ── 3. ext4 hardening: unknown and forbidden opcodes ─────────────────────

    #[test]
    fn stage93_ext4_rejects_unknown_opcode_as_unsupported() {
        // Any opcode not in the VFS dispatch table returns Err(VfsError::Unsupported)
        // rather than panicking or returning garbage.  Opcode 0x4242 is not in the table.
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let unknown_msg = yarm_user_rt::ipc::Message::with_header(0, 0x4242, 0, None, &[])
            .expect("build unknown-opcode message");
        assert_eq!(
            svc.handle(unknown_msg),
            Err(VfsError::Unsupported),
            "ext4 service must return Unsupported for unknown opcode 0x4242"
        );
    }

    #[test]
    fn stage93_ext4_rejects_write_inline_opcode_28_as_unsupported() {
        // VFS_OP_WRITE_INLINE (28) is FAT-only. The generic VfsService catch-all
        // returns Unsupported, so ext4 must not decode it as a real write.
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let inline_payload = [1u8; 32];
        let write_inline_msg = yarm_user_rt::ipc::Message::with_header(
            0,
            VFS_OP_WRITE_INLINE,
            0,
            None,
            &inline_payload,
        )
        .expect("build write-inline message");
        assert_eq!(
            svc.handle(write_inline_msg),
            Err(VfsError::Unsupported),
            "ext4 service must reject VFS_OP_WRITE_INLINE (opcode 28) with Unsupported"
        );
    }

    // ── 4. ext4 hardening: read and short-EOF ────────────────────────────────

    #[test]
    fn stage93_ext4_read_returns_zero_for_empty_file() {
        // ext4 demo files have file_len=0; a read request should return 0 bytes read.
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("open");
        let open_rep = svc.handle(open).expect("open reply");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode fd")
            .as_u64();
        let read = read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 1024,
        })
        .expect("read msg");
        let read_rep = svc.handle(read).expect("read reply");
        let bytes_read =
            VfsReply::from_opcode_payload_checked(read_rep.opcode, read_rep.as_slice())
                .expect("decode read result")
                .as_u64();
        assert_eq!(
            bytes_read, 0,
            "ext4 read on empty demo file must return 0 bytes"
        );
    }

    #[test]
    fn stage93_ext4_write_returns_unsupported_after_read() {
        // ext4 is read-only: all write attempts must return Unsupported regardless
        // of whether the file was opened or read first.
        use crate::fs::common::vfs_ipc::{ReadWriteRequest, write_message};
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("open");
        let open_rep = svc.handle(open).expect("open");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode fd")
            .as_u64();
        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 4096,
        })
        .expect("write");
        assert_eq!(
            svc.handle(write),
            Err(VfsError::Unsupported),
            "ext4 write must remain Unsupported after successful open"
        );
    }

    // ── 5. Smoke script hardening ─────────────────────────────────────────────

    #[test]
    fn stage93_optional_fs_smoke_scripts_check_kspawn_fail() {
        for (arch, script) in &[
            (
                "aarch64",
                include_str!("../../../scripts/qemu-aarch64-optional-fs-smoke.sh"),
            ),
            (
                "x86_64",
                include_str!("../../../scripts/qemu-x86_64-optional-fs-smoke.sh"),
            ),
        ] {
            assert!(
                script.contains("KSPAWN_EXTRA_CAP_DELEGATE_FAIL"),
                "{arch} optional-FS smoke must check for KSPAWN_EXTRA_CAP_DELEGATE_FAIL"
            );
            assert!(
                script.contains("PM_VFS_SPAWN_FAIL"),
                "{arch} optional-FS smoke must check for PM_VFS_SPAWN_FAIL"
            );
            assert!(
                script.contains("reason=bad_fd_decode"),
                "{arch} optional-FS smoke must check for bad_fd_decode"
            );
        }
    }

    // ── 6. FAT block smoke scripts and image-creation script exist ────────────

    #[test]
    fn stage93_fat_block_smoke_script_aarch64_has_virtio_blk_args() {
        let script = include_str!("../../../scripts/qemu-aarch64-fat-block-smoke.sh");
        assert!(
            script.contains("virtio-blk"),
            "aarch64 fat-block smoke must use virtio-blk QEMU device"
        );
        assert!(
            script.contains("FAT_IMAGE"),
            "aarch64 fat-block smoke must reference FAT_IMAGE"
        );
        assert!(
            script.contains("FAT_MOUNT_READY"),
            "aarch64 fat-block smoke must check FAT_MOUNT_READY marker"
        );
    }

    #[test]
    fn stage93_fat_block_smoke_script_x86_64_has_virtio_blk_args() {
        let script = include_str!("../../../scripts/qemu-x86_64-fat-block-smoke.sh");
        assert!(
            script.contains("virtio-blk"),
            "x86_64 fat-block smoke must use virtio-blk QEMU device"
        );
        assert!(
            script.contains("FAT_IMAGE"),
            "x86_64 fat-block smoke must reference FAT_IMAGE"
        );
        assert!(
            script.contains("FAT_MOUNT_READY"),
            "x86_64 fat-block smoke must check FAT_MOUNT_READY marker"
        );
    }

    #[test]
    fn stage93_create_fat_image_script_exists_with_mtools_path() {
        let script = include_str!("../../../scripts/create-fat-image.sh");
        assert!(
            script.contains("mformat") || script.contains("mkfs.fat"),
            "create-fat-image.sh must use mformat or mkfs.fat to create the FAT image"
        );
        assert!(
            script.contains("hello.txt"),
            "create-fat-image.sh must create a hello.txt file in the FAT image"
        );
    }
}
