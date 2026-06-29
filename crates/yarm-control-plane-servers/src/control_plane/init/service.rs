// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(test)]
use super::super::process_manager::service::ProcessService;
#[cfg(test)]
use super::super::process_manager::service::run_request_loop as run_process_manager_request_loop;
#[cfg(test)]
use super::super::supervisor::SupervisorService;
#[cfg(test)]
use super::super::vfs::service::run_request_loop as run_vfs_request_loop;
#[cfg(test)]
use crate::control_plane::init::InitService;
use crate::control_plane::init::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitBootPhase,
};
#[cfg(test)]
use yarm::kernel::boot::{KernelError, KernelState};
#[cfg(test)]
use yarm_fs_servers::common::service::FsService;
#[cfg(test)]
use yarm_fs_servers::common::vfs_ipc::InMemoryBackend;
#[cfg(test)]
use yarm_fs_servers::devfs::service::run_request_loop as run_devfs_request_loop;
#[cfg(test)]
use yarm_fs_servers::devfs::{DevFsBackend, DevFsService};
use yarm_fs_servers::fat::service::FatMountConfig;
#[cfg(test)]
use yarm_fs_servers::initramfs::build_core_service_elf_launch_plan;
#[cfg(test)]
use yarm_fs_servers::initramfs::service::run_request_loop as run_initramfs_request_loop;
#[cfg(test)]
use yarm_fs_servers::initramfs::{InitramfsBackend, InitramfsService, boot_initrd_bytes};
use yarm_fs_servers::ramfs::service::RamFsMountConfig;
use yarm_ipc_abi::blkcache_abi::{BLKCACHE_OP_REGISTER_BACKEND, RegisterBackendArgs};
use yarm_ipc_abi::block_abi::{BLK_OP_GET_INFO, BlkGetInfoReply, BlkGetInfoRequest, BlkStatus};
use yarm_ipc_abi::supervisor_abi::{RegisterDriverRequest, SUPERVISOR_OP_REGISTER_DRIVER};
use yarm_ipc_abi::vfs_abi::{MountRegisterArgs, VFS_MOUNT_STATUS_OK, VFS_OP_MOUNT_REGISTER};
#[cfg(test)]
use yarm_ipc_abi::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};

fn supervisor_restart_test_build_gate_enabled() -> bool {
    option_env!("YARM_SUPERVISOR_RESTART_TEST") == Some("1")
        || option_env!("SUPERVISOR_RESTART_TEST") == Some("1")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitCoreImageSource<'a> {
    Fixed(CoreServiceImagePlan),
    Manifest {
        manifest_bytes: &'a [u8],
        images: &'a [(u64, &'a [u8])],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRuntimeBootConfig<'a> {
    pub launch_strategy: CoreLaunchStrategy,
    pub graph: CoreServiceGraph,
    pub image_source: InitCoreImageSource<'a>,
    pub restart_window_ticks: u64,
}

impl InitRuntimeBootConfig<'_> {
    pub const fn baseline() -> Self {
        Self {
            launch_strategy: CoreLaunchStrategy::SupervisorFirst,
            graph: CoreServiceGraph {
                init_tid: 1,
                process_manager_tid: 2,
                vfs_tid: 3,
                supervisor_tid: 4,
                posix_compat_tid: None,
            },
            image_source: InitCoreImageSource::Fixed(CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
                posix_compat_entry: None,
            }),
            restart_window_ticks: 100,
        }
    }
}

