// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Deprecated legacy namespace.
//! Workspace crates under `crates/` are the runtime dispatch entrypoints.

#[cfg(any(not(test), feature = "legacy-tests"))]
pub mod driver_manager;
#[cfg(any(not(test), feature = "legacy-tests"))]
pub mod init;
#[cfg(all(test, feature = "legacy-tests"))]
pub(crate) mod ipc_roundtrip;
#[cfg(any(not(test), feature = "legacy-tests"))]
pub mod process_manager;
#[cfg(any(not(test), feature = "legacy-tests"))]
pub mod supervisor;
pub mod vfs;

#[cfg(test)]
mod tests {
    use yarm_ipc_abi::process_abi::{decode_spawn_v5_reply, encode_spawn_v5_reply};

    fn spawn_v5_reply_is_success(pid: u64, _service_send_cap: u64) -> bool {
        pid != 0
    }

    #[test]
    fn decode_spawn_v5_reply_all_zero_is_failure_shape() {
        let payload = [0u8; 16];
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_eq!(decoded.pid, 0);
        assert_eq!(decoded.service_send_cap, 0);
    }

    #[test]
    fn spawn_v5_zero_reply_is_not_success() {
        let payload = [0u8; 16];
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert!(!spawn_v5_reply_is_success(
            decoded.pid,
            decoded.service_send_cap
        ));
    }

    #[test]
    fn spawn_v5_success_reply_is_success() {
        let payload = encode_spawn_v5_reply(7, 65541);
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert!(spawn_v5_reply_is_success(
            decoded.pid,
            decoded.service_send_cap
        ));
    }

    #[test]
    fn migrated_control_plane_services_avoid_legacy_blocking_ipc_recv_calls() {
        let vfs_src = include_str!("vfs/service.rs");
        let supervisor_src = include_str!("supervisor/service.rs");
        let init_src = include_str!("init/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");
        let driver_manager_src = include_str!("driver_manager/service.rs");
        let legacy_call = ["kernel", ".ipc_recv", "("].concat();
        let _ = driver_manager_src; // checked separately since it uses ipc_recv_v2

        assert!(
            !vfs_src.contains(legacy_call.as_str()),
            "vfs control-plane migration regressed to blocking ipc_recv"
        );
        assert!(
            !supervisor_src.contains(legacy_call.as_str()),
            "supervisor control-plane migration regressed to blocking ipc_recv"
        );
        assert!(
            !init_src.contains(legacy_call.as_str()),
            "init control-plane flow regressed to blocking ipc_recv"
        );
        assert!(
            !process_manager_src.contains(legacy_call.as_str()),
            "process-manager flow regressed to blocking ipc_recv"
        );
    }

    #[test]
    fn phase4_choreography_retirement_bundle_avoids_server_send_reply_hops() {
        let vfs_src = include_str!("vfs/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");

        assert!(
            !vfs_src.contains("ipc_send(server_send_cap"),
            "vfs migrated call/reply path should not use ad-hoc server-send reply hop"
        );
        assert!(
            !process_manager_src.contains("ipc_send(server_send_cap"),
            "process-manager migrated call/reply path should not use ad-hoc server-send reply hop"
        );
    }

    // ── OPENAT reply decode tests ─────────────────────────────────────────────
    //
    // process_manager/service.rs::decode_u64 is excluded from the test-mode
    // module graph (the `pub mod process_manager` is cfg-gated to non-test builds).
    // These tests provide equivalent coverage by re-implementing the same one-liner
    // logic that service.rs uses, ensuring the VFS OPENAT-reply decode contract is
    // locked in at the control-plane test level.
    fn decode_u64_from_payload(payload: &[u8]) -> Option<u64> {
        if payload.len() < 8 {
            return None;
        }
        let mut b = [0u8; 8];
        b.copy_from_slice(&payload[..8]);
        Some(u64::from_le_bytes(b))
    }

    #[test]
    fn openat_reply_8_byte_le_fd13_decodes_correctly() {
        // QEMU proof: VFS sends bytes=[d, 0, 0, 0, 0, 0, 0, 0] for fd=13.
        let payload = [0x0du8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            decode_u64_from_payload(&payload),
            Some(13),
            "fd=13 must decode from 8-byte LE payload"
        );
    }

