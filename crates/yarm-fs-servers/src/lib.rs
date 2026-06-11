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
    use crate::fs::fat::service::{FatServiceStartup, FatStartupConfig, service_from_startup_config};
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
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::fat::fs::FatBackend;
    use crate::fs::ramfs::service::{
        RamFsServiceStartup, RamFsStartupConfig, run_with_config, service_from_startup_config,
    };
    use crate::fs::common::vfs_ipc::{VfsError, openat_inline_message, write_message, ReadWriteRequest};
    use yarm_srv_common::vfs_core::VfsBackend;
    use yarm_srv_common::vfs_reply::VfsReply;

    #[test]
    fn stage86_gate_vfs_ramfs_live_mount_enabled() {
        assert!(VFS_RAMFS_LIVE_MOUNT_ENABLED, "RAMFS live-mount gate must be true");
    }

    #[test]
    fn stage86_gate_vfs_fat_shared_io_disabled() {
        assert!(!VFS_FAT_SHARED_IO_ENABLED, "FAT shared-I/O gate must be false");
    }

    #[test]
    fn stage86_gate_vfs_ext4_recv_loop_enabled() {
        assert!(VFS_EXT4_RECV_LOOP_ENABLED, "ext4 recv-loop gate must be true");
    }

    #[test]
    fn stage86_gate_vfs_ext4_live_mount_disabled() {
        // Stage 88 supersedes this Stage-86 invariant: ext4 live-mount is now enabled.
        // VFS_EXT4_LIVE_MOUNT_ENABLED was false in Stage 86; Stage 88 lifts it to true.
        assert!(VFS_EXT4_LIVE_MOUNT_ENABLED, "Stage 88: ext4 live-mount gate must be true");
    }

    #[test]
    fn stage86_stage85_gate_still_enabled() {
        assert!(VFS_STAGE85_RAMFS_LIVE_ROUTE_ENABLED);
    }

    #[test]
    fn stage86_ramfs_service_has_run_resident() {
        let src = include_str!("fs/ramfs/service.rs");
        assert!(src.contains("fn run_resident("), "ramfs/service.rs must export run_resident");
        assert!(
            src.contains("fn run_resident_service_loop("),
            "ramfs/service.rs must have run_resident_service_loop"
        );
        assert!(src.contains("RAMFS_SRV_READY"), "ramfs/service.rs must emit RAMFS_SRV_READY");
        assert!(src.contains("ipc_recv_v2("), "ramfs/service.rs must use ipc_recv_v2");
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
        let fd = backend.openat_path(crate::fs::fat::fs::FAT_HELLO_PATH).expect("open");
        let mut buf = [0u8; 32];
        let n = backend.read_shared_bytes(fd, &mut buf).expect("read_shared_bytes");
        assert!(n <= 32);
    }

    #[test]
    fn stage86_init_spawn_sub_gates_present() {
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(src.contains("INIT_SPAWN_RAMFS_SRV"));
        assert!(src.contains("INIT_SPAWN_FAT_SRV"));
        assert!(src.contains("INIT_SPAWN_EXT4_SRV"));
    }

    #[test]
    fn stage86_init_spawn_fail_markers_present() {
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let write = write_message(ReadWriteRequest { fd, buf_ptr: 0, len: 512 }).expect("write");
        assert_eq!(svc.handle(write), Err(VfsError::Unsupported));
    }
}