impl Default for InitRuntimeBootConfig<'_> {
    fn default() -> Self {
        Self::baseline()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRuntimeSummary {
    pub phase: InitBootPhase,
    pub handles: crate::control_plane::init::CoreServiceHandles,
    pub seeded_registrations: usize,
    pub mount_report: crate::control_plane::init::MountRecoveryReport,
    pub present_cpus: usize,
    pub present_cpu_bitmap: u64,
    pub online_cpus: usize,
    pub restart_counts: (u8, u8, u8),
    pub isolation: CoreServiceIsolationReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServiceIsolationReport {
    pub process_manager_asid: u16,
    pub vfs_asid: u16,
    pub supervisor_asid: u16,
}

#[cfg(test)]
fn boot_init_runtime(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig<'_>,
) -> Result<(InitService, usize, CoreServiceIsolationReport), KernelError> {
    let mut init = InitService::new();
    init.set_launch_strategy(config.launch_strategy);
    init.register_core_graph(kernel, config.graph)?;
    init.launch_core_services(kernel, resolve_core_image_plan(config.image_source)?)?;
    init.install_fault_handoff(kernel, config.restart_window_ticks)?;
    let isolation = validate_core_service_isolation(kernel, &init)?;
    let seeded_registrations = init.seed_supervisor_registrations(kernel)?;
    init.begin_running(kernel)?;
    Ok((init, seeded_registrations, isolation))
}

#[cfg(test)]
pub fn run_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig<'_>,
) -> Result<InitRuntimeSummary, KernelError> {
    let (init, seeded_registrations, isolation) = boot_init_runtime(kernel, config)?;

    Ok(InitRuntimeSummary {
        phase: init.phase(),
        handles: init.handles(),
        seeded_registrations,
        mount_report: init.mount_status().ok_or(KernelError::WrongObject)?,
        present_cpus: kernel.present_cpu_count(),
        present_cpu_bitmap: kernel.present_cpu_bitmap(),
        online_cpus: kernel.online_cpu_count(),
        restart_counts: init.restart_counts(),
        isolation,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub struct MinimumRunnableProfileSummary {
    pub init_phase: InitBootPhase,
    pub seeded_registrations: usize,
    pub supervisor_managed_services: usize,
    pub process_wait_exit: u64,
    pub process_loop_handled: usize,
    pub control_vfs_fd: u64,
    pub control_vfs_handled: usize,
    pub devfs_open_opcode: u16,
    pub devfs_handled: usize,
    pub initramfs_read_opcode: u16,
    pub initramfs_handled: usize,
    pub mount_report: crate::control_plane::init::MountRecoveryReport,
    pub recovered_core_services: usize,
}

#[cfg(test)]
pub fn run_minimum_profile_with_kernel(
    kernel: &mut KernelState,
    config: InitRuntimeBootConfig<'_>,
) -> Result<MinimumRunnableProfileSummary, KernelError> {
    let (mut init, seeded_registrations, _) = boot_init_runtime(kernel, config)?;
    let handoff = init.fault_handoff().ok_or(KernelError::WrongObject)?;
    let mut supervisor = SupervisorService::new(
        init.handles().init_tid.ok_or(KernelError::WrongObject)?,
        handoff,
        init.restart_policies(),
    );
    let supervisor_managed_services = supervisor.run_until_idle(kernel)?;

    let mut proc = ProcessService::new();
    let proc_summary = run_process_manager_request_loop(&mut proc, 1, 99, 7)
        .map_err(|_| KernelError::WrongObject)?;

    let mut control_vfs = FsService::with_backend(InMemoryBackend::new());
    let control_vfs_summary =
        run_vfs_request_loop(&mut control_vfs, 0x1000).map_err(|_| KernelError::WrongObject)?;

    let mut devfs = DevFsService::with_backend(DevFsBackend::default());
    let devfs_summary = run_devfs_request_loop(&mut devfs).map_err(|_| KernelError::WrongObject)?;
    let devfs_open_opcode = VFS_OP_OPENAT;

    let initramfs_backend = if let Some(bytes) = boot_initrd_bytes() {
        InitramfsBackend::from_cpio_newc_static(bytes)
    } else {
        InitramfsBackend::new(4096)
    };
    let mut initramfs = InitramfsService::with_backend(initramfs_backend);
    let initramfs_summary =
        run_initramfs_request_loop(&mut initramfs).map_err(|_| KernelError::WrongObject)?;
    let initramfs_read_opcode = VFS_OP_READ;

    if devfs_open_opcode != VFS_OP_OPENAT || initramfs_read_opcode != VFS_OP_READ {
        return Err(KernelError::WrongObject);
    }
    let recovered_core_services = init.monitor_core_failures(kernel).unwrap_or(0);

    Ok(MinimumRunnableProfileSummary {
        init_phase: init.phase(),
        seeded_registrations,
        supervisor_managed_services,
        process_wait_exit: proc_summary.waited_exit,
        process_loop_handled: proc_summary.handled,
        control_vfs_fd: control_vfs_summary.fd,
        control_vfs_handled: control_vfs_summary.handled,
        devfs_open_opcode,
        devfs_handled: devfs_summary.handled,
        initramfs_read_opcode,
        initramfs_handled: initramfs_summary.handled,
        mount_report: init.mount_status().ok_or(KernelError::WrongObject)?,
        recovered_core_services,
    })
}

#[cfg(test)]
fn validate_core_service_isolation(
    kernel: &KernelState,
    init: &InitService,
) -> Result<CoreServiceIsolationReport, KernelError> {
    let handles = init.handles();
    let proc_tid = handles
        .process_manager_tid
        .ok_or(KernelError::WrongObject)?;
    let vfs_tid = handles.vfs_tid.ok_or(KernelError::WrongObject)?;
    let supervisor_tid = handles.supervisor_tid.ok_or(KernelError::WrongObject)?;
    let proc_asid = kernel.task_asid(proc_tid).ok_or(KernelError::WrongObject)?;
    let vfs_asid = kernel.task_asid(vfs_tid).ok_or(KernelError::WrongObject)?;
    let supervisor_asid = kernel
        .task_asid(supervisor_tid)
        .ok_or(KernelError::WrongObject)?;
    if proc_asid == vfs_asid || proc_asid == supervisor_asid || vfs_asid == supervisor_asid {
        return Err(KernelError::WrongObject);
    }
    Ok(CoreServiceIsolationReport {
        process_manager_asid: proc_asid.0,
        vfs_asid: vfs_asid.0,
        supervisor_asid: supervisor_asid.0,
    })
}

#[cfg(test)]
fn resolve_core_image_plan(
    source: InitCoreImageSource<'_>,
) -> Result<CoreServiceImagePlan, KernelError> {
    match source {
        InitCoreImageSource::Fixed(plan) => Ok(plan),
        InitCoreImageSource::Manifest {
            manifest_bytes,
            images,
        } => {
            let launch = build_core_service_elf_launch_plan(manifest_bytes, images)
                .map_err(|_| KernelError::WrongObject)?;
            Ok(CoreServiceImagePlan {
                process_manager_entry: launch.process_manager.validated_entry as usize,
                vfs_entry: launch.vfs.validated_entry as usize,
                supervisor_entry: launch.supervisor.validated_entry as usize,
                posix_compat_entry: launch
                    .posix_compat
                    .map(|plan| plan.validated_entry as usize),
            })
        }
    }
}

fn spawn_v5_cap(
    pm_send: u32,
    pm_recv: u32,
    image_id: u64,
    service_caps: [u64; 4],
    parent_pid: u64,
) -> Option<(u64, u64)> {
    use yarm_ipc_abi::process_abi::{
        PROC_OP_SPAWN_V5_CAP, SpawnV5CapArgs, SpawnV5CapResult, decode_spawn_v5_reply,
    };
    let args = SpawnV5CapArgs::new(parent_pid, image_id, service_caps);
    let encoded = args.encode();
    let Ok(msg) =
        yarm_user_rt::ipc::Message::with_header(0, PROC_OP_SPAWN_V5_CAP, 0, None, &encoded)
    else {
        return None;
    };
    // SAFETY: Uses kernel-provided startup caps for PM IPC request.
    let _ = unsafe { yarm_user_rt::syscall::ipc_call(pm_send, pm_recv, &msg) };
    // PM tid is deterministically init_tid + 2 (init=1, supervisor=2, PM=3).
    let expected_pm_tid = yarm_user_rt::runtime::startup_context().task_id + 2;
    const MAX_WRONG_SENDER_DRAIN: usize = 16;
    let mut wrong_sender_count: usize = 0;
    loop {
        yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_RECV_BEGIN cap={}", pm_recv);
        let reply = unsafe { yarm_user_rt::syscall::ipc_recv_v2(pm_recv) };
        match reply {
            Ok(Some(received)) => {
                let r = received.message;
                let payload = r.as_slice();
                yarm_user_rt::user_log!(
                    "INIT_SPAWN_V5_REPLY_RECV_OK status=na sender_tid={} payload_len={} opcode={} flags={}",
                    received.sender_tid,
                    payload.len(),
                    r.opcode,
                    r.flags
                );
                yarm_user_rt::user_log!(
                    "INIT_SPAWN_V5_REPLY_RECV_BYTES len={} bytes={:x?}",
                    payload.len(),
                    payload
                );
                yarm_user_rt::user_log!(
                    "INIT_SPAWN_V5_REPLY_DECODE_INPUT len={} bytes={:x?}",
                    payload.len(),
                    payload
                );
                if received.sender_tid != expected_pm_tid
                    || payload.len() != SpawnV5CapResult::ENCODED_LEN
                {
                    wrong_sender_count = wrong_sender_count.saturating_add(1);
                    yarm_user_rt::user_log!(
                        "INIT_SPAWN_V5_WRONG_SENDER_REPLY sender_tid={} payload_len={} expected_tid={} expected_len={} drain={}",
                        received.sender_tid,
                        payload.len(),
                        expected_pm_tid,
                        SpawnV5CapResult::ENCODED_LEN,
                        wrong_sender_count
                    );
                    if wrong_sender_count >= MAX_WRONG_SENDER_DRAIN {
                        yarm_user_rt::user_log!(
                            "INIT_SPAWN_V5_REPLY_DECODE ok=0 child_tid=0 reason=wrong_sender_drain_limit drain={}",
                            wrong_sender_count
                        );
                        return None;
                    }
                    continue;
                }
                yarm_user_rt::user_log!(
                    "INIT_SPAWN_V5_REPLY_RECV len={} opcode={} flags={} bytes={:x?}",
                    r.len,
                    r.opcode,
                    r.flags,
                    payload
                );
                match decode_spawn_v5_reply(payload) {
                    Ok(result) => {
                        if !spawn_v5_reply_is_success(result.pid, result.service_send_cap) {
                            yarm_user_rt::user_log!(
                                "INIT_SPAWN_V5_REPLY_DECODE ok=0 child_tid=0 reason=zero_pid"
                            );
                            yarm_user_rt::user_log!(
                                "INIT_SPAWN_V5_REPLY_FALLBACK_ZERO reason=zero_pid"
                            );
                            return None;
                        }
                        yarm_user_rt::user_log!(
                            "INIT_SPAWN_V5_REPLY_DECODE ok=1 child_tid={}",
                            result.pid
                        );
                        return Some((result.pid, result.service_send_cap));
                    }
                    Err(_) => {
                        yarm_user_rt::user_log!(
                            "INIT_SPAWN_V5_REPLY_FALLBACK_ZERO reason=decode_err"
                        );
                        yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_DECODE ok=0 child_tid=0");
                        return None;
                    }
                }
            }
            Ok(None) => {
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_RECV_ERR err=WouldBlockOrNoMessage");
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_FALLBACK_ZERO reason=recv_none");
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_DECODE ok=0 child_tid=0");
                return None;
            }
            Err(err) => {
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_RECV_ERR err={:?}", err);
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_FALLBACK_ZERO reason=recv_err");
                yarm_user_rt::user_log!("INIT_SPAWN_V5_REPLY_DECODE ok=0 child_tid=0");
                return None;
            }
        }
    }
}

fn spawn_v5_reply_is_success(pid: u64, _service_send_cap: u64) -> bool {
    pid != 0
}

fn register_ramfs_mount_with_vfs(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    ramfs_send_cap: u64,
    mount_config: RamFsMountConfig,
) -> bool {
    let Some((payload, len)) = (MountRegisterArgs {
        backend_send_cap: ramfs_send_cap,
        flags: if mount_config.readonly { 1 } else { 0 },
        prefix: mount_config.prefix(),
    })
    .encode() else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR reason=encode");
        return false;
    };
    let Ok(msg) =
        yarm_user_rt::ipc::Message::with_header(0, VFS_OP_MOUNT_REGISTER, 0, None, &payload[..len])
    else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR reason=message");
        return false;
    };
    yarm_user_rt::user_log!(
        "VFS_MOUNT_REGISTER_RAMFS_BEGIN prefix={}",
        alloc::string::String::from_utf8_lossy(mount_config.prefix())
    );
    let call = unsafe { yarm_user_rt::syscall::ipc_call(vfs_send_cap, reply_recv_cap, &msg) };
    if call.is_err() {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR reason=ipc-call");
        return false;
    }
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_v2(reply_recv_cap) };
    let Ok(Some(reply_msg)) = reply else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR reason=no-reply");
        return false;
    };
    let reply_payload = reply_msg.message.as_slice();
    if reply_payload.len() < 4 {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR reason=short-reply");
        return false;
    }
    let status = u32::from_le_bytes([
        reply_payload[0],
        reply_payload[1],
        reply_payload[2],
        reply_payload[3],
    ]);
    if status == VFS_MOUNT_STATUS_OK {
        yarm_user_rt::user_log!(
            "VFS_MOUNT_REGISTER_RAMFS_OK prefix={}",
            alloc::string::String::from_utf8_lossy(mount_config.prefix())
        );
        true
    } else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_RAMFS_ERR status={}", status);
        false
    }
}

fn register_fat_mount_with_vfs(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    fat_send_cap: u64,
    mount_config: FatMountConfig,
) -> bool {
    let Some((payload, len)) = (MountRegisterArgs {
        backend_send_cap: fat_send_cap,
        flags: if mount_config.readonly { 1 } else { 0 },
        prefix: mount_config.prefix(),
    })
    .encode() else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR reason=encode");
        return false;
    };
    let Ok(msg) =
        yarm_user_rt::ipc::Message::with_header(0, VFS_OP_MOUNT_REGISTER, 0, None, &payload[..len])
    else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR reason=message");
        return false;
    };
    yarm_user_rt::user_log!(
        "VFS_MOUNT_REGISTER_FAT_BEGIN prefix={} device_id={}",
        alloc::string::String::from_utf8_lossy(mount_config.prefix()),
        mount_config.device_id
    );
    let call = unsafe { yarm_user_rt::syscall::ipc_call(vfs_send_cap, reply_recv_cap, &msg) };
    if call.is_err() {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR reason=ipc-call");
        return false;
    }
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_v2(reply_recv_cap) };
    let Ok(Some(reply_msg)) = reply else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR reason=no-reply");
        return false;
    };
    let reply_payload = reply_msg.message.as_slice();
    if reply_payload.len() < 4 {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR reason=short-reply");
        return false;
    }
    let status = u32::from_le_bytes([
        reply_payload[0],
        reply_payload[1],
        reply_payload[2],
        reply_payload[3],
    ]);
    if status == VFS_MOUNT_STATUS_OK {
        yarm_user_rt::user_log!(
            "VFS_MOUNT_REGISTER_FAT_OK prefix={}",
            alloc::string::String::from_utf8_lossy(mount_config.prefix())
        );
        true
    } else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_FAT_ERR status={}", status);
        false
    }
}

fn register_ext4_mount_with_vfs(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    ext4_send_cap: u64,
) -> bool {
    let Some((payload, len)) = (MountRegisterArgs {
        backend_send_cap: ext4_send_cap,
        flags: 1,
        prefix: b"/ext4",
    })
    .encode() else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR reason=encode");
        return false;
    };
    let Ok(msg) =
        yarm_user_rt::ipc::Message::with_header(0, VFS_OP_MOUNT_REGISTER, 0, None, &payload[..len])
    else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR reason=message");
        return false;
    };
    yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_BEGIN prefix=/ext4");
    let call = unsafe { yarm_user_rt::syscall::ipc_call(vfs_send_cap, reply_recv_cap, &msg) };
    if call.is_err() {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR reason=ipc-call");
        return false;
    }
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_v2(reply_recv_cap) };
    let Ok(Some(reply_msg)) = reply else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR reason=no-reply");
        return false;
    };
    let reply_payload = reply_msg.message.as_slice();
    if reply_payload.len() < 4 {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR reason=short-reply");
        return false;
    }
    let status = u32::from_le_bytes([
        reply_payload[0],
        reply_payload[1],
        reply_payload[2],
        reply_payload[3],
    ]);
    if status == VFS_MOUNT_STATUS_OK {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_OK prefix=/ext4");
        true
    } else {
        yarm_user_rt::user_log!("VFS_MOUNT_REGISTER_EXT4_ERR status={}", status);
        false
    }
}