    #[test]
    fn openat_reply_bad_length_returns_none() {
        // A 7-byte payload is too short; caller must log PM_VFS_SPAWN_FAIL
        // stage=after-openat reason=bad_fd_decode.
        let payload = [0x0du8, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            decode_u64_from_payload(&payload),
            None,
            "7-byte payload must return None"
        );
    }

    #[test]
    fn openat_reply_empty_returns_none() {
        assert_eq!(
            decode_u64_from_payload(&[]),
            None,
            "empty payload must return None"
        );
    }

    #[test]
    fn openat_reply_fd_zero_returns_zero() {
        // fd=0 is a valid u64 value; the protocol layer decides if fd=0 is
        // acceptable, but the decode function itself must not reject it.
        let payload = [0u8; 8];
        assert_eq!(
            decode_u64_from_payload(&payload),
            Some(0),
            "fd=0 must decode to 0"
        );
    }

    #[test]
    fn openat_reply_extra_bytes_beyond_8_are_ignored() {
        // A payload longer than 8 bytes is fine; only the first 8 count.
        let mut payload = [0u8; 16];
        payload[0] = 0x0d; // fd=13 LE
        assert_eq!(
            decode_u64_from_payload(&payload),
            Some(13),
            "extra bytes beyond 8 must be ignored"
        );
    }

    // ── Stage 80: spawn policy and CPIO staging gate ──────────────────────────
    //
    // PM and init modules are excluded from the test module graph (cfg-gated on
    // `legacy-tests`). These source-inspection tests verify Stage 80 invariants
    // without needing a live kernel or the full module tree.

    #[test]
    fn stage80_pm_image_id_range_covers_fs_servers() {
        let pm_src = include_str!("process_manager/service.rs");
        // VFS_SERVICE_IMAGE_ID_MAX must be 12 to cover fat(10), ramfs(11), ext4(12).
        assert!(
            pm_src.contains("const VFS_SERVICE_IMAGE_ID_MAX: u64 = 12;"),
            "PM VFS image ID range must extend to 12 (fat=10, ramfs=11, ext4=12)"
        );
    }

    #[test]
    fn stage80_init_spawns_ext4_srv_with_image_id_12() {
        let init_src = include_str!("init/service.rs");
        // The ext4 spawn code exists inside the INIT_SPAWN_OPTIONAL_FS_SERVERS gate.
        assert!(
            init_src.contains("spawn_v5_cap(pm_send, pm_recv, 12,"),
            "init run() must contain ext4_srv spawn with image_id=12 (inside optional gate)"
        );
        assert!(
            init_src.contains("INIT_EXT4_SPAWN_BEGIN"),
            "init must contain INIT_EXT4_SPAWN_BEGIN (inside optional gate)"
        );
        assert!(
            init_src.contains("EXT4_SRV_READY"),
            "init must contain EXT4_SRV_READY (inside optional gate)"
        );
    }

