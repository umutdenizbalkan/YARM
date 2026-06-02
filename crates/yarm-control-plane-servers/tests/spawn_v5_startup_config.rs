// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_control_plane_servers::control_plane::init::service::{
    fat_spawn_v5_service_caps, ramfs_spawn_v5_service_caps,
};
use yarm_control_plane_servers::control_plane::process_manager::service::build_spawn_v5_startup_layout;
use yarm_fs_servers::fat::service::FatMountConfig;
use yarm_fs_servers::ramfs::service::RamFsMountConfig;
use yarm_user_rt::runtime::{
    STARTUP_SLOT_INITRD_PTR, STARTUP_SLOT_SERVICE_EXTRA_CAP_0, STARTUP_SLOT_SERVICE_EXTRA_CAP_1,
    install_startup_arg_slots, startup_arg_slot,
};

#[test]
fn ramfs_spawn_v5_request_keeps_config_out_of_cap_fields() {
    let service_caps = ramfs_spawn_v5_service_caps();
    assert_eq!(service_caps, [0, 0, 0, 0]);

    let layout = build_spawn_v5_startup_layout(11, service_caps);
    assert_eq!(layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_0], 0);
    assert_ne!(layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_1], 0);
    assert_ne!(layout.startup_args[STARTUP_SLOT_INITRD_PTR], 0);

    let decoded = RamFsMountConfig::decode_startup_words(
        layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_1],
        layout.startup_args[STARTUP_SLOT_INITRD_PTR],
    )
    .expect("ramfs config decodes from child raw startup words");
    assert_eq!(decoded.prefix(), b"/ram");
}

#[test]
fn fat_spawn_v5_request_keeps_config_out_of_cap_fields() {
    let service_caps = fat_spawn_v5_service_caps(0xfeed);
    assert_eq!(service_caps, [0xfeed, 0, 0, 0]);

    let layout = build_spawn_v5_startup_layout(10, service_caps);
    assert_eq!(
        layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_0],
        0xfeed
    );
    assert_ne!(layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_1], 0);
    assert_ne!(layout.startup_args[STARTUP_SLOT_INITRD_PTR], 0);

    let decoded = FatMountConfig::decode_startup_words(
        layout.startup_args[STARTUP_SLOT_SERVICE_EXTRA_CAP_1],
        layout.startup_args[STARTUP_SLOT_INITRD_PTR],
    )
    .expect("fat config decodes from child raw startup words");
    assert_eq!(decoded.prefix(), b"/fat");
    assert_eq!(decoded.device_id, 1);
    assert!(decoded.readonly);
}

#[test]
fn pm_cap_decode_ignores_ramfs_and_fat_config_words_as_caps() {
    let ram_word = u64::from_le_bytes([b'/', b'r', b'a', b'm', 0, 0, 0, 0]);
    assert_ne!(ram_word, 0);
    assert!(!ramfs_spawn_v5_service_caps().contains(&ram_word));
    assert!(!fat_spawn_v5_service_caps(7).contains(&ram_word));
}

#[test]
fn startup_arg_slot_14_15_receive_packed_ramfs_config() {
    let layout = build_spawn_v5_startup_layout(11, ramfs_spawn_v5_service_caps());
    install_startup_arg_slots(layout.startup_args);

    let prefix_word = startup_arg_slot(STARTUP_SLOT_SERVICE_EXTRA_CAP_1).expect("slot 14");
    let meta_word = startup_arg_slot(STARTUP_SLOT_INITRD_PTR).expect("slot 15");
    let decoded = RamFsMountConfig::decode_startup_words(prefix_word, meta_word)
        .expect("ramfs startup slots decode");

    assert_eq!(decoded.prefix(), b"/ram");
}

#[test]
fn ramfs_smoke_marker_regex_does_not_match_initramfs_binary_marker() {
    let marker = "RAMFS_BIN_ENTRY_START";
    let initramfs_log = "USER_LOG tid=4 msg=INITRAMFS_BIN_ENTRY_START";
    let ramfs_log = "USER_LOG tid=11 msg=RAMFS_BIN_ENTRY_START";

    let exact_msg_match = |line: &str, marker: &str| {
        line.split_whitespace()
            .any(|field| field == format!("msg={marker}").as_str())
    };

    assert!(!exact_msg_match(initramfs_log, marker));
    assert!(exact_msg_match(ramfs_log, marker));
}

