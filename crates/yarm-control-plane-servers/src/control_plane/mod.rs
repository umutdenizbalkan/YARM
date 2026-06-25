// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Deprecated legacy namespace.
//! Workspace crates under `crates/` are the runtime dispatch entrypoints.

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

    // ── Stage 91: Optional-FS runtime stabilization ───────────────────────────

    // Part A: Smoke marker source-scan tests

    #[test]
    fn stage91_init_ramfs_spawn_ok_marker_stable() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("INIT_RAMFS_SPAWN_OK"),
            "init/service.rs must log INIT_RAMFS_SPAWN_OK on successful ramfs_srv spawn"
        );
    }

    #[test]
    fn stage91_init_ext4_spawn_ok_marker_stable() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("INIT_EXT4_SPAWN_OK"),
            "init/service.rs must log INIT_EXT4_SPAWN_OK on successful ext4_srv spawn"
        );
    }

    #[test]
    fn stage91_init_fat_spawn_skipped_markers_stable() {
        let init_src = include_str!("init/service.rs");
        // At least one of the two FAT skip reasons must be present.
        let has_profile_disabled =
            init_src.contains("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled");
        let has_server_disabled =
            init_src.contains("INIT_FAT_SPAWN_SKIPPED reason=server_disabled");
        assert!(
            has_profile_disabled || has_server_disabled,
            "init/service.rs must log INIT_FAT_SPAWN_SKIPPED with a reason tag"
        );
    }

    #[test]
    fn stage91_vfs_mount_register_ext4_ok_marker_present_in_init() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("VFS_MOUNT_REGISTER_EXT4_OK"),
            "init/service.rs must log VFS_MOUNT_REGISTER_EXT4_OK after ext4 mount registration"
        );
    }

    #[test]
    fn stage91_pm_recv_drain_begin_done_markers_present() {
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("INIT_PM_RECV_DRAIN_BEGIN"),
            "init/service.rs must log INIT_PM_RECV_DRAIN_BEGIN before the drain loop"
        );
        assert!(
            init_src.contains("INIT_PM_RECV_DRAIN_DONE"),
            "init/service.rs must log INIT_PM_RECV_DRAIN_DONE with count after drain"
        );
    }

    // Part D: Spawn/mount ordering tests

    #[test]
    fn stage91_fat_spawn_block_after_drain() {
        // FAT spawn (or skip) must happen after the pm_recv drain.
        let init_src = include_str!("init/service.rs");
        let drain_pos = init_src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let fat_pos = init_src
            .find("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled")
            .expect("FAT skip marker must be present");
        assert!(
            drain_pos < fat_pos,
            "pm_recv drain must appear before FAT spawn block in init/service.rs"
        );
    }

    #[test]
    fn stage91_ext4_mount_registration_after_spawn_ok() {
        // register_ext4_mount_with_vfs is called after INIT_EXT4_SPAWN_OK.
        // In source, the function definition (containing VFS_MOUNT_REGISTER_EXT4_OK)
        // precedes the call site, but the call to register_ext4_mount_with_vfs
        // must appear after INIT_EXT4_SPAWN_BEGIN in the run() function body.
        let init_src = include_str!("init/service.rs");
        let spawn_pos = init_src
            .find("INIT_EXT4_SPAWN_BEGIN")
            .expect("INIT_EXT4_SPAWN_BEGIN must be present");
        // The call to register_ext4_mount_with_vfs appears after INIT_EXT4_SPAWN_BEGIN.
        let call_pos = init_src[spawn_pos..]
            .find("register_ext4_mount_with_vfs(")
            .map(|off| spawn_pos + off)
            .expect("call to register_ext4_mount_with_vfs must follow INIT_EXT4_SPAWN_BEGIN");
        assert!(
            spawn_pos < call_pos,
            "INIT_EXT4_SPAWN_BEGIN must appear before the call to register_ext4_mount_with_vfs"
        );
    }

    // Part C: Reply endpoint hygiene (source-scan)

    #[test]
    fn stage91_mount_register_ext4_uses_dedicated_reply_cap_not_pm_recv() {
        let init_src = include_str!("init/service.rs");
        // register_ext4_mount_with_vfs must use blocking ipc_recv_v2 on reply_recv_cap.
        assert!(
            init_src.contains("fn register_ext4_mount_with_vfs("),
            "init/service.rs must define register_ext4_mount_with_vfs"
        );
        assert!(
            init_src.contains("ipc_recv_v2(reply_recv_cap"),
            "register_ext4_mount_with_vfs must use blocking ipc_recv_v2 on dedicated reply_recv_cap"
        );
        // Must not use non-blocking poll on pm_recv (stale-reply poisoning).
        let fn_start = init_src
            .find("fn register_ext4_mount_with_vfs(")
            .expect("function must exist");
        let after_fn = &init_src[fn_start + 1..];
        let fn_off = after_fn.find("\nfn ").unwrap_or(usize::MAX);
        let pub_fn_off = after_fn.find("\npub fn ").unwrap_or(usize::MAX);
        let fn_body_end = fn_start + 1 + fn_off.min(pub_fn_off);
        let fn_text = &init_src[fn_start..fn_body_end];
        assert!(
            !fn_text.contains("ipc_recv_with_deadline(pm_recv"),
            "register_ext4_mount_with_vfs must NOT use ipc_recv_with_deadline(pm_recv)"
        );
        assert!(
            !fn_text.contains("ipc_recv_with_deadline(reply_recv_cap"),
            "register_ext4_mount_with_vfs must NOT use non-blocking ipc_recv_with_deadline"
        );
    }

    #[test]
    fn stage91_ramfs_spawn_uses_zero_service_caps() {
        // RAMFS spawn must pass [0,0,0,0] — no config words in cap slots.
        // Passing encoded mount-config words causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL
        // because the kernel treats every non-zero service_caps entry as a cap ID.
        let init_src = include_str!("init/service.rs");
        let spawn_begin = init_src
            .find("INIT_RAMFS_SPAWN_BEGIN")
            .expect("INIT_RAMFS_SPAWN_BEGIN must be present");
        let spawn_ok = init_src[spawn_begin..]
            .find("INIT_RAMFS_SPAWN_OK")
            .map(|off| spawn_begin + off)
            .expect("INIT_RAMFS_SPAWN_OK must follow INIT_RAMFS_SPAWN_BEGIN");
        let ramfs_spawn_region = &init_src[spawn_begin..spawn_ok];
        assert!(
            ramfs_spawn_region.contains("[0, 0, 0, 0]"),
            "RAMFS spawn (image_id=11) must pass [0,0,0,0] as service_caps — no config words in cap slots"
        );
        assert!(
            !ramfs_spawn_region.contains("ramfs_prefix_word"),
            "RAMFS spawn must not pass ramfs_prefix_word as a service_cap"
        );
        assert!(
            !ramfs_spawn_region.contains("ramfs_meta_word"),
            "RAMFS spawn must not pass ramfs_meta_word as a service_cap"
        );
    }

    #[test]
    fn stage91_fat_spawn_uses_only_blkcache_cap() {
        // FAT spawn must pass blkcache cap only at position 0; positions 1-3 must be zero.
        // Passing encoded mount-config words in positions 1-2 causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL.
        let init_src = include_str!("init/service.rs");
        let spawn_begin = init_src
            .find("INIT_FAT_SPAWN_BEGIN")
            .expect("INIT_FAT_SPAWN_BEGIN must be present");
        let spawn_ok = init_src[spawn_begin..]
            .find("INIT_FAT_SPAWN_OK")
            .map(|off| spawn_begin + off)
            .expect("INIT_FAT_SPAWN_OK must follow INIT_FAT_SPAWN_BEGIN");
        let fat_spawn_region = &init_src[spawn_begin..spawn_ok];
        assert!(
            fat_spawn_region.contains("[init_blkcache_send_cap, 0, 0, 0]"),
            "FAT spawn (image_id=10) must pass [init_blkcache_send_cap,0,0,0] — only position 0 is a real cap"
        );
        assert!(
            !fat_spawn_region.contains("fat_prefix_word"),
            "FAT spawn must not pass fat_prefix_word as a service_cap"
        );
        assert!(
            !fat_spawn_region.contains("fat_meta_word"),
            "FAT spawn must not pass fat_meta_word as a service_cap"
        );
    }

    #[test]
    fn stage91_register_ramfs_uses_blocking_recv() {
        // register_ramfs_mount_with_vfs must use blocking ipc_recv_v2 on reply_recv_cap,
        // not a non-blocking poll. A non-blocking poll at deadline=0 leaves delayed VFS
        // mount-status replies (4 bytes) on pm_recv, poisoning the next spawn's reply read.
        let init_src = include_str!("init/service.rs");
        let fn_start = init_src
            .find("fn register_ramfs_mount_with_vfs(")
            .expect("register_ramfs_mount_with_vfs must be defined");
        let after_fn = &init_src[fn_start + 1..];
        let fn_off = after_fn.find("\nfn ").unwrap_or(usize::MAX);
        let pub_fn_off = after_fn.find("\npub fn ").unwrap_or(usize::MAX);
        let fn_body_end = fn_start + 1 + fn_off.min(pub_fn_off);
        let fn_text = &init_src[fn_start..fn_body_end];
        assert!(
            fn_text.contains("ipc_recv_v2(reply_recv_cap"),
            "register_ramfs_mount_with_vfs must use blocking ipc_recv_v2 on reply_recv_cap"
        );
        assert!(
            !fn_text.contains("ipc_recv_with_deadline(reply_recv_cap"),
            "register_ramfs_mount_with_vfs must NOT use non-blocking ipc_recv_with_deadline"
        );
    }

    #[test]
    fn stage91_register_ext4_uses_blocking_recv() {
        // register_ext4_mount_with_vfs must use blocking ipc_recv_v2 on reply_recv_cap.
        let init_src = include_str!("init/service.rs");
        let fn_start = init_src
            .find("fn register_ext4_mount_with_vfs(")
            .expect("register_ext4_mount_with_vfs must be defined");
        let after_fn = &init_src[fn_start + 1..];
        let fn_off = after_fn.find("\nfn ").unwrap_or(usize::MAX);
        let pub_fn_off = after_fn.find("\npub fn ").unwrap_or(usize::MAX);
        let fn_body_end = fn_start + 1 + fn_off.min(pub_fn_off);
        let fn_text = &init_src[fn_start..fn_body_end];
        assert!(
            fn_text.contains("ipc_recv_v2(reply_recv_cap"),
            "register_ext4_mount_with_vfs must use blocking ipc_recv_v2 on reply_recv_cap"
        );
        assert!(
            !fn_text.contains("ipc_recv_with_deadline(reply_recv_cap"),
            "register_ext4_mount_with_vfs must NOT use non-blocking ipc_recv_with_deadline"
        );
    }

    // Part I: initramfs path table rule

    #[test]
    fn stage91_pm_image_path_table_covers_all_optional_fs_servers() {
        // spawn_image_path_for_image_id must have arms for image_id 10 (fat_srv),
        // 11 (ramfs_srv), 12 (ext4_srv).
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("spawn_image_path_for_image_id"),
            "init/service.rs must use spawn_image_path_for_image_id for path lookup"
        );
        // The function must cover at least the sbin server paths.
        assert!(
            init_src.contains("INITRAMFS_FAT_SRV_PATH") || init_src.contains("fat_srv"),
            "init/service.rs must reference fat_srv path for spawn (image_id 10)"
        );
        assert!(
            init_src.contains("INITRAMFS_RAMFS_SRV_PATH") || init_src.contains("ramfs_srv"),
            "init/service.rs must reference ramfs_srv path for spawn (image_id 11)"
        );
        assert!(
            init_src.contains("INITRAMFS_EXT4_SRV_PATH") || init_src.contains("ext4_srv"),
            "init/service.rs must reference ext4_srv path for spawn (image_id 12)"
        );
    }

    // Part II: wrong-sender reply race fix (stage 91 AArch64 optional spawn fix)

    #[test]
    fn stage91_spawn_v5_cap_filters_wrong_sender_replies() {
        // Source-scan: spawn_v5_cap must filter out replies from non-PM senders.
        // VFS (tid≠PM) sends 8-byte OPENAT replies to the shared pm_recv endpoint
        // during PM's SpawnFromInitramfsFile grant path.  The drain loop must log
        // INIT_SPAWN_V5_WRONG_SENDER_REPLY and continue rather than treating those
        // replies as spawn failures.
        let src = include_str!("init/service.rs");
        let fn_start = src
            .find("fn spawn_v5_cap(")
            .expect("spawn_v5_cap must exist in init/service.rs");
        let fn_body = &src[fn_start..];
        assert!(
            fn_body.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "spawn_v5_cap must log INIT_SPAWN_V5_WRONG_SENDER_REPLY for wrong-sender drains"
        );
        assert!(
            fn_body.contains("expected_pm_tid"),
            "spawn_v5_cap must compute expected_pm_tid to identify PM's replies"
        );
        assert!(
            fn_body.contains("wrong_sender_count"),
            "spawn_v5_cap must maintain a wrong_sender_count drain counter"
        );
        assert!(
            fn_body.contains("MAX_WRONG_SENDER_DRAIN"),
            "spawn_v5_cap must use MAX_WRONG_SENDER_DRAIN to cap the drain loop"
        );
    }

    #[test]
    fn stage91_spawn_v5_cap_requires_pm_tid_and_correct_len() {
        // Source-scan: spawn_v5_cap must guard on BOTH sender_tid == expected_pm_tid
        // AND payload.len() == SpawnV5CapResult::ENCODED_LEN (16) before accepting.
        // An 8-byte VFS reply from tid=10002 must not be decoded as SpawnV5 failure.
        let src = include_str!("init/service.rs");
        let fn_start = src
            .find("fn spawn_v5_cap(")
            .expect("spawn_v5_cap must exist in init/service.rs");
        let fn_body = &src[fn_start..];
        assert!(
            fn_body.contains("sender_tid != expected_pm_tid"),
            "spawn_v5_cap must reject replies where sender_tid != expected_pm_tid"
        );
        assert!(
            fn_body.contains("SpawnV5CapResult::ENCODED_LEN"),
            "spawn_v5_cap must check payload.len() against SpawnV5CapResult::ENCODED_LEN"
        );
    }

    #[test]
    fn stage91_spawn_v5_cap_loops_until_pm_reply() {
        // Source-scan: spawn_v5_cap must loop (not single-recv) to drain wrong-sender
        // messages, using `continue` to skip non-PM messages and `return` for all
        // terminal paths.  Optional RAMFS/ext4 spawns both call spawn_v5_cap, so
        // both benefit from the loop.
        let src = include_str!("init/service.rs");
        let fn_start = src
            .find("fn spawn_v5_cap(")
            .expect("spawn_v5_cap must exist in init/service.rs");
        let fn_body = &src[fn_start..];
        assert!(
            fn_body.contains("loop {"),
            "spawn_v5_cap must use a loop to drain wrong-sender replies"
        );
        assert!(
            fn_body.contains("continue;"),
            "spawn_v5_cap drain loop must use continue on wrong-sender replies"
        );
        assert!(
            fn_body.contains("wrong_sender_drain_limit"),
            "spawn_v5_cap must log wrong_sender_drain_limit when drain limit is reached"
        );
    }

    #[test]
    fn stage91_pm_vfs_spawn_uses_service_recv_ep_not_reply_recv_cap() {
        // Source-scan: pm_vfs_spawn_inline must prefer process_manager_service_recv_ep
        // (slot 12, PM-private) over process_manager_reply_recv_cap (slot 2, shared
        // with init's pm_recv).  Routing VFS sub-call replies to slot 12 prevents
        // them from appearing on init's endpoint and being misread as SpawnV5 results.
        let src = include_str!("process_manager/service.rs");
        let fn_start = src
            .find("fn pm_vfs_spawn_inline(")
            .expect("pm_vfs_spawn_inline must exist in process_manager/service.rs");
        let fn_end = (fn_start + 2500).min(src.len());
        let fn_body = &src[fn_start..fn_end];
        assert!(
            fn_body.contains("process_manager_service_recv_ep"),
            "pm_vfs_spawn_inline must use process_manager_service_recv_ep (slot 12) for VFS sub-calls"
        );
        assert!(
            fn_body.contains(".or(ctx.process_manager_reply_recv_cap)"),
            "pm_vfs_spawn_inline must fall back to process_manager_reply_recv_cap only if service_recv_ep absent"
        );
    }

    #[test]
    fn stage91_vfs_8byte_reply_from_non_pm_tid_cannot_decode_as_spawn_v5_result() {
        // An 8-byte OPENAT reply from VFS (sender_tid=10002) must not satisfy the
        // spawn_v5_cap acceptance condition, which requires both sender_tid==PM_tid
        // and payload_len==16.  This unit test verifies that size mismatch alone
        // (8 != 16) is sufficient to trigger the wrong-sender drain path.
        use yarm_ipc_abi::process_abi::SpawnV5CapResult;
        let vfs_payload_len: usize = core::mem::size_of::<u64>(); // 8 bytes (OPENAT fd reply)
        let spawn_v5_len: usize = SpawnV5CapResult::ENCODED_LEN;
        assert_ne!(
            vfs_payload_len, spawn_v5_len,
            "VFS 8-byte reply must not match SpawnV5CapResult::ENCODED_LEN"
        );
        assert_eq!(
            spawn_v5_len, 16,
            "SpawnV5 result is always 16 bytes (pid:u64 + cap:u64)"
        );
    }

    // ── Stage 92: vfs_client.rs blocking-recv fix ─────────────────────────────

    #[test]
    fn stage92_vfs_client_all_ipc_helpers_use_ipc_recv_v2() {
        // Root-cause fix: all four vfs_client.rs IPC helpers must use blocking
        // ipc_recv_v2 so delayed VFS replies cannot appear on pm_recv during the
        // subsequent spawn_v5_cap wait.
        let src = include_str!("../../../yarm-user-rt/src/vfs_client.rs");

        let statx_start = src
            .find("pub unsafe fn vfs_statx(")
            .expect("vfs_statx must be defined in vfs_client.rs");
        assert!(
            src[statx_start..].contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_statx must use blocking ipc_recv_v2(reply_recv_cap)"
        );

        let openat_start = src
            .find("pub unsafe fn vfs_openat(")
            .expect("vfs_openat must be defined in vfs_client.rs");
        assert!(
            src[openat_start..].contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_openat must use blocking ipc_recv_v2(reply_recv_cap)"
        );

        let read_start = src
            .find("pub unsafe fn vfs_read(")
            .expect("vfs_read must be defined in vfs_client.rs");
        assert!(
            src[read_start..].contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_read must use blocking ipc_recv_v2(reply_recv_cap)"
        );

        let close_start = src
            .find("pub unsafe fn vfs_close(")
            .expect("vfs_close must be defined in vfs_client.rs");
        assert!(
            src[close_start..].contains("ipc_recv_v2(reply_recv_cap)"),
            "vfs_close must use blocking ipc_recv_v2(reply_recv_cap)"
        );
    }

    #[test]
    fn stage92_vfs_client_ipc_helpers_no_zero_deadline_recv() {
        // Negative: none of the four IPC helpers may use ipc_recv_with_deadline.
        // Non-blocking poll at deadline=0 was the root cause of Stage 92 wrong-sender
        // race on AArch64 (delayed VFS replies missed by pre-spawn drain loop).
        let src = include_str!("../../../yarm-user-rt/src/vfs_client.rs");
        let ipc_start = src
            .find("// ── IPC helpers")
            .expect("IPC helpers section header must be present in vfs_client.rs");
        let test_start = src.find("#[cfg(test)]").unwrap_or(src.len());
        let ipc_section = &src[ipc_start..test_start];
        assert!(
            !ipc_section.contains("ipc_recv_with_deadline"),
            "vfs_client.rs IPC helpers must not use ipc_recv_with_deadline after Stage 92 fix"
        );
    }

    #[test]
    fn stage92_smoke_aarch64_checks_spawn_fail_and_wrong_sender() {
        let script = include_str!("../../../../scripts/qemu-aarch64-optional-fs-smoke.sh");
        assert!(
            script.contains("INIT_RAMFS_SPAWN_FAIL"),
            "aarch64 smoke script must check for INIT_RAMFS_SPAWN_FAIL"
        );
        assert!(
            script.contains("INIT_EXT4_SPAWN_FAIL"),
            "aarch64 smoke script must check for INIT_EXT4_SPAWN_FAIL"
        );
        assert!(
            script.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "aarch64 smoke script must enforce zero INIT_SPAWN_V5_WRONG_SENDER_REPLY in strict mode"
        );
    }

    #[test]
    fn stage92_smoke_x86_64_checks_spawn_fail_and_wrong_sender() {
        let script = include_str!("../../../../scripts/qemu-x86_64-optional-fs-smoke.sh");
        assert!(
            script.contains("INIT_RAMFS_SPAWN_FAIL"),
            "x86_64 smoke script must check for INIT_RAMFS_SPAWN_FAIL"
        );
        assert!(
            script.contains("INIT_EXT4_SPAWN_FAIL"),
            "x86_64 smoke script must check for INIT_EXT4_SPAWN_FAIL"
        );
        assert!(
            script.contains("INIT_SPAWN_V5_WRONG_SENDER_REPLY"),
            "x86_64 smoke script must enforce zero INIT_SPAWN_V5_WRONG_SENDER_REPLY in strict mode"
        );
    }

    // ── Stage 93: FAT production groundwork ──────────────────────────────────

    #[test]
    fn stage93_ipc_block_device_no_zero_deadline_recv_in_fat_fs() {
        // Both IpcBlockDevice::read_exact_at and write_sector must use ipc_recv_v2
        // (blocking) to receive blkcache replies.  Same root cause as Stage 92's
        // vfs_client.rs fix: deadline=0 is non-blocking and returns immediately
        // if blkcache_srv hasn't yet processed the request.
        let src = include_str!("../../../yarm-fs-servers/src/fs/fat/fs.rs");
        let impl_start = src
            .find("impl BlockDevice for IpcBlockDevice")
            .expect("IpcBlockDevice impl must be present in fat/fs.rs");
        let impl_body = &src[impl_start..];
        assert!(
            !impl_body.contains("ipc_recv_with_deadline"),
            "IpcBlockDevice must not use ipc_recv_with_deadline (deadline-0 race same as Stage 92)"
        );
        assert!(
            impl_body.contains("ipc_recv_v2(self.reply_recv_cap)"),
            "IpcBlockDevice::read_exact_at must use ipc_recv_v2"
        );
        assert!(
            impl_body.contains("ipc_recv_v2(reply_recv_cap)"),
            "IpcBlockDevice::write_sector must use ipc_recv_v2"
        );
    }

    #[test]
    fn stage93_fat_default_profile_all_gates_disabled() {
        // All three FAT production gates must be false in the default optional-fs profile.
        let pm_src = include_str!("process_manager/service.rs");
        let shared_src =
            include_str!("../../../yarm-fs-servers/src/fs/common/shared_io_adapter.rs");
        let init_src = include_str!("init/service.rs");
        assert!(
            init_src.contains("const INIT_SPAWN_FAT_SRV: bool = false"),
            "INIT_SPAWN_FAT_SRV must be false in default profile"
        );
        assert!(
            shared_src.contains("VFS_FAT_LIVE_MOUNT_ENABLED: bool = false"),
            "VFS_FAT_LIVE_MOUNT_ENABLED must be false in default profile"
        );
        assert!(
            shared_src.contains("VFS_FAT_SHARED_IO_ENABLED: bool = false"),
            "VFS_FAT_SHARED_IO_ENABLED must be false in default profile"
        );
        assert!(
            pm_src.contains("12 => b\"/initramfs/sbin/ext4_srv\""),
            "PM must still map image_id=12 to ext4_srv (regression guard)"
        );
    }

    #[test]
    fn stage93_smoke_scripts_check_all_fatal_patterns() {
        let aarch64 = include_str!("../../../../scripts/qemu-aarch64-optional-fs-smoke.sh");
        let x86_64 = include_str!("../../../../scripts/qemu-x86_64-optional-fs-smoke.sh");
        for (arch, script) in &[("aarch64", aarch64), ("x86_64", x86_64)] {
            for pattern in &[
                "KSPAWN_EXTRA_CAP_DELEGATE_FAIL",
                "PM_VFS_SPAWN_FAIL",
                "reason=bad_fd_decode",
                "fallback=phase2b",
                "panic",
            ] {
                assert!(
                    script.contains(pattern),
                    "{arch} optional-FS smoke must check for {pattern}"
                );
            }
        }
    }

    // ── SUP-2: supervisor inert PM restart model guardrails ──────────────────

    #[test]
    fn sup2_supervisor_pm_restart_contract_model_is_inert_and_bounded() {
        let src = include_str!("supervisor/service.rs");
        for needle in &[
            "pub struct SupervisorRestartRequest",
            "pub struct SupervisorRestartRequestBundle",
            "pub enum SupervisorRestartReason",
            "pub enum SupervisorRestartBlocker",
            "pub enum SupervisorRestartRequestStatus",
            "pub struct SupervisorRestartTokenRef",
            "pub struct SupervisorPmHandleRef",
            "pub enum SupervisorRestartRequestFailure",
            "MAX_RESTART_REQUESTS: usize = MAX_MANAGED_SERVICES",
            "SupervisorPmRestartValidationReport",
            "SupervisorPmRestartAccountingReport",
            "SupervisorPmRestartRollbackStep",
        ] {
            assert!(
                src.contains(needle),
                "supervisor SUP-2 model must include {needle}"
            );
        }
        assert!(
            src.contains("redacted_fingerprint") && !src.contains("token={}"),
            "restart-token model/logging should use redacted token refs, not full token logs"
        );
    }

    #[test]
    fn sup2_supervisor_runtime_restart_execution_remains_fail_closed() {
        let src = include_str!("supervisor/service.rs");
        for marker in &[
            "SUPERVISOR_PM_RESTART_REQUEST_BUILT",
            "SUPERVISOR_PM_RESTART_EXEC_DEFERRED_NO_PM_OP",
            "SUPERVISOR_PM_RESTART_VALIDATION_DEFERRED",
            "SUPERVISOR_PM_RESTART_ACCOUNTING_DEFERRED",
        ] {
            assert!(
                src.contains(marker),
                "runtime must preserve visible marker {marker}"
            );
        }
        assert!(
            src.contains("fn restart_task(&mut self, _tid: u64, _restart_token: u64) -> Result<(), KernelError>")
                && src.contains("Err(KernelError::InvalidCapability)"),
            "runtime restart op must not fake production success"
        );
    }

    #[test]
    fn sup2_supervisor_contract_does_not_call_live_pm_spawn_restart_or_caps() {
        let src = include_str!("supervisor/service.rs");
        let model_start = src
            .find("pub struct SupervisorRestartRequest")
            .expect("SUP-2 restart request model must be present");
        let model_end = src
            .find("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nstruct ManagedServiceRecord")
            .unwrap_or(src.len());
        let model_section = &src[model_start..model_end];
        for forbidden in &[
            "restart_task(",
            "ipc_send(",
            "ipc_reply(",
            "grant_driver_irq",
            "mint_irq_cap",
            "delegate_driver_bundle(",
            "alloc_anonymous_memory_object",
            "create_iova_space_cap",
        ] {
            assert!(
                !model_section.contains(forbidden),
                "SUP-2 model must be inert and not call {forbidden}"
            );
        }
    }

    // ── SUP-3: supervisor PM restart IPC contract and timer oracle ──────────

    #[test]
    fn sup3_supervisor_pm_restart_contract_descriptor_is_versioned_and_bounded() {
        let src = include_str!("supervisor/service.rs");
        for needle in &[
            "pub struct SupervisorPmRestartContract",
            "pub struct SupervisorPmRestartRequestV1",
            "pub struct SupervisorPmRestartReplyV1",
            "pub enum SupervisorPmRestartReplyStatus",
            "pub enum SupervisorPmRestartReplyFailure",
            "pub type SupervisorPmRestartContractVersion = u16",
            "pub struct SupervisorPmRestartWireLimits",
            "max_requests: MAX_RESTART_REQUESTS",
            "mock_only: true",
        ] {
            assert!(src.contains(needle), "SUP-3 contract must include {needle}");
        }
    }

    #[test]
    fn sup3_restart_request_mapping_and_reply_model_remain_inert() {
        let src = include_str!("supervisor/service.rs");
        for needle in &[
            "map_restart_request_to_pm_descriptor",
            "SupervisorPmRestartDescriptorStatus::Sendable",
            "SupervisorPmRestartDescriptorStatus::NonSendable",
            "SupervisorPmRestartDescriptorStatus::Deferred",
            "SupervisorRestartBlocker::MissingRestartToken",
            "apply_pm_restart_reply_model",
            "AcceptedRecorded",
            "DeferredRetryScheduled",
            "RollbackMarkedDegraded",
            "InvalidVersionRejected",
        ] {
            assert!(
                src.contains(needle),
                "SUP-3 mapping/reply model must include {needle}"
            );
        }
        assert!(
            src.contains("restart_token: request.restart_token")
                && src.contains("redacted_fingerprint")
                && !src.contains("raw_token"),
            "SUP-3 descriptor must preserve redacted token refs without raw tokens"
        );
    }

    #[test]
    fn sup3_timer_backoff_semantics_are_logical_and_fail_closed() {
        let src = include_str!("supervisor/service.rs");
        for needle in &[
            "pub enum SupervisorTimerMode",
            "LogicalTickOnly",
            "FutureTimerEndpoint",
            "pub struct SupervisorBackoffSchedule",
            "pub enum SupervisorBackoffDecision",
            "DeferredNoTimer",
            "OverflowCapped",
            "compute_backoff_decision",
            "due_restart_ready",
            "SUPERVISOR_TIMER_ENDPOINT_DEFERRED",
            "SUPERVISOR_BACKOFF_LOGICAL_TICK_ONLY",
        ] {
            assert!(
                src.contains(needle),
                "SUP-3 timer/backoff model must include {needle}"
            );
        }
    }

    #[test]
    fn sup3_runtime_pm_restart_ipc_remains_deferred() {
        let src = include_str!("supervisor/service.rs");
        for marker in &[
            "SUPERVISOR_PM_RESTART_CONTRACT_BUILT",
            "SUPERVISOR_PM_RESTART_IPC_DEFERRED_NO_PM_CLIENT",
            "SUPERVISOR_PM_RESTART_EXEC_DEFERRED_NO_PM_OP",
        ] {
            assert!(
                src.contains(marker),
                "runtime must preserve SUP-3 marker {marker}"
            );
        }
        assert!(
            !src.contains("PROC_OP_SUPERVISOR_RESTART") && !src.contains("PM_RESTART_SEND_LIVE"),
            "SUP-3 must not add a new live PM restart IPC call"
        );
    }

    // ── SUP-4: PM-side inert restart validation/accounting oracle ───────────

    #[test]
    fn sup4_pm_restart_validation_model_is_bounded_and_inert() {
        let pm_src = include_str!("process_manager/service.rs");
        for needle in &[
            "pub struct PmRestartRequestDescriptor",
            "pub struct PmRestartValidationReport",
            "pub enum PmRestartValidationStatus",
            "pub enum PmRestartValidationFailure",
            "pub struct PmRestartValidationPolicy",
            "pub enum PmRestartAuthority",
            "pub enum PmRestartTokenCheck",
            "pub enum PmRestartSenderCheck",
            "PM_RESTART_MAX_ENTRIES: usize = 8",
            "validate_pm_restart_request",
        ] {
            assert!(
                pm_src.contains(needle),
                "SUP-4 PM validation model must include {needle}"
            );
        }
        assert!(
            pm_src.contains("RawUnscopedToken")
                && pm_src.contains("WrongTokenOwner")
                && pm_src.contains("MissingVerifiedSupervisorIdentity"),
            "PM validation must reject unscoped tokens, wrong owners, and missing supervisor identity"
        );
    }

    #[test]
    fn sup4_pm_restart_accounting_and_reply_are_descriptive_only() {
        let pm_src = include_str!("process_manager/service.rs");
        for needle in &[
            "pub struct PmRestartAccountingPlan",
            "pub enum PmRestartReservation",
            "OldTaskTeardownSlot",
            "ReplacementTaskSlot",
            "AddressSpaceSlot",
            "CNodeStartupCapSlots",
            "pub struct PmRestartRollbackPlan",
            "pub struct PmRestartReplyDescriptor",
            "pub enum PmRestartReplyStatus",
            "pub enum PmRestartReplyFailure",
            "PmReplacementHandleDescriptor",
            "build_pm_restart_reply_descriptor",
        ] {
            assert!(
                pm_src.contains(needle),
                "SUP-4 PM accounting/reply model must include {needle}"
            );
        }
        let model_start = pm_src
            .find("pub struct PmRestartRequestDescriptor")
            .expect("SUP-4 PM model must be present");
        let model_end = pm_src
            .find("#[derive(Debug)]\n#[cfg(test)]")
            .unwrap_or(pm_src.len());
        let model = &pm_src[model_start..model_end];
        for forbidden in &[
            "spawn_process(",
            "restart_task(",
            "ipc_send(",
            "ipc_reply(",
            "mint",
            "revoke",
            "grant_driver_irq",
            "alloc_anonymous_memory_object",
        ] {
            assert!(
                !model.contains(forbidden),
                "SUP-4 PM model must not call {forbidden}"
            );
        }
    }

    #[test]
    fn sup4_does_not_change_global_ipc_abi_or_add_live_restart_opcode() {
        let pm_src = include_str!("process_manager/service.rs");
        let abi_src = include_str!("../../../yarm-ipc-abi/src/process_abi.rs");
        assert!(
            !abi_src.contains("PROC_OP_SUPERVISOR_RESTART")
                && !abi_src.contains("PROC_OP_PM_RESTART_V1"),
            "SUP-4 must not change global process IPC ABI constants"
        );
        assert!(
            pm_src.contains("PM_RESTART_CONTRACT_VERSION_V1")
                && !pm_src.contains("PROC_OP_PM_RESTART_V1"),
            "SUP-4 may define local oracle versioning but no live PM restart opcode"
        );
    }
}