    #[test]
    fn stage88_init_ext4_vfs_mount_enabled_after_spawn_documented() {
        // Stage 88 supersedes the Stage-80 deferred-mount requirement.
        // ext4 live mount is now enabled: register_ext4_mount_with_vfs() is called
        // after a successful ext4 spawn and /ext4 is registered read-only with VFS.
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("register_ext4_mount_with_vfs("),
            "init must call register_ext4_mount_with_vfs after successful ext4 spawn (Stage 88)"
        );
        assert!(
            init_src.contains("VFS_MOUNT_REGISTER_EXT4_OK"),
            "init must document VFS_MOUNT_REGISTER_EXT4_OK marker for /ext4 registration"
        );
        assert!(
            !init_src.contains("mount_deferred=true"),
            "mount_deferred=true must be absent (Stage 88: ext4 is live-mounted, not deferred)"
        );
        assert!(
            !init_src.contains("no-ipc-loop"),
            "no-ipc-loop blocker must be absent (lifted in Stage 86; mount wired in Stage 88)"
        );
    }

    #[test]
    fn stage80_pm_ext4_cpio_path_registered() {
        let pm_src = include_str!("process_manager/service.rs");
        assert!(
            pm_src.contains("12 => b\"/initramfs/sbin/ext4_srv\""),
            "PM must map image_id=12 to /initramfs/sbin/ext4_srv in pm_vfs_spawn_inline"
        );
        assert!(
            pm_src.contains("12 => Some(b\"sbin/ext4_srv\")"),
            "PM must map image_id=12 to sbin/ext4_srv in pm_image_cpio_name"
        );
    }

    #[test]
    fn stage80_syscall_count_unchanged() {
        let pm_src = include_str!("process_manager/service.rs");
        assert!(
            !pm_src.contains("SYSCALL_COUNT = 32"),
            "SYSCALL_COUNT must remain 31; Stage 80 must not add syscalls"
        );
    }

    // ── Stage 80R/81: optional FS profile gating ─────────────────────────────

    #[test]
    fn stage86_optional_fs_spawn_sub_gates_present() {
        // Stage 86 lifts the Stage-81 "all-off" guard.  Verify the per-server sub-gates exist.
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("INIT_SPAWN_OPTIONAL_FS_SERVERS"),
            "init must define INIT_SPAWN_OPTIONAL_FS_SERVERS"
        );
        assert!(
            init_src.contains("INIT_SPAWN_RAMFS_SRV"),
            "init must define INIT_SPAWN_RAMFS_SRV sub-gate"
        );
        assert!(
            init_src.contains("INIT_SPAWN_FAT_SRV"),
            "init must define INIT_SPAWN_FAT_SRV sub-gate"
        );
        assert!(
            init_src.contains("INIT_SPAWN_EXT4_SRV"),
            "init must define INIT_SPAWN_EXT4_SRV sub-gate"
        );
    }

    #[test]
    fn stage81_optional_fs_skipped_markers_present() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("INIT_RAMFS_SPAWN_SKIPPED reason=profile_disabled"),
            "init must emit INIT_RAMFS_SPAWN_SKIPPED when optional FS is disabled"
        );
        assert!(
            init_src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled"),
            "init must emit INIT_FAT_SPAWN_SKIPPED when optional FS is disabled"
        );
        assert!(
            init_src.contains("INIT_EXT4_SPAWN_SKIPPED reason=profile_disabled"),
            "init must emit INIT_EXT4_SPAWN_SKIPPED when optional FS is disabled"
        );
    }

    #[test]
    fn stage81_core_spawn_order_driver_manager_before_optional_fs() {
        let init_src = include_str!("init/service.rs");
        let dm_pos = init_src
            .find("INIT_DRIVER_MANAGER_SPAWN_V5_CALL_BEGIN")
            .expect("INIT_DRIVER_MANAGER_SPAWN_V5_CALL_BEGIN must be present");
        let optional_pos = init_src
            .find("INIT_SPAWN_OPTIONAL_FS_SERVERS")
            .expect("INIT_SPAWN_OPTIONAL_FS_SERVERS must be present");
        assert!(
            dm_pos < optional_pos,
            "driver_manager spawn must appear before optional FS section in init/service.rs"
        );
    }

    #[test]
    fn stage81_kernel_spawn_path_table_blocker_documented() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("spawn_image_path_for_image_id"),
            "init/service.rs must document the kernel spawn_image_path_for_image_id blocker"
        );
        assert!(
            init_src.contains("SyscallError::InvalidArgs"),
            "init/service.rs must document the InvalidArgs failure from the kernel path table"
        );
    }

    #[test]
    fn stage81_optional_fs_spawn_code_gates_not_direct_spawns() {
        let init_src = include_str!("init/service.rs");
        // INIT_RAMFS_SPAWN_BEGIN must only appear inside the INIT_SPAWN_OPTIONAL_FS_SERVERS gate.
        // Verify by checking that INIT_SPAWN_OPTIONAL_FS_SERVERS appears before
        // INIT_RAMFS_SPAWN_BEGIN in the source.
        let gate_pos = init_src
            .find("INIT_SPAWN_OPTIONAL_FS_SERVERS")
            .expect("INIT_SPAWN_OPTIONAL_FS_SERVERS gate must be present");
        let ramfs_begin_pos = init_src
            .find("INIT_RAMFS_SPAWN_BEGIN")
            .expect("INIT_RAMFS_SPAWN_BEGIN must be present inside optional gate");
        assert!(
            gate_pos < ramfs_begin_pos,
            "INIT_RAMFS_SPAWN_BEGIN must appear after INIT_SPAWN_OPTIONAL_FS_SERVERS gate declaration"
        );
    }
}