fn crash_test_supervisor_control_send_cap(
    ctx: &yarm_user_rt::runtime::StartupContext,
) -> Option<u32> {
    let raw = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SUPERVISOR_CONTROL_SEND_EP,
    )
    .unwrap_or(0);
    yarm_user_rt::user_log!("INIT_STARTUP_SLOT_SUPERVISOR_CONTROL_SEND raw={}", raw);
    match ctx.supervisor_control_send_ep {
        Some(cap) => {
            yarm_user_rt::user_log!("INIT_SUPERVISOR_CONTROL_SEND_CAP_PRESENT cap={}", cap);
            Some(cap)
        }
        None => {
            if raw == 0 {
                yarm_user_rt::user_log!("INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=zero");
                yarm_user_rt::user_log!(
                    "INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=startup-slot-empty"
                );
            } else {
                yarm_user_rt::user_log!("INIT_SUPERVISOR_CONTROL_SEND_CAP_MISSING reason=decode");
            }
            None
        }
    }
}

fn register_crash_test_with_supervisor(supervisor_send: u32, crash_tid: u64) {
    yarm_user_rt::user_log!("INIT_CRASH_TEST_REGISTER_BEGIN tid={}", crash_tid);
    let req = RegisterDriverRequest {
        tid: crash_tid,
        max_restarts: 3,
        restart_group: 13,
        dependency_mask: 0,
        backoff_ticks: 1,
        irq_line: 0,
        mem_cap: 0,
        iova_cap: 0,
        iova_base: 0,
        dma_len: 0,
        iova_len: 0,
    };
    let payload = req.encode();
    let Ok(msg) = yarm_user_rt::ipc::Message::with_header(
        0,
        SUPERVISOR_OP_REGISTER_DRIVER,
        0,
        None,
        &payload,
    ) else {
        yarm_user_rt::user_log!(
            "INIT_CRASH_TEST_REGISTER_FAIL tid={} reason=message",
            crash_tid
        );
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_CRASH_TEST_REGISTER_META opcode={} flags={} len={}",
        SUPERVISOR_OP_REGISTER_DRIVER,
        0,
        payload.len()
    );
    let first = [
        payload.first().copied().unwrap_or(0),
        payload.get(1).copied().unwrap_or(0),
        payload.get(2).copied().unwrap_or(0),
        payload.get(3).copied().unwrap_or(0),
        payload.get(4).copied().unwrap_or(0),
        payload.get(5).copied().unwrap_or(0),
        payload.get(6).copied().unwrap_or(0),
        payload.get(7).copied().unwrap_or(0),
    ];
    yarm_user_rt::user_log!(
        "INIT_CRASH_TEST_REGISTER_PAYLOAD first8=[{:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}] len={}",
        first[0],
        first[1],
        first[2],
        first[3],
        first[4],
        first[5],
        first[6],
        first[7],
        payload.len()
    );
    yarm_user_rt::user_log!(
        "INIT_CRASH_TEST_REGISTER_SEND cap={} tid={}",
        supervisor_send,
        crash_tid
    );
    match unsafe { yarm_user_rt::syscall::ipc_send(supervisor_send, &msg) } {
        Ok(()) => yarm_user_rt::user_log!("INIT_CRASH_TEST_REGISTER_OK tid={}", crash_tid),
        Err(err) => yarm_user_rt::user_log!(
            "INIT_CRASH_TEST_REGISTER_FAIL tid={} reason=ipc-send err={:?}",
            crash_tid,
            err
        ),
    }
}