#[test]
fn ramfs_expected_smoke_markers_match_runtime_strings() {
    let required = [
        "INIT_RAMFS_SPAWN_BEGIN",
        "INIT_RAMFS_SPAWN_OK",
        "PM_IMAGE_ID_11_RAMFS_SRV",
        "RAMFS_BIN_ENTRY_START",
        "RAMFS_BIN_BEFORE_RUN",
        "RAMFS_MOUNT_READY",
        "VFS_MOUNT_REGISTER_RAMFS_OK",
    ];
    let runtime_strings = [
        include_str!("../src/control_plane/init/service.rs"),
        include_str!("../src/control_plane/process_manager/service.rs"),
        include_str!("../../yarm-fs-servers/src/bin/ramfs_srv.rs"),
        include_str!("../../yarm-fs-servers/src/fs/ramfs/service.rs"),
    ];

    for marker in required {
        assert!(
            runtime_strings.iter().any(|src| src.contains(marker)),
            "missing runtime marker: {marker}"
        );
    }
    assert!(
        runtime_strings
            .iter()
            .any(|src| src.contains("RAMFS_CONFIG_FOUND") || src.contains("RAMFS_CONFIG_DEFAULT")),
        "missing runtime RAMFS config marker"
    );
}

#[test]
fn artifact_scripts_align_vfs_spawned_ramfs_and_fat_services() {
    let common = include_str!("../../../scripts/lib/build-qemu-artifacts-common.sh");
    assert!(common.contains("--align sbin/ramfs_srv"));
    assert!(common.contains("--align sbin/fat_srv"));
    assert!(common.contains("ALIGN_PROOF"));
    assert!(common.contains("sbin/ramfs_srv"));
    assert!(common.contains("sbin/fat_srv"));
}

#[test]
fn artifact_scripts_build_and_stage_ramfs_and_fat_services() {
    let x86 = include_str!("../../../scripts/build-qemu-x86_64-artifacts.sh");
    let aarch64 = include_str!("../../../scripts/build-qemu-aarch64-artifacts.sh");
    let common = include_str!("../../../scripts/lib/build-qemu-artifacts-common.sh");

    for script in [x86, aarch64] {
        assert!(script.contains("RAMFS_SERVER_BIN"));
        assert!(script.contains("FAT_SERVER_BIN"));
        assert!(script.contains("--bin \"$RAMFS_SERVER_BIN\""));
        assert!(script.contains("--bin \"$FAT_SERVER_BIN\""));
        assert!(script.contains("common_stage_ramfs_server_elf"));
        assert!(script.contains("common_stage_fat_server_elf"));
    }
    assert!(common.contains("common_stage_ramfs_server_elf()"));
    assert!(common.contains("common_stage_fat_server_elf()"));
}

#[test]
fn aligned_packer_emits_align_proof_for_ramfs_srv() {
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let packer = repo.join("scripts/pack-initramfs-aligned.py");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let tmp = std::env::temp_dir().join(format!("yarm-align-proof-{nonce}"));
    let root = tmp.join("rootfs");
    let sbin = root.join("sbin");
    fs::create_dir_all(&sbin).expect("create sbin");
    fs::write(sbin.join("ramfs_srv"), b"ramfs-test-elf-bytes").expect("write ramfs");
    let out = tmp.join("initramfs.cpio");

    let output = Command::new("python3")
        .arg(&packer)
        .arg(&root)
        .arg(&out)
        .arg("--align")
        .arg("sbin/ramfs_srv")
        .output()
        .expect("run aligned packer");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = fs::remove_dir_all(&tmp);

    assert!(output.status.success(), "packer failed: {stderr}");
    assert!(
        stderr.contains("ALIGN_PROOF path=sbin/ramfs_srv") && stderr.contains("aligned=true"),
        "missing ramfs ALIGN_PROOF in: {stderr}"
    );
}