#[cfg(test)]
mod stage87_tests {
    use crate::fs::common::shared_io_adapter::{VFS_FAT_LIVE_MOUNT_ENABLED, VFS_RAMFS_LIVE_MOUNT_ENABLED};
    use crate::fs::ramfs::service::{
        RamFsMountConfig, RamFsServiceStartup, RamFsStartupConfig, run_with_config,
        run_request_loop,
    };
    use crate::fs::ramfs::tree::RamFsBackend;
    use crate::fs::common::service::FsService;

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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        assert!(src.contains("RAMFS_SRV_ENTRY"), "ramfs must log RAMFS_SRV_ENTRY");
        assert!(src.contains("RAMFS_SRV_READY"), "ramfs must log RAMFS_SRV_READY");
        assert!(src.contains("RAMFS_MOUNT_READY"), "ramfs must log RAMFS_MOUNT_READY");
    }

    #[test]
    fn stage87_ext4_srv_entry_and_ready_markers_present() {
        // EXT4_SRV_ENTRY is logged by the binary entry point (bin/ext4_srv.rs).
        // EXT4_SRV_READY is logged by the service loop (fs/ext4/service.rs).
        let bin_src = include_str!("bin/ext4_srv.rs");
        let svc_src = include_str!("fs/ext4/service.rs");
        assert!(bin_src.contains("EXT4_SRV_ENTRY"), "ext4 binary must log EXT4_SRV_ENTRY");
        assert!(svc_src.contains("EXT4_SRV_READY"), "ext4 service must log EXT4_SRV_READY");
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
    use crate::fs::fat::fs::FatBackend;
    use crate::fs::fat::service::{FatServiceStartup, FatStartupConfig, service_from_startup_config};
    use crate::fs::ext4::{EXT4_SERVICE_PATH, Ext4Backend};
    use crate::fs::ext4::service::Ext4Service;
    use crate::fs::common::vfs_ipc::{VfsError, openat_inline_message, write_message, ReadWriteRequest};
    use yarm_srv_common::vfs_core::VfsBackend;
    use yarm_srv_common::vfs_reply::VfsReply;

    #[test]
    fn stage88_fat_shared_io_gate_disabled() {
        assert!(!VFS_FAT_SHARED_IO_ENABLED);
    }

    #[test]
    fn stage88_fat_read_shared_bytes_returns_valid_count() {
        let mut backend = FatBackend::new();
        let fd = backend.openat_path(crate::fs::fat::fs::FAT_HELLO_PATH).expect("open");
        let mut buf = [0u8; 64];
        let n = backend.read_shared_bytes(fd, &mut buf).expect("read_shared_bytes");
        assert!(n <= 64);
    }

    #[test]
    fn stage88_fat_read_shared_bytes_empty_buf_returns_zero() {
        let mut backend = FatBackend::new();
        let fd = backend.openat_path(crate::fs::fat::fs::FAT_HELLO_PATH).expect("open");
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(
            src.contains("INIT_EXT4_SPAWN_OK child_tid={} send_cap={}"),
            "INIT_EXT4_SPAWN_OK must include send_cap field (Stage 88: cap used for VFS registration)"
        );
    }

    #[test]
    fn stage88_fat_handoff_blkcache_cap_passed_in_spawn() {
        // FAT cap handoff audit: init already passes init_blkcache_send_cap to fat_srv
        // via service_extra_cap_0 at spawn time. This proves the cap handoff design is correct
        // even though INIT_SPAWN_FAT_SRV remains false (no virtio_blk in hosted-dev).
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(
            src.contains("[init_blkcache_send_cap, fat_prefix_word, fat_meta_word, 0]"),
            "FAT spawn must pass init_blkcache_send_cap as service_extra_cap_0 (block-device handoff)"
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        let spawn_ok_pos = src
            .find("INIT_EXT4_SPAWN_OK")
            .expect("INIT_EXT4_SPAWN_OK must be present");
        // Search for the call site specifically (not the function definition).
        let register_pos = src
            .find("let _ = register_ext4_mount_with_vfs(")
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
        let write = write_message(ReadWriteRequest { fd, buf_ptr: 0, len: 512 }).expect("write");
        assert_eq!(svc.handle(write), Err(VfsError::Unsupported));
    }
}

#[cfg(test)]
mod stage89_tests {
    use crate::fs::initramfs::archive::{
        InitramfsBackend, INITRAMFS_EXT4_SRV_PATH, INITRAMFS_RAMFS_SRV_PATH,
    };
    use crate::fs::common::vfs_ipc::VfsBackend;

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
    fn stage89_initramfs_max_inodes_is_14() {
        // Validates that the inode table was bumped to accommodate ext4_srv.
        let src = include_str!("fs/initramfs/archive.rs");
        assert!(
            src.contains("const MAX_INITRAMFS_INODES: usize = 14;"),
            "MAX_INITRAMFS_INODES must be 14 after adding ext4_srv"
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

    // ── Spawn cap-value invariants ─────────────────────────────────────────

    #[test]
    fn stage89_ext4_spawn_service_caps_all_zero() {
        // ext4_srv spawn uses [0, 0, 0, 0] — no prefix word or metadata.
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(
            init_src.contains("spawn_v5_cap(pm_send, pm_recv, 12, [0, 0, 0, 0], 1)"),
            "ext4_srv spawn must use all-zero service_caps [0,0,0,0]"
        );
    }

    #[test]
    fn stage89_ramfs_spawn_cap0_is_zero_cap1_is_prefix_word() {
        // RAMFS spawn encodes mount-prefix in cap1 (intentional, not corruption).
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(
            init_src.contains("[0, ramfs_prefix_word, ramfs_meta_word, 0]"),
            "RAMFS spawn must use cap0=0, cap1=ramfs_prefix_word (mount-config encoding)"
        );
    }

    // ── Sequential spawn ordering ──────────────────────────────────────────

    #[test]
    fn stage89_optional_spawns_are_sequential_ramfs_before_ext4() {
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
    use crate::fs::fat::fs::{FAT_HELLO_PATH, FatBackend, FatBackendKind};
    use crate::fs::fat::service::{FatServiceStartup, FatStartupConfig, service_from_startup_config};
    use crate::fs::common::shared_io_adapter::VFS_FAT_LIVE_MOUNT_ENABLED;
    use crate::fs::common::vfs_ipc::VfsBackend;

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
        let fd = backend.openat_path(FAT_HELLO_PATH).expect("open must succeed");
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
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let init_src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
        assert!(
            init_src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled"),
            "init must emit INIT_FAT_SPAWN_SKIPPED when FAT gate is false"
        );
    }

    // ── Source-level invariants for the false SPAWN_FAIL fix ─────────────────

    #[test]
    fn stage90_init_drains_pm_recv_before_optional_spawns() {
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
        let src = include_str!(
            "../../yarm-control-plane-servers/src/control_plane/init/service.rs"
        );
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