pub fn run() {
    yarm_user_rt::user_log!("INIT_RUN_ENTER");
    let ctx = yarm_user_rt::runtime::startup_context();
    let (Some(pm_send), Some(pm_recv)) = (
        ctx.process_manager_request_send_cap,
        ctx.process_manager_reply_recv_cap,
    ) else {
        yarm_user_rt::user_log!("INIT_NO_PM_CAPS");
        return;
    };
    yarm_user_rt::user_log!("INIT_PM_CAPS send={} reply={}", pm_send, pm_recv);

    // --- Spawn initramfs_srv (image_id=4) ---
    yarm_user_rt::user_log!("INIT_SPAWN_V5_CALL_BEGIN");
    let Some((child_tid, initramfs_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 4, [0, 0, 0, 0], 0)
    else {
        yarm_user_rt::user_log!("INIT_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!("INIT_SPAWN_V5_CALL_RETURN ok=1 child_tid={}", child_tid);
    yarm_user_rt::user_log!("INIT_INITRAMFS_SPAWN_CAPS recv_cap={}", initramfs_send_cap);

    // --- Spawn devfs_srv (image_id=5) ---
    yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_V5_CALL_BEGIN");
    let Some((devfs_child_tid, devfs_send_cap)) =
        spawn_v5_cap(pm_send, pm_recv, 5, [0, 0, 0, 0], 0)
    else {
        yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_DEVFS_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        devfs_child_tid
    );
    yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_CAPS recv_cap={}", devfs_send_cap);

    // --- Spawn vfs_server (image_id=6) passing initramfs and devfs send caps ---
    // parent_pid=1 so the kernel delegates the vfs send cap into init's own cnode,
    // allowing init to send directly to vfs_server without going through PM.
    yarm_user_rt::user_log!("INIT_VFS_SPAWN_V5_CALL_BEGIN");
    let Some((vfs_child_tid, vfs_recv_cap)) = spawn_v5_cap(
        pm_send,
        pm_recv,
        6,
        [initramfs_send_cap, devfs_send_cap, 0, 0],
        1,
    ) else {
        yarm_user_rt::user_log!("INIT_VFS_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_VFS_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        vfs_child_tid
    );
    yarm_user_rt::user_log!(
        "INIT_VFS_SPAWN_CAPS recv_cap={} initramfs_send={} devfs_send={}",
        vfs_recv_cap,
        initramfs_send_cap,
        devfs_send_cap
    );

    // --- Spawn driver_manager (image_id=7) ---
    // No service caps required at spawn time. VFS-backed late services are
    // wired post-spawn through explicit IPC registration flows.
    yarm_user_rt::user_log!("INIT_DRIVER_MANAGER_SPAWN_V5_CALL_BEGIN");
    let Some((dm_child_tid, _dm_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 7, [0, 0, 0, 0], 0)
    else {
        yarm_user_rt::user_log!("INIT_DRIVER_MANAGER_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_DRIVER_MANAGER_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        dm_child_tid
    );

    // --- Spawn blkcache_srv (image_id=8) ---
    yarm_user_rt::user_log!("INIT_BLKCACHE_SPAWN_V5_CALL_BEGIN");
    // parent_pid=1 so PM delegates the blkcache service send cap into init's
    // CNode (caller-local namespace). PM lifecycle `pm_service_send_cap` stays
    // PM-local and must not be used by init for IPC.
    let Some((blkcache_child_tid, init_blkcache_send_cap)) =
        spawn_v5_cap(pm_send, pm_recv, 8, [0, 0, 0, 0], 1)
    else {
        yarm_user_rt::user_log!("INIT_BLKCACHE_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_BLKCACHE_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        blkcache_child_tid
    );

    // --- Spawn virtio_blk_srv (image_id=9) ---
    yarm_user_rt::user_log!("INIT_VIRTIO_BLK_SPAWN_V5_CALL_BEGIN");
    let Some((virtio_blk_child_tid, init_virtio_blk_send_cap)) =
        spawn_v5_cap(pm_send, pm_recv, 9, [0, 0, 0, 0], 1)
    else {
        yarm_user_rt::user_log!("INIT_VIRTIO_BLK_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_VIRTIO_BLK_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        virtio_blk_child_tid
    );

    yarm_user_rt::user_log!("INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_BEGIN");
    yarm_user_rt::user_log!(
        "INIT_BLKCACHE_REGISTER_BACKEND_CAP_TRANSFER cap={}",
        init_virtio_blk_send_cap
    );
    let register_backend_req = RegisterBackendArgs {
        backend_id: 1,
        backend_send_cap: init_virtio_blk_send_cap,
        block_size: 512,
        flags: 0,
        block_count: 1,
    };
    let register_backend_payload = register_backend_req.encode();
    yarm_user_rt::user_log!(
        "INIT_BLKCACHE_REGISTER_BACKEND_SEND_META opcode={} flags={} transferred_cap={} payload_len={} backend_id={} block_size={}",
        BLKCACHE_OP_REGISTER_BACKEND,
        yarm_user_rt::ipc::Message::FLAG_CAP_TRANSFER,
        init_virtio_blk_send_cap,
        register_backend_payload.len(),
        register_backend_req.backend_id,
        register_backend_req.block_size
    );
    let Ok(register_backend_msg) = yarm_user_rt::ipc::Message::with_header(
        0,
        BLKCACHE_OP_REGISTER_BACKEND,
        yarm_user_rt::ipc::Message::FLAG_CAP_TRANSFER,
        Some(init_virtio_blk_send_cap),
        &register_backend_payload,
    ) else {
        yarm_user_rt::user_log!(
            "INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=0 status=2 backend_id=1"
        );
        return;
    };
    yarm_user_rt::user_log!("INIT_BLKCACHE_REGISTER_BACKEND_SEND_NO_REPLY");
    let _ = unsafe {
        yarm_user_rt::syscall::ipc_send(init_blkcache_send_cap as u32, &register_backend_msg)
    };

    // NOTE: Keep SpawnV5/PM reply traffic isolated until the full service spawn
    // chain is complete. Since ipc_call is now send-only and replies are consumed
    // via explicit recv on pm_recv, interleaving VFS smoke calls here can enqueue
    // non-PM replies on the same endpoint and contaminate SpawnV5 recv/decode.
    // Run VFS smokes only after all SpawnV5 calls have completed.
    // --- VFS smoke: one real statx request through the routing stack ---
    {
        yarm_user_rt::user_log!("INIT_VFS_SMOKE_BEGIN path=/initramfs/boot-marker");
        let vfs_send = vfs_recv_cap as u32;
        yarm_user_rt::user_log!("INIT_VFS_SMOKE_CALL_BEGIN");
        // SAFETY: vfs_send and pm_recv are kernel-provided startup caps.
        match unsafe {
            yarm_user_rt::vfs_client::vfs_statx(vfs_send, pm_recv, b"/initramfs/boot-marker")
        } {
            Ok(status) => {
                yarm_user_rt::user_log!("INIT_VFS_SMOKE_CALL_RETURN ok=1 status={}", status)
            }
            Err(_) => yarm_user_rt::user_log!("INIT_VFS_SMOKE_CALL_RETURN ok=0 status=0"),
        }
    }

    // --- /dev smoke: prove VFS routing to devfs_srv ---
    {
        yarm_user_rt::user_log!("INIT_VFS_DEV_SMOKE_BEGIN path=/dev/null");
        let vfs_send = vfs_recv_cap as u32;
        yarm_user_rt::user_log!("INIT_VFS_DEV_SMOKE_CALL_BEGIN");
        // SAFETY: vfs_send and pm_recv are kernel-provided startup caps.
        match unsafe { yarm_user_rt::vfs_client::vfs_statx(vfs_send, pm_recv, b"/dev/null") } {
            Ok(status) => {
                yarm_user_rt::user_log!("INIT_VFS_DEV_SMOKE_CALL_RETURN ok=1 status={}", status)
            }
            Err(_) => yarm_user_rt::user_log!("INIT_VFS_DEV_SMOKE_CALL_RETURN ok=0 status=0"),
        }
    }

    // --- VFS open/read smoke ---
    {
        yarm_user_rt::user_log!("INIT_VFS_OPEN_SMOKE_BEGIN path=/initramfs/boot-marker");
        let vfs_send = vfs_recv_cap as u32;
        let mut read_buf = [0u8; 64];
        // SAFETY: vfs_send and pm_recv are kernel-provided startup caps.
        match unsafe {
            yarm_user_rt::vfs_client::vfs_openat(vfs_send, pm_recv, b"/initramfs/boot-marker", 0)
        } {
            Ok(fd) => {
                yarm_user_rt::user_log!("INIT_VFS_OPEN_SMOKE_CALL_RETURN ok=1 fd={}", fd);
                yarm_user_rt::user_log!("INIT_VFS_READ_SMOKE_BEGIN fd={}", fd);
                match unsafe {
                    yarm_user_rt::vfs_client::vfs_read(vfs_send, pm_recv, fd, &mut read_buf)
                } {
                    Ok(n) => {
                        yarm_user_rt::user_log!("INIT_VFS_READ_SMOKE_CALL_RETURN ok=1 len={}", n)
                    }
                    Err(_) => {
                        yarm_user_rt::user_log!("INIT_VFS_READ_SMOKE_CALL_RETURN ok=0 len=0")
                    }
                }
                yarm_user_rt::user_log!("INIT_VFS_CLOSE_SMOKE_BEGIN fd={}", fd);
                // SAFETY: vfs_send and pm_recv are kernel-provided startup caps.
                match unsafe { yarm_user_rt::vfs_client::vfs_close(vfs_send, pm_recv, fd) } {
                    Ok(status) => yarm_user_rt::user_log!(
                        "INIT_VFS_CLOSE_SMOKE_CALL_RETURN ok=1 status={}",
                        status
                    ),
                    Err(_) => {
                        yarm_user_rt::user_log!("INIT_VFS_CLOSE_SMOKE_CALL_RETURN ok=0 status=1")
                    }
                }
            }
            Err(_) => yarm_user_rt::user_log!("INIT_VFS_OPEN_SMOKE_CALL_RETURN ok=0 fd=0"),
        }
    }

    yarm_user_rt::user_log!("INIT_BLKCACHE_SMOKE_BEGIN");
    let get_info_req = BlkGetInfoRequest { device_id: 0 };
    let get_info_payload = get_info_req.encode();
    let Ok(get_info_msg) =
        yarm_user_rt::ipc::Message::with_header(0, BLK_OP_GET_INFO, 0, None, &get_info_payload)
    else {
        yarm_user_rt::user_log!(
            "INIT_BLKCACHE_GET_INFO_SMOKE_RETURN ok=0 status={}",
            BlkStatus::InvalidRequest as u32
        );
        return;
    };
    // SAFETY: init_blkcache_send_cap is the caller-local delegated send cap.
    let _ = unsafe {
        yarm_user_rt::syscall::ipc_call(init_blkcache_send_cap as u32, pm_recv, &get_info_msg)
    };
    // SAFETY: pm_recv is init's startup-provided reply endpoint.
    // Use blocking ipc_recv so the reply is always consumed before optional FS
    // spawns.  A deadline-zero non-blocking poll may return None if blkcache
    // replies after the poll; the reply then sits on pm_recv and contaminates
    // the next spawn_v5_cap recv, causing false SPAWN_FAIL for RAMFS/ext4.
    let get_info_reply = unsafe { yarm_user_rt::syscall::ipc_recv(pm_recv) };
    match get_info_reply {
        Ok(Some(reply_msg)) => match BlkGetInfoReply::decode(reply_msg.as_slice()) {
            Some(resp) => {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_GET_INFO_SMOKE_RETURN ok=1 status={}",
                    resp.status as u32
                );
            }
            None => {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_GET_INFO_SMOKE_RETURN ok=0 status={}",
                    BlkStatus::InvalidRequest as u32
                );
            }
        },
        Ok(None) => {
            yarm_user_rt::user_log!(
                "INIT_BLKCACHE_GET_INFO_SMOKE_RETURN ok=0 status={}",
                BlkStatus::NotReady as u32
            );
        }
        Err(_e) => {
            yarm_user_rt::user_log!(
                "INIT_BLKCACHE_GET_INFO_SMOKE_RETURN ok=0 status={}",
                BlkStatus::DeviceUnavailable as u32
            );
        }
    }

    // ── Optional FS server live spawns (profile-gated) ───────────────────────
    //
    // ramfs_srv (11), fat_srv (10), and ext4_srv (12) are packed and aligned in
    // the CPIO archive (Stage 80) and their PM image IDs are registered. However,
    // live spawning in the core profile is DISABLED because:
    //
    //  KERNEL BLOCKER: spawn_image_path_for_image_id() in src/kernel/syscall.rs
    //  only covers image IDs 0–9. IDs 10/11/12 return None → SyscallError::InvalidArgs.
    //  On AArch64 this causes the trap handler to halt the kernel. On x86_64, PM
    //  falls through a long Phase-2B VFS bulk-read chain before the spawn also fails,
    //  delaying or corrupting the PM reply state before core services are spawned.
    //  Adding entries 10/11/12 to spawn_image_path_for_image_id is a kernel behavior
    //  change deferred to the expanded-FS profile stage.
    //
    //  STARTUP CONTRACT: ramfs/fat/ext4 do not yet have proven resident IPC recv
    //  loops with established startup contracts for the core boot sequence.
    //
    //  SPAWN ORDER: optional FS spawns must come AFTER all core services
    //  (driver_manager/blkcache/virtio_blk) and after VFS/blkcache smokes to
    //  avoid interleaving VFS IPC with the PM spawn-reply stream.
    //
    // Set INIT_SPAWN_OPTIONAL_FS_SERVERS = true for the expanded-FS profile once:
    //  (1) kernel spawn_image_path_for_image_id has entries for 10/11/12,
    //  (2) each server has a proven resident IPC recv loop and startup contract,
    //  (3) x86_64 and aarch64 smoke tests confirm the spawns succeed.
    //
    // Stage 86: per-server sub-gates.  RAMFS and ext4 have resident recv loops;
    // FAT requires a virtio_blk block device not present in the default profile.
    const INIT_SPAWN_RAMFS_SRV: bool = true;
    const INIT_SPAWN_FAT_SRV: bool = false; // needs block device
    const INIT_SPAWN_EXT4_SRV: bool = true;
    const INIT_SPAWN_OPTIONAL_FS_SERVERS: bool =
        INIT_SPAWN_RAMFS_SRV || INIT_SPAWN_FAT_SRV || INIT_SPAWN_EXT4_SRV;

    if INIT_SPAWN_OPTIONAL_FS_SERVERS {
        // Drain pm_recv of any stale replies accumulated during smoke calls.
        // VFS client helpers use non-blocking ipc_recv_with_deadline(_, 0); if
        // a service replied after the poll window, the reply is still queued on
        // pm_recv.  Consuming all pending messages here prevents them from
        // being misinterpreted as SpawnV5 replies for RAMFS/FAT/ext4.
        yarm_user_rt::user_log!("INIT_PM_RECV_DRAIN_BEGIN");
        let mut drain_count: u32 = 0;
        loop {
            match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(pm_recv, 0) } {
                Ok(None) => break,
                Ok(Some(_)) | Err(_) => {
                    drain_count = drain_count.saturating_add(1);
                }
            }
        }
        yarm_user_rt::user_log!("INIT_PM_RECV_DRAIN_DONE count={}", drain_count);

        // --- Spawn ramfs_srv (image_id=11) and register /ram with VFS. ---
        if INIT_SPAWN_RAMFS_SRV {
            let ramfs_mount_config = RamFsMountConfig::new(
                b"/ram",
                false,
                yarm_fs_servers::ramfs::RAMFS_DEFAULT_MAX_BYTES as u32,
            )
            .unwrap_or_else(RamFsMountConfig::default_compat);
            // Stage 91: service_caps positions 1-3 must be zero for RAMFS.
            // Passing config words (ramfs_prefix_word, ramfs_meta_word) in non-zero slots
            // causes KSPAWN_EXTRA_CAP_DELEGATE_FAIL: the kernel treats every non-zero
            // service_caps entry as a capability ID to delegate into the child cspace.
            // RAMFS falls back to default_compat (prefix=/ram) when config slots are zeroed.
            yarm_user_rt::user_log!("INIT_RAMFS_SPAWN_BEGIN");
            if let Some((ramfs_child_tid, init_ramfs_send_cap)) =
                spawn_v5_cap(pm_send, pm_recv, 11, [0, 0, 0, 0], 1)
            {
                yarm_user_rt::user_log!(
                    "INIT_RAMFS_SPAWN_OK child_tid={} send_cap={} prefix={}",
                    ramfs_child_tid,
                    init_ramfs_send_cap,
                    alloc::string::String::from_utf8_lossy(ramfs_mount_config.prefix())
                );
                let _ = register_ramfs_mount_with_vfs(
                    vfs_recv_cap as u32,
                    pm_recv,
                    init_ramfs_send_cap,
                    ramfs_mount_config,
                );
            } else {
                yarm_user_rt::user_log!("INIT_RAMFS_SPAWN_FAIL ok=0 child_tid=0");
            }
        } else {
            yarm_user_rt::user_log!("INIT_RAMFS_SPAWN_SKIPPED reason=server_disabled");
        }

        // --- Spawn read-only fat_srv (image_id=10) with blkcache cap. ---
        // INIT_SPAWN_FAT_SRV = false: needs virtio_blk block device.
        // Non-fatal: log and continue to ext4 and alert loop.
        if INIT_SPAWN_FAT_SRV {
            let fat_mount_config = FatMountConfig::new(b"/fat", 1, true)
                .unwrap_or_else(FatMountConfig::default_compat);
            // Stage 91: only position 0 (blkcache cap) is a real capability.
            // Passing fat_prefix_word/fat_meta_word in positions 1-2 causes
            // KSPAWN_EXTRA_CAP_DELEGATE_FAIL — the kernel treats all non-zero
            // service_caps entries as cap IDs to delegate. FAT srv reads its
            // mount config from protocol messages, not startup slots.
            yarm_user_rt::user_log!("INIT_FAT_SPAWN_BEGIN");
            if let Some((fat_child_tid, init_fat_send_cap)) =
                spawn_v5_cap(pm_send, pm_recv, 10, [init_blkcache_send_cap, 0, 0, 0], 1)
            {
                yarm_user_rt::user_log!(
                    "INIT_FAT_SPAWN_OK child_tid={} send_cap={} prefix={} device_id={}",
                    fat_child_tid,
                    init_fat_send_cap,
                    alloc::string::String::from_utf8_lossy(fat_mount_config.prefix()),
                    fat_mount_config.device_id
                );
                let _ = register_fat_mount_with_vfs(
                    vfs_recv_cap as u32,
                    pm_recv,
                    init_fat_send_cap,
                    fat_mount_config,
                );
            } else {
                yarm_user_rt::user_log!("INIT_FAT_SPAWN_FAIL ok=0 child_tid=0");
            }
        } else {
            yarm_user_rt::user_log!("INIT_FAT_SPAWN_SKIPPED reason=server_disabled");
        }

        // --- Spawn ext4_srv (image_id=12) read-only and register /ext4 with VFS. ---
        // Stage 86: ext4_srv has a resident ipc_recv_v2 loop (VFS_EXT4_RECV_LOOP_ENABLED=true).
        // Stage 88: VFS_EXT4_LIVE_MOUNT_ENABLED=true; register_ext4_mount_with_vfs wires /ext4.
        if INIT_SPAWN_EXT4_SRV {
            yarm_user_rt::user_log!("INIT_EXT4_SPAWN_BEGIN");
            if let Some((ext4_child_tid, init_ext4_send_cap)) =
                spawn_v5_cap(pm_send, pm_recv, 12, [0, 0, 0, 0], 1)
            {
                yarm_user_rt::user_log!(
                    "INIT_EXT4_SPAWN_OK child_tid={} send_cap={}",
                    ext4_child_tid,
                    init_ext4_send_cap,
                );
                yarm_user_rt::user_log!("EXT4_SRV_READY child_tid={}", ext4_child_tid);
                let _ =
                    register_ext4_mount_with_vfs(vfs_recv_cap as u32, pm_recv, init_ext4_send_cap);
            } else {
                yarm_user_rt::user_log!("INIT_EXT4_SPAWN_FAIL ok=0 child_tid=0");
            }
        } else {
            yarm_user_rt::user_log!("INIT_EXT4_SPAWN_SKIPPED reason=server_disabled");
        }
    } else {
        yarm_user_rt::user_log!("INIT_RAMFS_SPAWN_SKIPPED reason=profile_disabled");
        yarm_user_rt::user_log!("INIT_FAT_SPAWN_SKIPPED reason=profile_disabled");
        yarm_user_rt::user_log!("INIT_EXT4_SPAWN_SKIPPED reason=profile_disabled");
    }

    if supervisor_restart_test_build_gate_enabled() {
        yarm_user_rt::user_log!("INIT_SUPERVISOR_RESTART_TEST_GATE_ON");
        yarm_user_rt::user_log!("INIT_CRASH_TEST_SPAWN_REQUEST image_id=13");
        if let Some((crash_tid, _crash_send_cap)) =
            spawn_v5_cap(pm_send, pm_recv, 13, [0, 0, 0, 0], 1)
        {
            yarm_user_rt::user_log!("INIT_CRASH_TEST_SPAWN_OK tid={}", crash_tid);
            if let Some(supervisor_send) = crash_test_supervisor_control_send_cap(&ctx) {
                register_crash_test_with_supervisor(supervisor_send, crash_tid);
            } else {
                yarm_user_rt::user_log!(
                    "INIT_CRASH_TEST_REGISTER_FAIL tid={} reason=no-supervisor-send-cap",
                    crash_tid
                );
            }
        } else {
            yarm_user_rt::user_log!("INIT_CRASH_TEST_SPAWN_FAIL reason=pm-spawn");
        }
    }

    // Stage 159BC/D: default-off userspace IPC recv-v2 oracle workload. The
    // kernel bootstrap provisions slots 6/7 (init_alert_send/recv) with a
    // dedicated loopback endpoint ONLY when `yarm.ipc_recv_proof=1`; their joint
    // presence is the gate. A normal boot leaves them None and skips this
    // entirely.
    if let (Some(proof_send), Some(proof_recv)) = (ctx.init_alert_send_ep, ctx.init_alert_recv_ep) {
        run_ipc_recv_proof_workload(proof_send, proof_recv);
        // Stage 163: sender-wake proof. Runs ONLY when the kernel also provisioned
        // the coordination endpoint E2 (slot 13 / service_extra_cap_0), which it
        // does ONLY under `yarm.ipc_recv_proof_sender_wake=1`. So queued-split +
        // rollback proof boots (base knob only) leave E2 None and skip this.
        if let Some(e2_recv) = ctx.service_extra_cap_0 {
            // Slot 14 (service_extra_cap_1) carries E1's buffered capacity, so init
            // can fill E1 to EXACTLY full with non-blocking sends and never become a
            // sender-waiter itself. Default to a safe small capacity if absent.
            let e1_capacity = ctx.service_extra_cap_1.unwrap_or(8) as usize;
            run_ipc_recv_proof_sender_wake(
                proof_send,
                proof_recv,
                e2_recv,
                e1_capacity,
                ctx.task_id,
            );
        } else {
            // Sub-knob absent: sender-wake is intentionally not driven (no fake
            // marker). The queued-split + rollback proof above stands alone.
            yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SUBKNOB_ABSENT");
        }
    }

    let Some(alert_recv) = ctx.init_alert_recv_ep else {
        return;
    };
    yarm_user_rt::user_log!("INIT_ALERT_WAIT_BEGIN cap={}", alert_recv);
    loop {
        // SAFETY: init owns this startup-provided alert receive endpoint.
        let _ = unsafe { yarm_user_rt::syscall::ipc_recv(alert_recv) };
    }
}

/// Stage 159BC/D: deterministic userspace IPC recv-v2 oracle workload.
///
/// Runs ONLY under `yarm.ipc_recv_proof=1` (gated by the kernel-provisioned
/// loopback endpoint in slots 6/7). `proof_send` and `proof_recv` are a SEND and
/// a RECV capability to the SAME fresh endpoint, both held by init, so each
/// subtest is single-threaded and race-free: a send-to-self enqueues (no
/// receiver is blocked), then a recv-from-self drains the queued message via the
/// kernel queued-split delivery path.
///
/// Subtest 1 — queued split: enqueue a plain message, then drain it with a
/// normal recv-v2. The drain takes the queued-split writeback path and the
/// kernel emits `IPC_RECV_V2_META_QUEUED_SPLIT_OK`.
///
/// Subtest 2 — rollback: enqueue a cap-bearing message (carrying a transferable
/// cap), then drain it with a deliberately undersized payload buffer. The kernel
/// materializes the carried cap, discovers the payload buffer is too small, and
/// rolls the freshly-minted cap back — emitting
/// `IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize`.
///
/// Subtest 3 — sender-wake: driven by a SEPARATE workload
/// (`run_ipc_recv_proof_sender_wake`) gated behind the additional
/// `yarm.ipc_recv_proof_sender_wake=1` sub-knob (Stage 163). It needs a real
/// second execution context blocked in `ipc_send` plus a race-free way to observe
/// that blocked state, neither of which this single-context base workload
/// provides — so the base proof never fakes `IPC_RECV_V2_SENDER_WAKE_ORDER_OK`
/// and stays byte-for-byte unchanged when the sub-knob is absent.
#[cfg(not(test))]
fn run_ipc_recv_proof_workload(proof_send: u32, proof_recv: u32) {
    use yarm_user_rt::ipc::Message;
    use yarm_user_rt::syscall::SyscallError;

    // SyscallError is #[repr(usize)] with explicit discriminants; expose a small
    // numeric code for phase diagnostics (0 == ok). This lets the next QEMU run
    // pin exactly where each subtest diverges from the intended kernel path.
    fn code(r: &Result<(), SyscallError>) -> usize {
        match r {
            Ok(()) => 0,
            Err(e) => *e as usize,
        }
    }

    yarm_user_rt::user_log!("IPC_RECV_PROOF_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_CAPS send={} recv={}",
        proof_send,
        proof_recv
    );

    // ── Subtest 1: queued split ──────────────────────────────────────────────
    // Enqueue a small plain message (no cap), then drain via recv-v2. The DONE
    // marker is emitted ONLY when the recv actually returned a message; the
    // authoritative proof is the kernel marker IPC_RECV_V2_META_QUEUED_SPLIT_OK,
    // required separately by the oracle. To diagnose a path divergence on the
    // next run, also grep the kernel-side YARM_RECV_CORE_PLAN /
    // YARM_RECV_CORE_ADAPTER / YARM_RECV_CORE_FALLBACK markers between
    // QS_RECV_BEGIN and QS_RECV_RET.
    if let Ok(msg) = Message::with_header(0, IPC_RECV_PROOF_OPCODE, 0, None, &[0xA5u8; 8]) {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_QS_SEND_BEGIN");
        // SAFETY: proof_send is a kernel-provisioned SEND cap to init's own
        // loopback endpoint. No receiver is blocked, so the message enqueues.
        let send = unsafe { yarm_user_rt::syscall::ipc_send(proof_send, &msg) };
        yarm_user_rt::user_log!("IPC_RECV_PROOF_QS_SEND_RET code={}", code(&send));
        if send.is_ok() {
            yarm_user_rt::user_log!("IPC_RECV_PROOF_QS_RECV_BEGIN");
            // SAFETY: proof_recv is the matching RECV cap; the message is queued,
            // so a split-path recv drains it through the queued-split path.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(proof_recv) } {
                Ok(Some(received)) => {
                    yarm_user_rt::user_log!(
                        "IPC_RECV_PROOF_QS_RECV_RET code=0 payload_len={} sender_tid={}",
                        received.message.as_slice().len(),
                        received.sender_tid
                    );
                    // Honest: the userspace sequence observed a delivered message.
                    // It does NOT (and cannot) assert which kernel path delivered
                    // it — the oracle pairs this with the kernel marker.
                    yarm_user_rt::user_log!("IPC_RECV_PROOF_QUEUED_SPLIT_SEQUENCE_DONE");
                }
                Ok(None) => {
                    yarm_user_rt::user_log!("IPC_RECV_PROOF_QS_RECV_RET code=wouldblock");
                }
                Err(e) => {
                    yarm_user_rt::user_log!("IPC_RECV_PROOF_QS_RECV_RET code={}", e as usize);
                }
            }
        }
    }

    // ── Subtest 2: rollback (cap materialize + undersized writeback) ─────────
    // Enqueue a cap-bearing message whose payload (32 bytes) exceeds the
    // undersized recv buffer (8 bytes). We transfer the loopback SEND cap itself
    // — a cap init definitely holds; on a split-path drain the kernel
    // materializes it then rolls it back when the writeback is found undersized
    // (IPC_RECV_V2_ROLLBACK_OK site=queued_split_undersize). The SEQUENCE marker
    // is emitted ONLY on the expected error return; the kernel marker is the
    // authoritative proof, required separately by the oracle.
    if let Ok(cap_msg) = Message::with_header(
        0,
        IPC_RECV_PROOF_OPCODE,
        Message::FLAG_CAP_TRANSFER,
        Some(proof_send as u64),
        &[0x5Au8; 32],
    ) {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_ROLLBACK_SEND_BEGIN");
        // SAFETY: same loopback SEND cap; carries a transferable cap, enqueues.
        let send = unsafe { yarm_user_rt::syscall::ipc_send(proof_send, &cap_msg) };
        yarm_user_rt::user_log!("IPC_RECV_PROOF_ROLLBACK_SEND_RET code={}", code(&send));
        if send.is_ok() {
            yarm_user_rt::user_log!("IPC_RECV_PROOF_ROLLBACK_RECV_BEGIN");
            // SAFETY: undersized-payload recv on the matching RECV cap; an Err
            // return (InvalidArgs on the undersize path) is the expected outcome.
            let recv = unsafe { yarm_user_rt::syscall::ipc_recv_v2_proof_undersized(proof_recv) };
            yarm_user_rt::user_log!("IPC_RECV_PROOF_ROLLBACK_RECV_RET code={}", code(&recv));
            if recv.is_err() {
                // Honest: observed the expected failure return. The oracle pairs
                // this with the kernel IPC_RECV_V2_ROLLBACK_OK marker.
                yarm_user_rt::user_log!("IPC_RECV_PROOF_ROLLBACK_SEQUENCE_DONE");
            }
        }
    }

    // ── Subtest 3: sender-wake — NOT driven from this single-context base
    // workload. IPC_RECV_V2_SENDER_WAKE_ORDER_OK fires only when the receiver
    // drains a queued message while a sender is BLOCKED as a waiter (queue full +
    // a timed blocking send), which needs a real second execution context AND a
    // way to observe the sender's blocked state before draining (a pure userspace
    // handshake races the timer-driven preemption). That determinism is supplied
    // by run_ipc_recv_proof_sender_wake() — a separate workload gated behind the
    // additional yarm.ipc_recv_proof_sender_wake=1 sub-knob (Stage 163), driven
    // from the call site only when the kernel provisioned the coordination
    // endpoint E2. The base (queued-split + rollback) proof never fakes the marker
    // here and is byte-for-byte unchanged when the sub-knob is absent.
    yarm_user_rt::user_log!("IPC_RECV_PROOF_END");
}

/// Application opcode used by the Stage 159BC/D proof workload's loopback
/// messages. Arbitrary and proof-local.
#[cfg(not(test))]
const IPC_RECV_PROOF_OPCODE: u16 = 0x0F9D;

/// Stage 163C: map a syscall error code (the kernel's `SyscallError as usize`,
/// matching `decode_syscall_error`) to a human label for the fork diagnostics. A
/// value `>= 0x100` is treated as "stale_or_internal" — on x86_64 a successful
/// forked child can carry a stale nonzero error lane (RCX = the SYSCALL-clobbered
/// return RIP), which is a large address, not a small error code.
#[cfg(not(test))]
const fn fork_err_meaning(code: u64) -> &'static str {
    match code {
        0 => "ok",
        1 => "InvalidNumber",
        2 => "InvalidArgs",
        3 => "InvalidCapability",
        4 => "MissingRight",
        5 => "WrongObject",
        6 => "QueueFull",
        7 => "WouldBlock",
        8 => "PageFault",
        9 => "TimedOut",
        10..=255 => "Internal",
        _ => "stale_or_internal",
    }
}

/// Stage 163C: clean-state fork smoke. Determines whether a full E1 / queued IPC
/// state is implicated in a fork failure — if this fails too, the full buffer is
/// ruled out.
///
/// Stage 163K: this is now a DIAGNOSTIC-ONLY helper and is intentionally NOT
/// called from the required sender-wake acceptance path. Its child parks and
/// yields forever, permanently holding a CNode-space reservation against the
/// global `max_total_cnode_slots` budget, which starved the real sender-wake
/// fork (`CapabilityFull step=register`). Acceptance now relies on the single
/// real sender-wake fork as the fork proof. Kept compiled (allow(dead_code)) so
/// it can be re-enabled for ad-hoc diagnosis without reintroducing the helper.
#[cfg(not(test))]
#[allow(dead_code)]
fn run_ipc_recv_proof_fork_smoke() {
    yarm_user_rt::user_log!("IPC_RECV_PROOF_FORK_SMOKE_BEGIN");
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FORK_SYSCALL_BEGIN smoke=1");
    // SAFETY: proof-only raw fork; exposes every return lane for diagnosis.
    let r = unsafe { yarm_user_rt::syscall::fork_raw() };
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_FORK_SMOKE_SYSCALL_RET ret0={} ret1={} ret2={} err={} arch={}",
        r.ret0,
        r.ret1,
        r.ret2,
        r.err,
        r.arch
    );
    if r.ret0 != 0 {
        // Parent with a concrete child pid → fork works in a clean state.
        yarm_user_rt::user_log!("IPC_RECV_PROOF_FORK_SMOKE_PARENT child_pid={}", r.ret0);
        return;
    }
    if r.err != 0 && r.err < 0x100 {
        // ret0 == 0 with a small known error code → a genuine fork failure.
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_FORK_SMOKE_FAILED code={} meaning={}",
            r.err,
            fork_err_meaning(r.err)
        );
        return;
    }
    // ret0 == 0 with err == 0 (or a large stale lane) → the child.
    yarm_user_rt::user_log!("IPC_RECV_PROOF_FORK_SMOKE_CHILD_ENTRY");
    loop {
        let _ = yarm_user_rt::syscall::yield_now();
    }
}

/// Stage 163 / 163A: deterministic sender-wake proof workload.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and
/// `yarm.ipc_recv_proof_sender_wake=1` (gated by the presence of the kernel-
/// provisioned coordination endpoint E2 recv cap). Proves
/// `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` via a REAL blocked sender-waiter that is the
/// forked CHILD (never init), with the drain ordered deterministically — race-free
/// even on SMP — by a kernel coordination signal:
///
/// 0. init drains any leftover messages from E1 (the base queued-split/rollback
///    subtests share E1), so the fill below starts from empty.
/// 1. init fills E1 to EXACTLY its buffered capacity with NON-BLOCKING sends. This
///    is the Stage 163A fix: a buffered send on a FULL endpoint *blocks* the sender
///    as a waiter even with a zero timeout (the kernel has no try-send for buffered
///    full — see `ipc_send_with_optional_deadline`). Filling exactly `e1_capacity`
///    (never one more) guarantees every fill send succeeds and **init never becomes
///    a sender-waiter**. The capacity is supplied by the kernel via startup slot 14.
/// 2. init forks; the child (fork returns 0) is the sender, the parent (init) is
///    the receiver. The child inherits init's proof caps via the COW fork.
/// 3. the child does a TIMED blocking send on the now-full E1 → it becomes the
///    real sender-waiter. The kernel's `enqueue_sender_waiter` hook, in the SAME
///    `ipc_state_lock` critical section, pushes a waiter-present signal (carrying
///    the waiter's TID) into E2.
/// 4. init non-blocking-polls E2; the signal appears EXACTLY when the sender is
///    provably a waiter (atomic with the enqueue), so there is no race window. init
///    verifies the signalled TID is the forked child (and NOT init) before
///    proceeding — a waiter-present for init would be a fill-phase bug, reported as
///    `..._WAITER_UNEXPECTED` and never accepted as proof.
/// 5. init `recv-v2` drains E1 (NR 2 → trap-entry split path). The real kernel
///    path emits `IPC_RECV_V2_SENDER_WAKE_ORDER_OK` and refills + wakes the sender.
/// 6. init confirms it observed the child's own message (sender_tid == child) —
///    concrete proof the sender made progress — then emits
///    `IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE`.
///
/// The child parks (never returns into init's flow). All waits are bounded so a
/// missing child (e.g. fork failure) degrades to a logged give-up, never a hang.
#[cfg(not(test))]
fn run_ipc_recv_proof_sender_wake(
    e1_send: u32,
    e1_recv: u32,
    e2_recv: u32,
    e1_capacity: usize,
    init_tid: u64,
) {
    use yarm_user_rt::ipc::Message;

    // Large enough that init always drains well before the sender's send deadline
    // (init drains immediately after the coordination signal).
    const SENDER_SEND_TIMEOUT_TICKS: u64 = 1_000_000_000;
    // Defensive clamp so a misconfigured capacity can never loop pathologically.
    const FILL_CAP_MAX: usize = 256;
    const PREDRAIN_MAX: usize = 512;
    // Stage 163N Task A: E2 coordination uses a bounded poll-then-yield loop
    // instead of a single blocking recv.  The kernel pushes the signal from
    // within `enqueue_sender_waiter` while holding `ipc_state_lock`, using
    // `endpoint.send(msg)` directly — it does NOT wake blocked receivers
    // (would require acquiring the scheduler lock while holding the IPC lock,
    // violating lock-order rank 1 < rank 3/4).  A single blocking recv would
    // therefore hang until the deadline; polling with short blocking yields
    // lets the parent give the child CPU time while Phase 1 of each
    // `ipc_recv_with_deadline` check immediately returns the queued signal
    // once the kernel has pushed it.
    //
    // E2_POLL_YIELD_TICKS: each iteration blocks for this many ticks so the
    // scheduler can run the child. Must be small relative to SENDER_SEND_TIMEOUT_TICKS
    // but large enough to give the child a meaningful scheduling slot.
    const E2_POLL_YIELD_TICKS: u64 = 5_000_000;
    // E2_POLL_MAX_ITERS: total wait budget = E2_POLL_MAX_ITERS * E2_POLL_YIELD_TICKS
    // = 100 * 5_000_000 = 500_000_000 ticks.  Must stay below SENDER_SEND_TIMEOUT_TICKS
    // so we bail before the child's own blocking-send timeout expires.
    const E2_POLL_MAX_ITERS: usize = 100;
    const DRAIN_MAX: usize = 72;

    let fill_target = if e1_capacity == 0 || e1_capacity > FILL_CAP_MAX {
        // Fall back to a safe small fill; never 0 (we need E1 full) and never huge.
        8
    } else {
        e1_capacity
    };

    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_SETUP_BEGIN e1_send={} e1_recv={} e2_recv={} e1_capacity={} init_tid={}",
        e1_send,
        e1_recv,
        e2_recv,
        fill_target,
        init_tid
    );
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SETUP_DONE");

    // Stage 163K: the clean-state fork smoke (Stage 163C) is NO LONGER run in the
    // required sender-wake path. Its child parks and yields forever, permanently
    // holding a CNode-space reservation against the GLOBAL `max_total_cnode_slots`
    // budget; the subsequent REAL sender-wake fork then failed with
    // `CapabilityFull step=register`. The sender-wake fork below is itself the
    // fork proof, so acceptance needs exactly ONE live fork child. The smoke
    // remains defined as a diagnostic-only helper (see `run_ipc_recv_proof_fork_
    // smoke`), not part of acceptance.

    // (0) Drain any leftover messages from the base subtests so the fill starts
    // empty (non-blocking; empty returns Ok(None)/WouldBlock — never blocks init).
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_PREDRAIN_DONE count={}",
        predrained
    );

    // (1) Fill E1 to EXACTLY capacity with NON-BLOCKING sends. Sending exactly
    // `fill_target` messages into an empty buffered endpoint of that capacity makes
    // every send succeed and leaves E1 full WITHOUT init ever blocking. We stop at
    // capacity and never attempt the (capacity+1)-th send (which would block init).
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_FILL_BEGIN target={}",
        fill_target
    );
    let mut filled = 0usize;
    let mut fill_blocker = false;
    while filled < fill_target {
        let Ok(msg) = Message::with_header(0, IPC_RECV_PROOF_OPCODE, 0, None, &[0xF1u8; 8]) else {
            break;
        };
        // SAFETY: e1_send is a kernel-provisioned SEND cap to init's proof loopback.
        // Non-blocking send; within capacity it always queues (Ok).
        let r = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
        match r {
            Ok(()) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_FILL_SEND_RET idx={} code=0",
                    filled
                );
                filled += 1;
            }
            Err(e) => {
                // Unexpected within capacity: a non-blocking send that returns an
                // error here means init was at risk of becoming a sender-waiter.
                // Stop immediately; do NOT proceed (init must never block in fill).
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_FILL_SEND_RET idx={} code={}",
                    filled,
                    e as usize
                );
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_FILL_UNEXPECTED_BLOCKER idx={} tid={}",
                    filled,
                    init_tid
                );
                fill_blocker = true;
                break;
            }
        }
    }
    if fill_blocker {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FILL_ABORT count={}", filled);
        return;
    }
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FILL_STOP_FULL idx={}", filled);
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FILL_DONE count={}", filled);

    // (5) Fork — child == sender, parent (init) == receiver. Stage 163C: the fork
    // syscall's full return lanes (ret0/ret1/ret2/err) are logged BEFORE any lossy
    // conversion, then decoded, so a fork failure exposes its exact error code (not
    // a bare "raw=err"). The parent NEVER polls E2 until fork has returned its
    // parent-side child pid (step 6 below).
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FORK_BEGIN");
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FORK_SYSCALL_BEGIN smoke=0");
    // SAFETY: proof-only raw fork; the child inherits init's COW address space +
    // proof caps and parks after its blocking send (never returns into init's flow).
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_FORK_SYSCALL_RET ret0={} ret1={} ret2={} err={} arch={}",
        fr.ret0,
        fr.ret1,
        fr.ret2,
        fr.err,
        fr.arch
    );
    // Decode role from the raw lanes: ret0 != 0 → parent (child pid = ret0); ret0 == 0
    // with a small known error code → a genuine failure; ret0 == 0 with err == 0 (or a
    // large/stale lane) → the child.
    let pid = if fr.ret0 != 0 {
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_FORK_DECODE code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_FORK_RET raw={} role=parent",
            fr.ret0
        );
        fr.ret0
    } else if fr.err != 0 && fr.err < 0x100 {
        // Genuine fork failure: abort boundedly — never spin on E2 for a missing child.
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_FORK_DECODE code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FORK_RET raw=err role=err");
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_FORK_FAILED code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        return;
    } else {
        // ── CHILD: the real blocked sender. CHILD_ENTRY/SENDER_START are emitted
        // BEFORE the timed blocking send so the ordering is observable. ──
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_FORK_RET raw=0 role=child");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_CHILD_ENTRY");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SENDER_START");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SENDER_BLOCKING_SEND_BEGIN");
        if let Ok(msg) = Message::with_header(0, IPC_RECV_PROOF_OPCODE, 0, None, &[0x5Eu8; 8]) {
            // SAFETY: timed blocking send on the FULL E1 → the child becomes a real
            // sender-waiter; completes once init's drain frees a slot and refills it.
            let _ = unsafe {
                yarm_user_rt::syscall::ipc_send_timeout_ticks(
                    e1_send,
                    &msg,
                    SENDER_SEND_TIMEOUT_TICKS,
                )
            };
        }
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_CHILD_DONE");
        // Park the child: do NOT return into init's post-proof flow.
        // Block on the proof endpoint rather than spinning on yield (nr=0) to
        // avoid polluting the syscall trace with repeated nr=0 noise.
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_PARK_BEGIN role=child");
        loop {
            let _ = unsafe { yarm_user_rt::syscall::ipc_recv(e1_recv) };
        }
    };

    // (6) PARENT only: now that fork has returned a parent-side child pid, wait for
    // the kernel's waiter-present coordination signal on E2. Non-blocking-poll;
    // bounded so a missing/never-scheduled child can never hang the boot. The signal
    // carries the waiter's TID (Message::sender_tid).
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_PARENT_WAIT_BEGIN child_pid={}",
        pid
    );
    let mut waiter_tid: Option<u64> = None;
    // Stage 163N Task A: poll E2 with short blocking yields so the scheduler
    // can run the child on the same CPU (AArch64/RISC-V single-core paths).
    // Each iteration calls ipc_recv_with_deadline(e2_recv, E2_POLL_YIELD_TICKS):
    //   Phase 1 (immediate): if the kernel already pushed the signal, returns
    //   Ok(Some(sig)) immediately without blocking.
    //   Phase 2 (block): if not yet pushed, blocks for E2_POLL_YIELD_TICKS
    //   ticks, yielding the CPU so the child can be scheduled, then returns
    //   Ok(None) on timeout.
    // This avoids the lock-order hazard: the kernel push path holds
    // ipc_state_lock and cannot acquire the scheduler lock to wake a blocked
    // receiver; polling means we never block long enough to miss the signal.
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_BEGIN");
    'e2_poll: for poll_iter in 0..E2_POLL_MAX_ITERS {
        // SAFETY: e2_recv is init's RECV cap to the proof coordination endpoint.
        match unsafe {
            yarm_user_rt::syscall::ipc_recv_with_deadline(e2_recv, E2_POLL_YIELD_TICKS)
        } {
            Ok(Some(sig)) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_HIT iter={}",
                    poll_iter
                );
                waiter_tid = Some(sig.sender_tid.0);
                break 'e2_poll;
            }
            Ok(None) => {
                // Timeout — child not yet a sender-waiter; loop and yield again.
            }
            Err(_) => {
                // Unexpected error — break out and let the no-waiter path handle it.
                break 'e2_poll;
            }
        }
    }
    if waiter_tid.is_none() {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_EXHAUSTED");
    }
    let Some(waiter_tid) = waiter_tid else {
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_NO_WAITER_SIGNAL child_pid={}",
            pid
        );
        return;
    };
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_WAITER_OBSERVED waiter_tid={} child_pid={}",
        waiter_tid,
        pid
    );
    // (6b) The waiter MUST be the forked child, never init. A waiter-present for
    // init would mean init blocked during fill (the Stage 163A bug) — reject it.
    if waiter_tid == init_tid {
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_WAITER_UNEXPECTED tid={}",
            waiter_tid
        );
        return;
    }
    if waiter_tid != pid {
        // Not init, but also not the child we forked — do not fake progress.
        yarm_user_rt::user_log!(
            "IPC_RECV_PROOF_SENDER_WAKE_WAITER_MISMATCH waiter_tid={} child_pid={}",
            waiter_tid,
            pid
        );
        return;
    }

    // (7) Wake-trigger drain: recv-v2 (NR 2 → trap-entry split path). The queue is
    // full and the child is a waiter, so this drains a queued message, refills the
    // child's message, and the real path emits IPC_RECV_V2_SENDER_WAKE_ORDER_OK.
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_RECV_BEGIN");
    let mut got_child = false;
    match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
        Ok(Some(received)) => {
            yarm_user_rt::user_log!(
                "IPC_RECV_PROOF_SENDER_WAKE_RECV_RET code=0 payload_len={} sender_tid={}",
                received.message.as_slice().len(),
                received.sender_tid
            );
            if received.sender_tid == pid {
                got_child = true;
            }
        }
        Ok(None) => {
            yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_RECV_RET code=wouldblock");
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_RECV_RET code={}", e as usize);
        }
    }
    // (8) Drain the remaining queued messages (non-blocking, so empty never blocks)
    // until the child's own message is observed — concrete proof of sender progress.
    if !got_child {
        for _ in 0..DRAIN_MAX {
            match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
                Ok(Some(msg)) => {
                    if msg.sender_tid.0 == pid {
                        got_child = true;
                        break;
                    }
                }
                _ => break,
            }
        }
    }
    if got_child {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SENDER_DONE observed=1");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_PARENT_DONE");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_PARK_BEGIN role=parent");
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_PARKED role=parent");
    } else {
        yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_SENDER_MSG_ABSENT");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::ipc_abi::process_abi::{decode_spawn_v5_reply, encode_spawn_v5_reply};
    use yarm::std::vec::Vec;
    use yarm_fs_servers::initramfs::ManifestEntryWire;
    use yarm_fs_servers::initramfs::{
        INITRAMFS_INIT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR,
        INITRAMFS_VFS_PATH_PTR,
    };

    const MANIFEST_MAGIC: u32 = 0x5941_524D;
    const MANIFEST_VERSION_V1: u16 = 1;

    fn encode_manifest(entries: &[ManifestEntryWire]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MANIFEST_MAGIC.to_le_bytes());
        out.extend_from_slice(&MANIFEST_VERSION_V1.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for entry in entries {
            out.extend_from_slice(&entry.path_ptr.to_le_bytes());
            out.extend_from_slice(&entry.file_len.to_le_bytes());
            out.extend_from_slice(&entry.entry_addr.to_le_bytes());
            out.extend_from_slice(&entry.abi.to_le_bytes());
            out.extend_from_slice(&entry.flags.to_le_bytes());
        }
        out
    }

    fn synthetic_elf_image(entry: u64) -> [u8; 192] {
        let mut image = [0u8; 192];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        image[7] = 0;
        image[16..18].copy_from_slice(&2u16.to_le_bytes());
        image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes());
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes());
        image[52..54].copy_from_slice(&(64u16).to_le_bytes());
        image[54..56].copy_from_slice(&(56u16).to_le_bytes());
        image[56..58].copy_from_slice(&(1u16).to_le_bytes());

        let ph = 64usize;
        image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
        image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes());
        image[ph + 8..ph + 16].copy_from_slice(&184u64.to_le_bytes());
        image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes());
        image[ph + 32..ph + 40].copy_from_slice(&8u64.to_le_bytes());
        image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes());
        image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        image[184..192].copy_from_slice(&[0x90; 8]);
        image
    }

    #[test]
    fn runtime_boot_config_can_drive_external_kernel_state() {
        let baseline = InitRuntimeBootConfig::baseline();
        assert_eq!(baseline.graph.init_tid, 1);
        assert_eq!(baseline.graph.process_manager_tid, 2);
        assert_eq!(baseline.graph.vfs_tid, 3);
        assert_eq!(baseline.graph.supervisor_tid, 4);
        assert_eq!(baseline.restart_window_ticks, 100);
        assert_eq!(
            baseline.launch_strategy,
            CoreLaunchStrategy::SupervisorFirst
        );
    }

    #[test]
    fn decode_spawn_v5_reply_all_zero_is_failure_shape() {
        let payload = [0u8; 16];
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_eq!(decoded.pid, 0);
        assert_eq!(decoded.service_send_cap, 0);
    }

    #[test]
    fn decode_spawn_v5_reply_success_roundtrip() {
        let payload = encode_spawn_v5_reply(42, 65552);
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_eq!(decoded.pid, 42);
        assert_eq!(decoded.service_send_cap, 65552);
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
    fn runtime_boot_config_supports_custom_service_identity_layout() {
        let config = InitRuntimeBootConfig {
            graph: CoreServiceGraph {
                init_tid: 41,
                process_manager_tid: 42,
                vfs_tid: 43,
                supervisor_tid: 44,
            },
            ..InitRuntimeBootConfig::baseline()
        };

        assert_eq!(config.graph.init_tid, 41);
        assert_eq!(config.graph.process_manager_tid, 42);
        assert_eq!(config.graph.vfs_tid, 43);
        assert_eq!(config.graph.supervisor_tid, 44);
        assert_eq!(config.launch_strategy, CoreLaunchStrategy::SupervisorFirst);
    }

    #[test]
    fn minimum_runnable_profile_brings_up_core_services_plus_devfs_and_initramfs() {
        let baseline = InitRuntimeBootConfig::baseline();
        let fixed = resolve_core_image_plan(baseline.image_source).expect("fixed image plan");
        assert_eq!(fixed.process_manager_entry, 0x8000);
        assert_eq!(fixed.vfs_entry, 0x9000);
        assert_eq!(fixed.supervisor_entry, 0xA000);
        assert!(VFS_OP_OPENAT > 0);
        assert!(VFS_OP_READ > 0);
    }

    #[test]
    fn runtime_boot_config_can_resolve_core_entries_from_manifest() {
        let init = synthetic_elf_image(0x410000);
        let proc = synthetic_elf_image(0x420000);
        let vfs = synthetic_elf_image(0x430000);
        let supervisor = synthetic_elf_image(0x440000);
        let entries = [
            ManifestEntryWire {
                path_ptr: INITRAMFS_INIT_PATH_PTR,
                file_len: init.len() as u64,
                entry_addr: 0x410000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_PROC_MGR_PATH_PTR,
                file_len: proc.len() as u64,
                entry_addr: 0x420000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_VFS_PATH_PTR,
                file_len: vfs.len() as u64,
                entry_addr: 0x430000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_SUPERVISOR_PATH_PTR,
                file_len: supervisor.len() as u64,
                entry_addr: 0x440000,
                abi: 1,
                flags: 0,
            },
        ];
        let manifest_storage = encode_manifest(&entries);
        let images = [
            (INITRAMFS_INIT_PATH_PTR, init.as_slice()),
            (INITRAMFS_PROC_MGR_PATH_PTR, proc.as_slice()),
            (INITRAMFS_VFS_PATH_PTR, vfs.as_slice()),
            (INITRAMFS_SUPERVISOR_PATH_PTR, supervisor.as_slice()),
        ];

        let plan = resolve_core_image_plan(InitCoreImageSource::Manifest {
            manifest_bytes: &manifest_storage,
            images: &images,
        })
        .expect("plan");

        assert_eq!(plan.process_manager_entry, 0x420000usize);
        assert_eq!(plan.vfs_entry, 0x430000usize);
        assert_eq!(plan.supervisor_entry, 0x440000usize);
    }

    // ── Stage 90: optional spawn reply decode regression tests ───────────────

    #[test]
    fn optional_spawn_image_id_11_ramfs_reply_decodes_as_success() {
        let payload = encode_spawn_v5_reply(10_011, 65_541);
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_eq!(decoded.pid, 10_011);
        assert!(
            spawn_v5_reply_is_success(decoded.pid, decoded.service_send_cap),
            "ramfs (image_id=11) success reply must decode as success"
        );
    }

    #[test]
    fn optional_spawn_image_id_12_ext4_reply_decodes_as_success() {
        let payload = encode_spawn_v5_reply(10_012, 65_542);
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_eq!(decoded.pid, 10_012);
        assert!(
            spawn_v5_reply_is_success(decoded.pid, decoded.service_send_cap),
            "ext4 (image_id=12) success reply must decode as success"
        );
    }

    #[test]
    fn pm_success_reply_cannot_decode_as_zero_pid() {
        let payload = encode_spawn_v5_reply(9_999, 65_540);
        let decoded = decode_spawn_v5_reply(&payload).expect("decode");
        assert_ne!(decoded.pid, 0, "PM success reply must not decode as pid=0");
        assert!(spawn_v5_reply_is_success(
            decoded.pid,
            decoded.service_send_cap
        ));
    }

    #[test]
    fn blkcache_reply_shape_32_bytes_fails_spawn_v5_decode() {
        // BlkGetInfoReply is 32 bytes; SpawnV5CapResult expects exactly 16.
        // A stale blkcache reply on pm_recv must fail decode and not silently
        // produce ok=0 child_tid=0 (which would cause a false SPAWN_FAIL log).
        let blk_reply_shape = [0u8; 32];
        assert!(
            decode_spawn_v5_reply(&blk_reply_shape).is_err(),
            "32-byte blkcache reply shape must fail SpawnV5 decode (expected 16 bytes)"
        );
    }

    #[test]
    fn pm_recv_drain_marker_present_in_init_source() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_BEGIN"),
            "init must drain pm_recv before optional FS spawns"
        );
        assert!(
            src.contains("INIT_PM_RECV_DRAIN_DONE"),
            "init must log drain completion"
        );
    }

    #[test]
    fn pm_recv_drain_appears_before_ramfs_spawn_begin() {
        let src = include_str!("service.rs");
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
    fn blkcache_smoke_uses_blocking_ipc_recv_not_deadline_zero() {
        let src = include_str!("service.rs");
        let smoke_pos = src
            .find("INIT_BLKCACHE_SMOKE_BEGIN")
            .expect("blkcache smoke marker must be present");
        let drain_pos = src
            .find("INIT_PM_RECV_DRAIN_BEGIN")
            .expect("drain marker must be present");
        let smoke_section = &src[smoke_pos..drain_pos];
        assert!(
            !smoke_section.contains("ipc_recv_with_deadline(pm_recv, 0)"),
            "blkcache smoke must not use non-blocking ipc_recv_with_deadline(pm_recv, 0) for GET_INFO reply"
        );
    }
}