#[test]
fn pm_logs_memory_object_alignment_precondition_for_vfs_spawn_ids() {
    let pm_src = include_str!("../src/control_plane/process_manager/service.rs");
    assert!(pm_src.contains("PM_VFS_MO_ALIGN_REQUIRED image_id={}"));
    assert!(pm_src.contains("PM_SPAWN_FROM_MO_BEGIN image_id={}"));
    assert!(pm_src.contains("reason=spawn_from_mo_err mo_cap={}"));
}

#[test]
fn kernel_spawn_from_mo_allowlist_covers_fat_and_ramfs() {
    let kernel_src = include_str!("../../../src/kernel/syscall.rs");
    assert!(kernel_src.contains("10 => Some(\"sbin/fat_srv\")"));
    assert!(kernel_src.contains("11 => Some(\"sbin/ramfs_srv\")"));
    assert!(kernel_src.contains("SPAWN_FROM_MO_AFTER_ENTRY image_id={}"));
    assert!(kernel_src.contains("SPAWN_FROM_MO_VALIDATE_OK image_id={}"));
    assert!(kernel_src.contains("SPAWN_FROM_MO_TID_ALLOC_BEGIN image_id={}"));
    assert!(kernel_src.contains("SPAWN_FROM_MO_TASK_CREATE_BEGIN image_id={}"));
    assert!(kernel_src.contains("SPAWN_FROM_MO_RETURN_OK image_id={}"));
}

#[test]
fn user_rt_x86_64_raw_syscall_documents_required_clobbers_and_arg3_r10() {
    let x86_src = include_str!("../../yarm-user-rt/src/arch/x86_64.rs");
    assert!(x86_src.contains("in(\"r10\") args[3]"));
    assert!(x86_src.contains("lateout(\"rcx\") error"));
    assert!(x86_src.contains("lateout(\"r11\") _"));
    assert!(!x86_src.contains("in(\"rcx\") args[3]"));
}

#[test]
fn spawn_from_memory_object_wrapper_packs_image11_args_and_exposes_raw_diagnostics() {
    let user_rt_src = include_str!("../../yarm-user-rt/src/lib.rs");
    assert!(user_rt_src.contains("pub unsafe fn spawn_from_memory_object_raw_diagnostic"));
    assert!(user_rt_src.contains("image_id as usize"));
    assert!(user_rt_src.contains("mo_cap as usize"));
    assert!(user_rt_src.contains("parent_pid as usize"));
    assert!(user_rt_src.contains("startup_args.as_ptr() as usize"));
    assert!(user_rt_src.contains("startup_args.len()"));
}

#[test]
fn pm_spawn_from_mo_error_path_keeps_normal_spawnv5_reply_shape() {
    let pm_src = include_str!("../src/control_plane/process_manager/service.rs");
    assert!(pm_src.contains("PM_SPAWN_FROM_MO_SYSCALL_BEGIN image_id={}"));
    assert!(pm_src.contains("PM_SPAWN_FROM_MO_SYSCALL_RETURN image_id={}"));
    assert!(pm_src.contains("PM_SPAWN_FROM_MO_SYSCALL_ERR image_id={}"));
    assert!(pm_src.contains("PM_SPAWN_FROM_MO_AFTER_SYSCALL image_id={}"));
    assert!(pm_src.contains("let encoded = encode_spawn_v5_reply(0, 0);"));
    assert!(pm_src.contains("Message::with_header(\n                                        0,\n                                        PROC_OP_SPAWN_V5_CAP"));
}

#[test]
fn pm_vfs_call_u64_has_raw_syscall_and_decode_diagnostics() {
    let pm_src = include_str!("../src/control_plane/process_manager/service.rs");
    assert!(pm_src.contains("PM_VFS_CALL_U64_BEGIN op={}"));
    assert!(pm_src.contains("PM_VFS_CALL_U64_AFTER_SYSCALL op={}"));
    assert!(pm_src.contains("PM_VFS_CALL_U64_DECODE_OK op={}"));
    assert!(pm_src.contains("PM_VFS_CALL_U64_DECODE_ERR op={}"));
    assert!(pm_src.contains("ipc_call_raw_diagnostic"));
}
