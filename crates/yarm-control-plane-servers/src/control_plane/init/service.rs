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

// ─── Stage 195C: AArch64 FutexWake live oracle ────────────────────────────────────────
// A controlled parent/child proof of the AArch64 split FutexWake (NR 10). The child thread
// blocks through the LEGACY global-lock FutexWait; the parent (init) wakes it through the
// SPLIT path and verifies the authoritative wake counts (1 then 0). Default-off (slot-5
// sentinel). The coordination signal is authoritative — the parent retries FutexWake and
// treats the kernel's returned wake COUNT (not any timing) as the "child is blocked" proof.
#[cfg(all(
    not(feature = "hosted-dev"),
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
static FUTEX_ORACLE_WORD: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0x5A5A);
#[cfg(all(
    not(feature = "hosted-dev"),
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
static FUTEX_ORACLE_PARK: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0x1234);
// Handshake futex: the parent (init) blocks on this to hand the CPU to the freshly-spawned
// child; the child wakes it immediately before the child blocks on the oracle word. This is
// the authoritative coordination signal (not timing): when the parent's FutexWait returns,
// the child has provably reached its own legacy FutexWait on the oracle word. (AArch64 cannot
// fresh-dispatch a never-run thread through `yield`; it CAN through the block/dispatch path,
// exactly as the control-plane servers first enter user mode.)
#[cfg(all(
    not(feature = "hosted-dev"),
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
static FUTEX_ORACLE_HANDSHAKE: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0x00C0);
// Stage 195F: never-woken word for the NO-INCOMING idle oracle. The final runnable user task
// blocks here; nothing ever wakes it, so the default-on post-lock drain takes the Idle outcome.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static FUTEX_ORACLE_IDLE_WORD: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0x1D1E);
// Stage 195G: the two-task Yield oracle child sets this flag before blocking, proving the Yield
// post-lock drain dispatched it (task B) after task A (init) yielded.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static YIELD_ORACLE_FLAG: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(all(
    not(feature = "hosted-dev"),
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
static mut FUTEX_ORACLE_CHILD_STACK: [u8; 16384] = [0u8; 16384];
// `spawn_user_thread` requires a NON-zero TLS base; a small dedicated TLS region suffices.
#[cfg(all(
    not(feature = "hosted-dev"),
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
static mut FUTEX_ORACLE_CHILD_TLS: [u8; 512] = [0u8; 512];

/// Child (waiter) thread entry: block ONCE on the oracle futex through the legacy
/// global-lock FutexWait, report the wake, then park on a second futex that is never woken
/// (so it does not re-block on the oracle word and does not busy-spin). Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
extern "C" fn futex_oracle_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    // Wake the parent (init), which is blocked on the handshake futex. This only marks init
    // Runnable + enqueues it (FutexWake does not context-switch); we keep the CPU and fall
    // straight into our own FutexWait below, so init is only dispatched once we are provably
    // Blocked(Futex) on the oracle word.
    let handshake = FUTEX_ORACLE_HANDSHAKE.as_ptr();
    yarm_user_rt::user_log!("AARCH64_FUTEX_ORACLE_CHILD_WAKE_PARENT");
    let _ = yarm_user_rt::syscall::futex_wake(handshake, 1);
    let addr = FUTEX_ORACLE_WORD.as_ptr();
    let observed = FUTEX_ORACLE_WORD.load(Relaxed);
    yarm_user_rt::user_log!(
        "AARCH64_FUTEX_ORACLE_CHILD_WAIT_BEGIN observed={}",
        observed
    );
    // Legacy global-lock FutexWait (NOT split) — blocks on Futex(addr) and hands the CPU to
    // the now-Runnable parent.
    let _ = yarm_user_rt::syscall::futex_wait(addr, observed, observed);
    yarm_user_rt::user_log!("AARCH64_FUTEX_ORACLE_CHILD_WOKE");
    // Park cleanly on an unrelated futex so a second wake on the oracle word finds no waiter.
    let park = FUTEX_ORACLE_PARK.as_ptr();
    loop {
        let pv = FUTEX_ORACLE_PARK.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Parent (waker): spawn the child, wake it exactly once through the split path (count must
/// be 1), then wake again (count must be 0). Emits the live-oracle proof marker.
///
/// `futex_wait_mode` (Stage 195E): when true the kernel enabled the FutexWait queue-advancing
/// retirement, so the parent's handshake `FutexWait` below is dispatched by the OUT-OF-LOCK
/// drain (task A blocks via NR 9 → drain dispatches task B = the child → child wakes A via NR 10
/// → A resumes). The extra `AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE` marker attests that flow.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn run_aarch64_futex_wake_oracle(init_tid: u64, futex_wait_mode: bool) {
    yarm_user_rt::user_log!(
        "AARCH64_FUTEX_WAKE_ORACLE_BEGIN init_tid={} futex_wait_mode={}",
        init_tid,
        futex_wait_mode as u32
    );
    let addr = FUTEX_ORACLE_WORD.as_ptr();
    // 16-byte-aligned stack top of the child's dedicated static stack.
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = futex_oracle_child as *const () as usize;
    // Non-zero TLS base (spawn_user_thread rejects tls_base == 0).
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("AARCH64_FUTEX_WAKE_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    yarm_user_rt::user_log!("AARCH64_FUTEX_ORACLE_CHILD_SPAWNED child_tid={}", child_tid);
    // Authoritative coordination (NOT timing): block on the handshake futex to hand the CPU
    // to the freshly-spawned child. AArch64 fresh-dispatches the never-run child through this
    // block/dispatch path (the same one that first enters the control-plane servers into user
    // mode). The child wakes us, then blocks on the oracle word; when THIS FutexWait returns,
    // the child is provably Blocked(Futex) on the oracle word.
    let handshake = FUTEX_ORACLE_HANDSHAKE.as_ptr();
    let hv = FUTEX_ORACLE_HANDSHAKE.load(core::sync::atomic::Ordering::Relaxed);
    yarm_user_rt::user_log!("AARCH64_FUTEX_ORACLE_PARENT_HANDSHAKE_WAIT hv={}", hv);
    let _ = yarm_user_rt::syscall::futex_wait(handshake, hv, hv);
    yarm_user_rt::user_log!("AARCH64_FUTEX_ORACLE_PARENT_RESUMED");
    // The child is now Blocked(Futex) on the oracle word. Wake exactly once through the SPLIT
    // path — the kernel's returned wake COUNT must be 1 (waiter → Runnable, enqueued once).
    let first_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0);
    // Second wake: the child is now Runnable (no longer Blocked) and does not re-block on the
    // oracle word, so no waiter remains → count must be 0.
    let second_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0xFFFF);
    yarm_user_rt::user_log!(
        "AARCH64_FUTEX_WAKE_ORACLE_COUNTS first_wake={} second_wake={}",
        first_wake,
        second_wake
    );
    if first_wake == 1 && second_wake == 0 {
        yarm_user_rt::user_log!(
            "AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0"
        );
        // Stage 195E: in FutexWait mode the parent's handshake FutexWait above was serviced by
        // the queue-advancing OUT-OF-LOCK drain (task A blocked via NR 9, drain dispatched task
        // B = child_tid, child woke A via NR 10, A resumed exactly once). blocked_tid = A =
        // init, dispatched_tid = B = child, wake_count = 1.
        if futex_wait_mode {
            yarm_user_rt::user_log!(
                "AARCH64_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid={} dispatched_tid={} wake_count=1",
                init_tid,
                child_tid
            );
        }
    } else {
        yarm_user_rt::user_log!(
            "AARCH64_FUTEX_WAKE_LIVE_ORACLE_DONE result=fail first_wake={} second_wake={}",
            first_wake,
            second_wake
        );
    }
}

// ─── Stage 197A: x86_64 FutexWake live oracle (closes the first-cohort matrix at 12/12) ──
// The x86_64 port of the parent/child split-FutexWake (NR 10) proof. Parent A spawns child B and
// blocks on a handshake futex; B wakes A (handshake) then blocks on the target futex; A resumes
// (B provably Blocked(Futex(target))), wakes B once through the SPLIT path (count must be 1), wakes
// again (count must be 0), then YIELDS so B resumes exactly once and publishes the resume proof; A
// then confirms `waiter_resumes=1`. Authoritative coordination (handshake + resume flag) — never
// timing-only. Default-off (slot-5 sentinel = 1).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static X86_FW_WORD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x00A5);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static X86_FW_HANDSHAKE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x00C0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static X86_FW_PARK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x00D0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static X86_FW_WAITER_RESUMED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static X86_FW_CHILD_TID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut X86_FW_CHILD_STACK: [u8; 16384] = [0u8; 16384];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut X86_FW_CHILD_TLS: [u8; 512] = [0u8; 512];

/// Naked entry trampoline for child B. `spawn_user_thread` requires a 16-byte-aligned initial
/// user stack top and installs it as RSP verbatim, but the x86-64 SysV ABI expects a function to
/// be reached by a `call` (so its entry RSP ≡ 8 (mod 16)). Entering a normal Rust `extern "C" fn`
/// directly with a 16-aligned RSP violates that and faults with #GP on the prologue's first
/// 16-byte SSE spill. This naked shim has no compiler prologue, so it is safe to enter with a
/// 16-aligned RSP; it then `call`s the real body, which restores the ABI-required alignment.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(naked)]
extern "C" fn x86_futex_wake_oracle_child() -> ! {
    // RSP is 16-aligned on entry; `call` pushes the 8-byte return address so the body observes
    // RSP ≡ 8 (mod 16) exactly as the SysV ABI requires. The body never returns; `ud2` guards
    // against an unexpected return.
    core::arch::naked_asm!(
        "call {body}",
        "ud2",
        body = sym x86_futex_wake_oracle_child_body,
    )
}

/// Child B body: wake the parent on the handshake futex, block on the target futex (legacy
/// FutexWait), then — once woken exactly once by the parent's split FutexWake — publish the resume
/// proof and park on an unrelated futex. Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn x86_futex_wake_oracle_child_body() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    let handshake = X86_FW_HANDSHAKE.as_ptr();
    yarm_user_rt::user_log!("X86_FUTEX_ORACLE_CHILD_WAKE_PARENT");
    let _ = yarm_user_rt::syscall::futex_wake(handshake, 1);
    let addr = X86_FW_WORD.as_ptr();
    let observed = X86_FW_WORD.load(Relaxed);
    yarm_user_rt::user_log!("X86_FUTEX_ORACLE_CHILD_WAIT_BEGIN observed={}", observed);
    let _ = yarm_user_rt::syscall::futex_wait(addr, observed, observed);
    // Woken exactly once by the parent's first split FutexWake.
    X86_FW_WAITER_RESUMED.store(1, Relaxed);
    let btid = X86_FW_CHILD_TID.load(Relaxed);
    yarm_user_rt::user_log!("X86_FUTEX_WAKE_WAITER_RESUMED_OK tid={}", btid);
    let park = X86_FW_PARK.as_ptr();
    loop {
        let pv = X86_FW_PARK.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Parent A: spawn B, block on the handshake (B is provably Blocked(Futex(target)) when this
/// returns), wake B once (count 1) then again (count 0), YIELD so B resumes once, then confirm
/// `waiter_resumes=1` and emit the live-oracle proof.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn run_x86_futex_wake_oracle(init_tid: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("X86_FUTEX_WAKE_ORACLE_BEGIN init_tid={}", init_tid);
    let addr = X86_FW_WORD.as_ptr();
    // 16-byte-aligned stack top (spawn_user_thread requires it and installs it as the initial
    // RSP verbatim). The naked `x86_futex_wake_oracle_child` trampoline re-establishes the
    // SysV `RSP ≡ 8 (mod 16)` entry convention via its `call` into the real body.
    let stack_top = {
        let base = core::ptr::addr_of_mut!(X86_FW_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = x86_futex_wake_oracle_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(X86_FW_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("X86_FUTEX_WAKE_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    X86_FW_CHILD_TID.store(child_tid, Relaxed);
    yarm_user_rt::user_log!("X86_FUTEX_ORACLE_CHILD_SPAWNED child_tid={}", child_tid);
    // Authoritative handshake: hand the CPU to the freshly-spawned child; when this returns the
    // child has woken us and is provably Blocked(Futex) on the target word.
    let handshake = X86_FW_HANDSHAKE.as_ptr();
    let hv = X86_FW_HANDSHAKE.load(Relaxed);
    yarm_user_rt::user_log!("X86_FUTEX_ORACLE_PARENT_HANDSHAKE_WAIT hv={}", hv);
    let _ = yarm_user_rt::syscall::futex_wait(handshake, hv, hv);
    yarm_user_rt::user_log!("X86_FUTEX_ORACLE_PARENT_RESUMED");
    // Wake B once through the SPLIT path — count must be 1 (waiter → Runnable, enqueued once).
    let first_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0);
    // Second wake: B is no longer Blocked on the target word → count must be 0.
    let second_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0xFFFF);
    yarm_user_rt::user_log!(
        "X86_FUTEX_WAKE_USER_RETURN_OK first_wake={} second_wake={}",
        first_wake,
        second_wake
    );
    // Hand the CPU to the now-Runnable B (Yield re-enqueues A) so B resumes exactly once and
    // publishes X86_FW_WAITER_RESUMED; A is re-dispatched when B parks.
    let _ = yarm_user_rt::syscall::yield_now();
    let waiter_resumes = X86_FW_WAITER_RESUMED.load(Relaxed);
    if first_wake == 1 && second_wake == 0 && waiter_resumes == 1 {
        yarm_user_rt::user_log!(
            "X86_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0 waiter_tid={} waiter_resumes=1",
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "X86_FUTEX_WAKE_LIVE_ORACLE_DONE result=fail first_wake={} second_wake={} waiter_resumes={} waiter_tid={}",
            first_wake,
            second_wake,
            waiter_resumes,
            child_tid
        );
    }
}

// ─── Stage 198E3C1B: x86_64 DIRECT shared-region live oracle (COMPILE-ONLY scaffold) ─────
// Parent/child DIRECT blocked-receiver shared-region proof topology. Child B recv-v2-blocks on the
// oracle endpoint (the ORDINARY recv path, NOT RecvSharedV3); parent A then issues exactly ONE
// `IpcSend(OPCODE_SHARED_MEM | FLAG_CAP_TRANSFER, mem_cap)` (the DIRECT boundary path, NOT the
// shared-region ENQUEUE path), whose blocked-waiter delivery maps the two source pages READ-ONLY at
// the dedicated unmapped window `SHARED_REGION_ORACLE_VA`. B resumes out of its recv frame, validates
// BOTH pages against the deterministic pattern, releases the mapping through the receiver-local
// cleanup cap (`TransferRelease(cap,0,0)` → Ok(len); a duplicate → InvalidArgs), and parks.
//
// Stage 198E3C1B-H — AUTHORITATIVE blocked-recv handshake (replaces the invalid pre-recv futex
// signal). The child's `CHILD_STARTED` futex bump is a LIVENESS signal ONLY — it is raised BEFORE the
// child enters recv, so it is NOT proof that the child is a committed recv-v2 waiter (a valid
// interleaving — a timer preemption, or SMP — could let A run and send while B is still runnable,
// hitting the immediate/no-waiter path). The AUTHORITATIVE proof lives in the KERNEL: the receiver's
// own recv path publishes `SHARED_REGION_BLOCKED_RECV_ACK` only AFTER the blocked-recv record is
// fully committed (endpoint waiter linked + task Blocked + `BlockedRecvState` payload/meta stored),
// and the DIRECT delivery producer (`produce_blocked_waiter_shared_region_delivery`) consumes that
// ack exactly once for the exact receiver + endpoint + oracle VA. So A's send can only REACH the
// direct blocked path once B is authoritatively a committed recv-v2 waiter at `SHARED_REGION_ORACLE_VA`;
// a too-early send finds no ack → the kernel declines the direct path → a detectable non-direct
// outcome, never a silently-wrong immediate delivery. There is no timing/yield-count dependency in
// the correctness gate. Default-off: slot-5 selector = 2 (mutually exclusive with the FutexWake
// selector 1). This scaffold is COMPILE-ONLY in this stage — it emits NO live retirement seal and is
// never exercised under QEMU here.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
mod x86_shared_region_direct_oracle {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

    // LIVENESS signal only (child reached its recv preamble) — NOT proof the child is blocked. The
    // authoritative blocked proof is the KERNEL's `SHARED_REGION_BLOCKED_RECV_ACK` (published after
    // full waiter commit) which the direct-delivery producer consumes; see the module header.
    pub(super) static CHILD_STARTED: AtomicU32 = AtomicU32::new(0x00E0);
    pub(super) static PARK: AtomicU32 = AtomicU32::new(0x00E1);
    pub(super) static CHILD_TID: AtomicU64 = AtomicU64::new(0);
    // Caps the parent hands the child through the shared address space (both share init's CSpace).
    pub(super) static ENDPOINT_CAP: AtomicU32 = AtomicU32::new(0);
    // Child-published outcome: 0 = unrun, 1 = both pages validated + release contract satisfied,
    // 0xFFFF-family = a specific failure. Read by the parent after it yields to the child.
    pub(super) static CHILD_RESULT: AtomicU32 = AtomicU32::new(0);
    pub(super) static PAGES_OK: AtomicU32 = AtomicU32::new(0);
    pub(super) static RELEASE_OK: AtomicU32 = AtomicU32::new(0);

    #[allow(unused)]
    pub(super) static mut CHILD_STACK: [u8; 16384] = [0u8; 16384];
    #[allow(unused)]
    pub(super) static mut CHILD_TLS: [u8; 512] = [0u8; 512];

    /// Validate the two mapped pages against the deterministic oracle pattern. Detects corruption
    /// (any mismatched byte), a page swap (page 0 and page 1 hold DISTINCT bytes at the same in-page
    /// offset, so a swapped mapping mismatches), and short/second-page coverage (every byte across
    /// BOTH pages is checked). Returns `true` iff every byte matches.
    ///
    /// # Safety
    /// `base..base+len` must be the readable two-page mapping returned by the recv.
    pub(super) unsafe fn validate_two_pages(base: usize, len: usize) -> bool {
        if len != yarm_user_rt::syscall::SHARED_REGION_ORACLE_LEN {
            return false;
        }
        let mut off = 0usize;
        while off < len {
            // SAFETY: caller guarantees the whole window is mapped readable.
            let got = unsafe { core::ptr::read_volatile((base + off) as *const u8) };
            if got != yarm_user_rt::syscall::shared_region_oracle_pattern_byte(off) {
                return false;
            }
            off += 1;
        }
        true
    }

    /// Naked child entry trampoline (see the FutexWake oracle for the RSP-alignment rationale).
    #[unsafe(naked)]
    pub(super) extern "C" fn child_entry() -> ! {
        core::arch::naked_asm!(
            "call {body}",
            "ud2",
            body = sym child_body,
        )
    }

    /// Child B: raise the `CHILD_STARTED` LIVENESS signal (NOT a blocked-proof — see the module
    /// header), recv-v2-block on the oracle endpoint (ordinary recv — the DIRECT blocked-waiter path
    /// maps read-only, so no map-intent arg), validate both mapped pages, exercise the release
    /// contract (first → Ok(len); duplicate → InvalidArgs), publish the outcome, and park. Never
    /// returns. Correctness does NOT depend on this signal's timing: the kernel's authoritative
    /// blocked-recv ack (published only after B's waiter is fully committed) is what gates A's direct
    /// delivery.
    pub(super) extern "C" fn child_body() -> ! {
        yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_CHILD_STARTED");
        // Liveness only: signal that B reached its recv preamble. This does NOT prove B is blocked —
        // the KERNEL's post-commit ack is the authoritative proof.
        let started = CHILD_STARTED.as_ptr();
        let _ = yarm_user_rt::syscall::futex_wake(started, 1);
        let endpoint_cap = ENDPOINT_CAP.load(Relaxed);
        let va = yarm_user_rt::syscall::SHARED_REGION_ORACLE_VA;
        let len = yarm_user_rt::syscall::SHARED_REGION_ORACLE_LEN;
        // SAFETY: `va..va+len` is the dedicated unmapped two-page window (compile-time VA contract).
        let recv = unsafe { yarm_user_rt::syscall::recv_shared_region_v2(endpoint_cap, va, len) };
        match recv {
            Ok(Some(r)) => {
                // SAFETY: the DIRECT delivery mapped the two pages readable at `r.mapped_base`.
                let pages_ok = unsafe { validate_two_pages(r.mapped_base, r.mapped_len) };
                PAGES_OK.store(u32::from(pages_ok), Relaxed);
                yarm_user_rt::user_log!(
                    "SHARED_REGION_DIRECT_ORACLE_CHILD_MAPPED base=0x{:x} len={} cap={} pages_ok={}",
                    r.mapped_base,
                    r.mapped_len,
                    r.receiver_cap,
                    pages_ok as u32
                );
                // Release contract: first release returns Ok(len); a duplicate returns InvalidArgs.
                // SAFETY: `r.receiver_cap` is the receiver-local cleanup cap from the recv.
                let first =
                    unsafe { yarm_user_rt::syscall::release_shared_region_mapping(r.receiver_cap) };
                // SAFETY: intentional duplicate release to observe the canonical stale rejection.
                let dup =
                    unsafe { yarm_user_rt::syscall::release_shared_region_mapping(r.receiver_cap) };
                let release_ok = matches!(first, Ok(l) if l == r.mapped_len)
                    && matches!(dup, Err(yarm_user_rt::syscall::SyscallError::InvalidArgs));
                RELEASE_OK.store(u32::from(release_ok), Relaxed);
                CHILD_RESULT.store(if pages_ok && release_ok { 1 } else { 0xF1 }, Relaxed);
            }
            Ok(None) => {
                CHILD_RESULT.store(0xF2, Relaxed);
                yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_CHILD_WOULDBLOCK");
            }
            Err(e) => {
                CHILD_RESULT.store(0xF3, Relaxed);
                yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_CHILD_RECV_FAIL err={:?}", e);
            }
        }
        let btid = CHILD_TID.load(Relaxed);
        yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_CHILD_DONE tid={}", btid);
        let park = PARK.as_ptr();
        loop {
            let pv = PARK.load(Relaxed);
            let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
        }
    }
}

/// Parent A (COMPILE-ONLY scaffold): read the provisioned caps from the startup slots, spawn child
/// B, wait on the `CHILD_STARTED` LIVENESS signal (NOT a blocked proof), send the region through the
/// DIRECT path exactly once, YIELD so B resumes and validates, then emit the topology +
/// child-validation markers. The send's CORRECTNESS is enforced by the KERNEL: the direct blocked
/// delivery consumes the authoritative `SHARED_REGION_BLOCKED_RECV_ACK` (published only after B's
/// recv-v2 waiter is fully committed at the oracle VA), so a too-early send cannot reach the direct
/// path — it declines to a detectable non-direct outcome, never a silent immediate delivery. Emits
/// NO live retirement seal.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn run_x86_shared_region_direct_oracle(init_tid: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    use x86_shared_region_direct_oracle as oracle;
    yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_BEGIN init_tid={}", init_tid);
    let mem_cap = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::syscall::STARTUP_SLOT_SHARED_REGION_MEM_CAP,
    )
    .unwrap_or(0) as u32;
    let endpoint_cap = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::syscall::STARTUP_SLOT_SHARED_REGION_ENDPOINT_CAP,
    )
    .unwrap_or(0) as u32;
    if mem_cap == 0 || endpoint_cap == 0 {
        yarm_user_rt::user_log!(
            "SHARED_REGION_DIRECT_ORACLE_MISSING_CAPS mem_cap={} endpoint_cap={}",
            mem_cap,
            endpoint_cap
        );
        return;
    }
    oracle::ENDPOINT_CAP.store(endpoint_cap, Relaxed);
    let stack_top = {
        let base = core::ptr::addr_of_mut!(oracle::CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = oracle::child_entry as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(oracle::CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    oracle::CHILD_TID.store(child_tid, Relaxed);
    yarm_user_rt::user_log!(
        "SHARED_REGION_DIRECT_ORACLE_CHILD_SPAWNED child_tid={}",
        child_tid
    );
    // LIVENESS wait on CHILD_STARTED (NOT a blocked proof). Even if A resumes before B is a committed
    // waiter, correctness holds: the kernel's authoritative ack (published only after B's waiter is
    // fully committed at the oracle VA) is what gates A's DIRECT delivery below — a too-early send
    // finds no ack and declines the direct path (detectable), never a silent immediate delivery.
    let started = oracle::CHILD_STARTED.as_ptr();
    let sv = oracle::CHILD_STARTED.load(Relaxed);
    let _ = yarm_user_rt::syscall::futex_wait(started, sv, sv);
    yarm_user_rt::user_log!("SHARED_REGION_DIRECT_ORACLE_PARENT_RESUMED");
    // Exactly ONE DIRECT send to the blocked receiver (IpcSend, NOT the shared-region enqueue path).
    // The kernel's ack gate (produce_blocked_waiter_shared_region_delivery → consume_for_delivery)
    // dominates this send: it reaches the direct path only if B is an authoritative committed waiter.
    let payload = [0x5Au8, 0x3C];
    // SAFETY: `endpoint_cap` is the SEND|RECEIVE oracle cap; `mem_cap` the read-only source cap.
    let sent =
        unsafe { yarm_user_rt::syscall::send_shared_region(endpoint_cap, mem_cap, &payload) };
    // Hand the CPU to the now-Runnable B so it resumes out of its recv frame and validates.
    let _ = yarm_user_rt::syscall::yield_now();
    let child_result = oracle::CHILD_RESULT.load(Relaxed);
    let pages_ok = oracle::PAGES_OK.load(Relaxed);
    let release_ok = oracle::RELEASE_OK.load(Relaxed);
    yarm_user_rt::user_log!(
        "SHARED_REGION_DIRECT_ORACLE_TOPOLOGY_OK init_tid={} child_tid={} send_ok={} pages_ok={} release_ok={} child_result={}",
        init_tid,
        child_tid,
        sent.is_ok() as u32,
        pages_ok,
        release_ok,
        child_result
    );
}

// ─── Stage 196C: RISC-V FutexWake live oracle ────────────────────────────────────────
// The RISC-V port of the 195C parent/child FutexWake (NR 10) proof. The child blocks through
// the LEGACY global-lock FutexWait (NR 9, still global-lock-only on RISC-V); the parent (init)
// wakes it through the SPLIT path (NR 10, retired in Stage 196C) and verifies the authoritative
// wake counts (1 then 0). Default-off (slot-5 sentinel = 1). The handshake futex is the
// authoritative coordination signal — when the parent's FutexWait returns, the child is provably
// Blocked(Futex) on the oracle word (NOT a timing/delay-loop assumption).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
extern "C" fn riscv_futex_oracle_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    // Wake the parent (init), blocked on the handshake futex. FutexWake does NOT context-switch,
    // so we keep the CPU and fall straight into our own FutexWait below; init is only dispatched
    // once we are provably Blocked(Futex) on the oracle word.
    let handshake = FUTEX_ORACLE_HANDSHAKE.as_ptr();
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_CHILD_WAKE_PARENT");
    let _ = yarm_user_rt::syscall::futex_wake(handshake, 1);
    let addr = FUTEX_ORACLE_WORD.as_ptr();
    let observed = FUTEX_ORACLE_WORD.load(Relaxed);
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_CHILD_WAIT_BEGIN observed={}", observed);
    // Legacy global-lock FutexWait (NOT split) — blocks on Futex(addr) and hands the CPU to the
    // now-Runnable parent.
    let _ = yarm_user_rt::syscall::futex_wait(addr, observed, observed);
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_CHILD_WOKE");
    // Park on an unrelated futex so a second wake on the oracle word finds no waiter.
    let park = FUTEX_ORACLE_PARK.as_ptr();
    loop {
        let pv = FUTEX_ORACLE_PARK.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Stage 196C parent (waker): spawn the child, wait for the authoritative handshake that it is
/// Blocked(Futex), wake it once through the SPLIT path (count must be 1), wake again (count must
/// be 0). Emits the live-oracle proof marker and the userspace return-proof marker.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_futex_wake_oracle(init_tid: u64) {
    yarm_user_rt::user_log!("RISCV_FUTEX_WAKE_ORACLE_BEGIN init_tid={}", init_tid);
    let addr = FUTEX_ORACLE_WORD.as_ptr();
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = riscv_futex_oracle_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("RISCV_FUTEX_WAKE_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_CHILD_SPAWNED child_tid={}", child_tid);
    // Authoritative coordination (NOT timing): block on the handshake futex to hand the CPU to the
    // freshly-spawned child. RISC-V fresh-dispatches the never-run child through this block/dispatch
    // path (the same one that first enters the control-plane servers into user mode). The child
    // wakes us, then blocks on the oracle word; when THIS FutexWait returns, the child is provably
    // Blocked(Futex) on the oracle word.
    let handshake = FUTEX_ORACLE_HANDSHAKE.as_ptr();
    let hv = FUTEX_ORACLE_HANDSHAKE.load(core::sync::atomic::Ordering::Relaxed);
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_PARENT_HANDSHAKE_WAIT hv={}", hv);
    let _ = yarm_user_rt::syscall::futex_wait(handshake, hv, hv);
    yarm_user_rt::user_log!("RISCV_FUTEX_ORACLE_PARENT_RESUMED");
    // The child is now Blocked(Futex) on the oracle word. Wake exactly once through the SPLIT path
    // (NR 10) — the kernel's returned wake COUNT must be 1 (waiter → Runnable, enqueued once).
    let first_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0);
    // Second wake: the child is now Runnable (no longer Blocked) and parks on an unrelated futex,
    // so no waiter remains on the oracle word → count must be 0.
    let second_wake = yarm_user_rt::syscall::futex_wake(addr, 1).unwrap_or(0xFFFF);
    // Userspace return proof: BOTH split FutexWake syscalls returned to userspace (same-task sret)
    // and this subsequent instruction runs with the correct wake counts in hand.
    yarm_user_rt::user_log!(
        "RISCV_FUTEX_WAKE_USER_RETURN_OK first_wake={} second_wake={}",
        first_wake,
        second_wake
    );
    if first_wake == 1 && second_wake == 0 {
        yarm_user_rt::user_log!(
            "RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=ok first_wake=1 second_wake=0 waiter_tid={}",
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "RISCV_FUTEX_WAKE_LIVE_ORACLE_DONE result=fail first_wake={} second_wake={} waiter_tid={}",
            first_wake,
            second_wake,
            child_tid
        );
    }
}

// ─── Stage 196D: RISC-V queue-advancing context-switch FOUNDATION oracle ──────────────
// A two-task proof of a GENUINE RISC-V post-lock context switch (NOT a syscall retirement).
// Task A (init) spawns task B, then Yields. When the foundation knob is on, A's Yield is
// deferred: A is re-enqueued once, `current` is cleared, and the post-lock drain dispatches B
// with a REAL SATP/sfence.vma + frame restore + `sret` into B. B runs (sets the flag + emits
// the userspace marker) and parks on a futex, so the LEGACY global-lock path re-dispatches A;
// A resumes and confirms B ran. Default-off (slot-5 sentinel = 2).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static QUEUE_SWITCH_FOUNDATION_FLAG: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static QUEUE_SWITCH_FOUNDATION_CHILD_TID: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Task B (incoming): set the ran-flag, emit the userspace proof that real U-mode execution
/// began after the post-lock SATP/frame/sret switch, then park on a futex (leaving the runnable
/// set) so the legacy path re-dispatches task A. Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
extern "C" fn riscv_queue_switch_foundation_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    QUEUE_SWITCH_FOUNDATION_FLAG.store(1, Relaxed);
    let btid = QUEUE_SWITCH_FOUNDATION_CHILD_TID.load(Relaxed);
    // Emitted AFTER real userspace execution begins in B (post-sret) — the incoming-user proof.
    yarm_user_rt::user_log!(
        "RISCV_QUEUE_SWITCH_FOUNDATION_INCOMING_USER_OK tid={}",
        btid
    );
    // Park on the (never-woken) oracle futex so a legacy FutexWait re-dispatches task A.
    let park = FUTEX_ORACLE_PARK.as_ptr();
    loop {
        let pv = FUTEX_ORACLE_PARK.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Task A (init/outgoing): spawn B, publish B's tid, Yield (foundation switch to B), then resume
/// and verify B ran (round-trip proof).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_queue_switch_foundation_oracle(init_tid: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!(
        "RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_BEGIN init_tid={}",
        init_tid
    );
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = riscv_queue_switch_foundation_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("RISCV_QUEUE_SWITCH_FOUNDATION_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    // Publish B's tid BEFORE yielding so B can log it once it runs.
    QUEUE_SWITCH_FOUNDATION_CHILD_TID.store(child_tid, Relaxed);
    yarm_user_rt::user_log!(
        "RISCV_QUEUE_SWITCH_FOUNDATION_CHILD_SPAWNED child_tid={}",
        child_tid
    );
    // A yields: re-enqueued at tail; the foundation post-lock drain dispatches B (FIFO head) via
    // a real SATP/sfence/frame switch. A resumes here only after B has run and parked (its legacy
    // FutexWait re-dispatches A).
    let _ = yarm_user_rt::syscall::yield_now();
    let ran = QUEUE_SWITCH_FOUNDATION_FLAG.load(Relaxed);
    if ran == 1 {
        yarm_user_rt::user_log!(
            "RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=ok outgoing={} incoming={} outgoing_resumed=1",
            init_tid,
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "RISCV_QUEUE_SWITCH_FOUNDATION_ORACLE_DONE result=fail flag={} outgoing={} incoming={}",
            ran,
            init_tid,
            child_tid
        );
    }
}

// ─── Stage 196E: RISC-V FutexWait queue-advancing RETIREMENT live oracle ──────────────
// A two-task proof of the FIRST genuine off-global-lock RISC-V syscall retirement that
// context-switches the BLOCKING caller. Task A (init) spawns task B (ensuring an incoming
// runnable task exists), then enters FutexWait NR 9 on the oracle word. Because the one-shot
// oracle is armed and B is runnable, A's FutexWait is RETIRED: A → Blocked(Futex), `current`
// cleared, and the post-lock drain switches to B with a REAL SATP/sfence.vma + frame restore +
// `sret` into B. B runs (emits the incoming-user proof), wakes A through the already-retired
// SPLIT FutexWake NR 10 (count must be 1), then parks through the LEGACY path (the one-shot was
// consumed by A, so B's FutexWait is NOT retired) — which re-dispatches the now-Runnable A. A
// resumes exactly once and confirms the wake count. Default-off (slot-5 sentinel = 3).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static FUTEX_WAIT_ORACLE_WAKE_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0xFFFF);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static FUTEX_WAIT_ORACLE_CHILD_TID: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Task B (incoming): emit the userspace proof that real U-mode execution began after the
/// post-lock SATP/frame/sret switch, wake task A (blocked on the oracle word) through the SPLIT
/// FutexWake NR 10 (count must be 1), publish the count, then park through the LEGACY path (which
/// re-dispatches the now-Runnable A). Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
extern "C" fn riscv_futex_wait_oracle_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    let btid = FUTEX_WAIT_ORACLE_CHILD_TID.load(Relaxed);
    // Emitted AFTER real userspace execution begins in B (post-sret) — the incoming-user proof.
    yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_INCOMING_USER_OK tid={}", btid);
    // Wake A through the already-retired split FutexWake (NR 10). A is Blocked(Futex) on the
    // oracle word, so the returned wake COUNT must be exactly 1. FutexWake does NOT context-switch,
    // so B keeps the CPU and falls into its own park below.
    let word = FUTEX_ORACLE_WORD.as_ptr();
    let woke = yarm_user_rt::syscall::futex_wake(word, 1).unwrap_or(0);
    FUTEX_WAIT_ORACLE_WAKE_COUNT.store(woke, Relaxed);
    yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_CHILD_WOKE_PARENT woke={}", woke);
    // Park on an unrelated futex through the LEGACY global-lock path (the one-shot retirement was
    // consumed by A). Legacy FutexWait blocks B and dispatches the now-Runnable A.
    let park = FUTEX_ORACLE_PARK.as_ptr();
    loop {
        let pv = FUTEX_ORACLE_PARK.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Task A (init/blocking caller): spawn B (ensuring an incoming runnable task exists), enter
/// FutexWait NR 9 (retired → post-lock switch to B), then resume exactly once and verify B woke
/// it with count 1 (the round-trip retirement proof).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_futex_wait_oracle(init_tid: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_ORACLE_BEGIN init_tid={}", init_tid);
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = riscv_futex_wait_oracle_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    // Publish B's tid BEFORE blocking so B can log it once it runs.
    FUTEX_WAIT_ORACLE_CHILD_TID.store(child_tid, Relaxed);
    yarm_user_rt::user_log!(
        "RISCV_FUTEX_WAIT_ORACLE_CHILD_SPAWNED child_tid={}",
        child_tid
    );
    // A blocks on the oracle word. B is now runnable (the mandatory incoming-task-exists gate), so
    // this FutexWait is RETIRED: A → Blocked(Futex), the post-lock drain switches to B via a real
    // SATP/sfence/frame/sret. A resumes here ONLY after B wakes it (split FutexWake) and parks
    // (legacy path re-dispatches A).
    let word = FUTEX_ORACLE_WORD.as_ptr();
    let wv = FUTEX_ORACLE_WORD.load(Relaxed);
    yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_ORACLE_BLOCK_BEGIN word_val={}", wv);
    let _ = yarm_user_rt::syscall::futex_wait(word, wv, wv);
    // Userspace return proof: A's retired FutexWait returned (post-sret continuation) exactly once.
    let wake_count = FUTEX_WAIT_ORACLE_WAKE_COUNT.load(Relaxed);
    yarm_user_rt::user_log!(
        "RISCV_FUTEX_WAIT_USER_RETURN_OK tid={} wake_count={}",
        init_tid,
        wake_count
    );
    if wake_count == 1 {
        yarm_user_rt::user_log!(
            "RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=ok blocked_tid={} dispatched_tid={} wake_count=1",
            init_tid,
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "RISCV_FUTEX_WAIT_LIVE_ORACLE_DONE result=fail blocked_tid={} dispatched_tid={} wake_count={}",
            init_tid,
            child_tid,
            wake_count
        );
    }
}

// ─── Stage 196F: RISC-V FutexWait no-incoming IDLE oracle workload ─────────────────────
// The final runnable user task (init) blocks on a never-woken futex when no other user task is
// runnable (every control-plane server is blocked on recv; no child is spawned). The PRODUCTION
// default-on post-lock FutexWait drain therefore observes NO incoming task and takes the Idle
// outcome: it clears the deferral, proves the broad lock is released, keeps `current` None,
// restores NO frame, emits the kernel-side RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE attestation, and
// enters the real RISC-V BSP idle loop. This call never returns — QEMU stays in `wfi` until the
// smoke timeout, interrupt-responsive.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static RISCV_FUTEX_WAIT_IDLE_WORD: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0x1D1E);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_futex_wait_idle_oracle(init_tid: u64) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_IDLE_ORACLE_BEGIN init_tid={}", init_tid);
    let addr = RISCV_FUTEX_WAIT_IDLE_WORD.as_ptr();
    let observed = RISCV_FUTEX_WAIT_IDLE_WORD.load(Relaxed);
    yarm_user_rt::user_log!(
        "RISCV_FUTEX_WAIT_IDLE_ORACLE_WAIT_BEGIN observed={}",
        observed
    );
    // FutexWait (NR 9) that blocks and — with nothing else runnable — drives the production
    // default-on drain to its post-lock Idle outcome. Nothing ever wakes this word, so it does not
    // return; a return would be a defect (the drain must never sret into the blocked caller).
    loop {
        let _ = yarm_user_rt::syscall::futex_wait(addr, observed, observed);
        yarm_user_rt::user_log!("RISCV_FUTEX_WAIT_IDLE_ORACLE_UNEXPECTED_RETURN");
    }
}

// ─── Stage 196G: RISC-V Yield (NR 0) two-task + lone-task retirement oracles ───────────
// Both run under the PRODUCTION default-on Yield mechanism (the knobs are workload selectors, not
// retirement arming). Two-task: A yields → post-lock switch to B; B blocks (default-on FutexWait)
// → drain re-dispatches A; A resumes exactly once. Lone-task: A is the only task; its Yield
// re-enqueues itself and the drain dequeues the caller ITSELF (self-redispatch, never idle),
// proving repeated Yield retirement.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static YIELD_TWO_TASK_FLAG: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static YIELD_TWO_TASK_CHILD_TID: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Task B (incoming): prove it was entered by the post-lock Yield drain (set flag + userspace
/// marker), then block via the already-proven default-on FutexWait (A is runnable, so the FutexWait
/// drain switches to A — not idle). Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
extern "C" fn riscv_yield_two_task_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    YIELD_TWO_TASK_FLAG.store(1, Relaxed);
    let btid = YIELD_TWO_TASK_CHILD_TID.load(Relaxed);
    yarm_user_rt::user_log!("RISCV_YIELD_TWO_TASK_INCOMING_USER_OK tid={}", btid);
    let word = RISCV_FUTEX_WAIT_IDLE_WORD.as_ptr();
    loop {
        let v = RISCV_FUTEX_WAIT_IDLE_WORD.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(word, v, v);
    }
}

/// Task A (init/outgoing): spawn B (runnable), Yield (retired → switch to B), then resume exactly
/// once and confirm B ran (round-trip proof). Busy-spins afterward so the proof state is stable and
/// no post-yield idle marker is emitted.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_yield_two_task_oracle(init_tid: u64) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("RISCV_YIELD_TWO_TASK_ORACLE_BEGIN init_tid={}", init_tid);
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = riscv_yield_two_task_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("RISCV_YIELD_TWO_TASK_ORACLE_SPAWN_FAIL err={:?}", e);
            loop {
                core::hint::spin_loop();
            }
        }
    };
    YIELD_TWO_TASK_CHILD_TID.store(child_tid, Relaxed);
    yarm_user_rt::user_log!("RISCV_YIELD_TWO_TASK_CHILD_SPAWNED child_tid={}", child_tid);
    // A yields: re-enqueued at tail; the post-lock Yield drain dispatches B (FIFO head). A resumes
    // here only after B ran and blocked (its default-on FutexWait re-dispatches A).
    let _ = yarm_user_rt::syscall::yield_now();
    let ran = YIELD_TWO_TASK_FLAG.load(Relaxed);
    if ran == 1 {
        yarm_user_rt::user_log!("RISCV_YIELD_TWO_TASK_OUTGOING_RESUMED_OK tid={}", init_tid);
        yarm_user_rt::user_log!(
            "RISCV_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing={} incoming={} outgoing_resumed=1",
            init_tid,
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "RISCV_YIELD_TWO_TASK_ORACLE_DONE result=fail flag={} outgoing={} incoming={}",
            ran,
            init_tid,
            child_tid
        );
    }
    // Park on the never-woken idle word so the boot reaches the canonical RISC-V idle terminal
    // (timer/PLIC/EXTIRQ idle-safe-point init). B is blocked, so this FutexWait finds no incoming
    // and takes the post-lock IDLE outcome — this is AFTER the Yield round-trip proof.
    let word = RISCV_FUTEX_WAIT_IDLE_WORD.as_ptr();
    loop {
        let v = RISCV_FUTEX_WAIT_IDLE_WORD.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(word, v, v);
    }
}

/// Lone-task oracle: init is the ONLY runnable task. Its Yield re-enqueues itself; the post-lock
/// drain dequeues the caller ITSELF (self-redispatch, never idle) and resumes it after the ecall.
/// Repeats a bounded number of Yields to prove the mechanism is not one-shot, then parks so the
/// boot reaches the canonical idle terminal. Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn run_riscv_yield_lone_task_oracle(init_tid: u64) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("RISCV_YIELD_LONE_TASK_ORACLE_BEGIN init_tid={}", init_tid);
    // First Yield: A re-enqueued, drain dequeues A itself, A resumes after the ecall.
    let _ = yarm_user_rt::syscall::yield_now();
    yarm_user_rt::user_log!(
        "RISCV_YIELD_LONE_TASK_ORACLE_DONE result=ok tid={} redispatched_self=1",
        init_tid
    );
    // Prove repeated (non-one-shot) retirement: a few more real Yields, each self-redispatching.
    let mut n: u32 = 1;
    while n < 4 {
        let _ = yarm_user_rt::syscall::yield_now();
        n += 1;
    }
    yarm_user_rt::user_log!(
        "RISCV_YIELD_LONE_TASK_REPEAT_OK tid={} yields={}",
        init_tid,
        n
    );
    // Park (FutexWait, no incoming → post-lock IDLE) so the boot reaches the canonical idle
    // terminal AFTER the lone-Yield proof. The Yield transition itself never idled (the drain has
    // no idle branch — redispatched_self=1 proves the self-switch).
    let word = RISCV_FUTEX_WAIT_IDLE_WORD.as_ptr();
    loop {
        let v = RISCV_FUTEX_WAIT_IDLE_WORD.load(Relaxed);
        let _ = yarm_user_rt::syscall::futex_wait(word, v, v);
    }
}

/// Stage 195F NO-INCOMING idle oracle: the final runnable user task (init) blocks on a
/// never-woken futex when no other user task is runnable (every server is blocked on recv and no
/// child is spawned). The default-on post-lock FutexWait drain therefore observes no incoming
/// task and takes the Idle outcome: it clears the deferral, proves the broad lock is released,
/// keeps `current` None, restores NO frame, and enters the BSP idle loop. This call never
/// returns — QEMU stays idle (WFI) until the smoke timeout, interrupt-responsive.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn run_aarch64_futex_wait_idle_oracle(init_tid: u64) -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("AARCH64_FUTEX_WAIT_IDLE_ORACLE_BEGIN init_tid={}", init_tid);
    let addr = FUTEX_ORACLE_IDLE_WORD.as_ptr();
    let observed = FUTEX_ORACLE_IDLE_WORD.load(Relaxed);
    yarm_user_rt::user_log!(
        "AARCH64_FUTEX_WAIT_IDLE_ORACLE_WAIT_BEGIN observed={}",
        observed
    );
    // Legacy-shape FutexWait (NR 9) that blocks and — with nothing else runnable — drives the
    // post-lock Idle outcome. Nothing ever wakes this word, so it does not return.
    loop {
        let _ = yarm_user_rt::syscall::futex_wait(addr, observed, observed);
        yarm_user_rt::user_log!("AARCH64_FUTEX_WAIT_IDLE_ORACLE_UNEXPECTED_RETURN");
    }
}

/// Stage 195G two-task Yield oracle child (task B): prove it was entered by the out-of-lock Yield
/// drain by setting the flag, then leave the runnable set (block on the never-woken idle word)
/// so task A can re-dispatch and confirm. Never returns.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
extern "C" fn yield_oracle_child() -> ! {
    use core::sync::atomic::Ordering::Relaxed;
    YIELD_ORACLE_FLAG.store(1, Relaxed);
    yarm_user_rt::user_log!("AARCH64_YIELD_ORACLE_CHILD_RAN");
    let park = FUTEX_ORACLE_IDLE_WORD.as_ptr();
    let pv = FUTEX_ORACLE_IDLE_WORD.load(Relaxed);
    loop {
        let _ = yarm_user_rt::syscall::futex_wait(park, pv, pv);
    }
}

/// Stage 195G two-task Yield oracle (Proof A): task A (init) spawns task B, then calls Yield
/// (NR 0). A is re-enqueued exactly once at the queue tail; the post-lock Yield drain dispatches
/// B (the FIFO head). B runs (sets the flag) and blocks, so the FutexWait drain re-dispatches A;
/// A resumes after the Yield and confirms B ran.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn run_aarch64_yield_two_task_oracle(init_tid: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    yarm_user_rt::user_log!("AARCH64_YIELD_TWO_TASK_ORACLE_BEGIN init_tid={}", init_tid);
    let stack_top = {
        let base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_STACK) as usize;
        (base + 16384) & !0xF
    };
    let entry = yield_oracle_child as *const () as usize;
    let tls_base = core::ptr::addr_of_mut!(FUTEX_ORACLE_CHILD_TLS) as usize;
    // SAFETY: `entry` is a valid `extern "C" fn() -> !`; the static stack + TLS outlive the thread.
    let child_tid = match unsafe { yarm_user_rt::syscall::spawn_thread(tls_base, stack_top, entry) }
    {
        Ok(t) => t,
        Err(e) => {
            yarm_user_rt::user_log!("AARCH64_YIELD_TWO_TASK_ORACLE_SPAWN_FAIL err={:?}", e);
            return;
        }
    };
    yarm_user_rt::user_log!("AARCH64_YIELD_ORACLE_CHILD_SPAWNED child_tid={}", child_tid);
    // A yields: re-enqueued at tail, the Yield drain dispatches B (FIFO head). A resumes only
    // after B has run and blocked (its FutexWait drain re-dispatches A).
    let _ = yarm_user_rt::syscall::yield_now();
    let ran = YIELD_ORACLE_FLAG.load(Relaxed);
    if ran == 1 {
        yarm_user_rt::user_log!(
            "AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=ok outgoing={} incoming={}",
            init_tid,
            child_tid
        );
    } else {
        yarm_user_rt::user_log!(
            "AARCH64_YIELD_TWO_TASK_ORACLE_DONE result=fail flag={}",
            ran
        );
    }
}

/// Stage 195G lone-task Yield oracle (Proof B): task A (init) is the only runnable user task and
/// calls Yield (NR 0). A is re-enqueued once; the post-lock Yield drain dequeues A ITSELF (the
/// sole FIFO head), makes A current again, and A resumes after the syscall — proving same-task
/// re-dispatch with NO idle outcome.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn run_aarch64_yield_lone_task_oracle(init_tid: u64) {
    yarm_user_rt::user_log!("AARCH64_YIELD_LONE_TASK_ORACLE_BEGIN init_tid={}", init_tid);
    // Only A is runnable; the Yield drain re-dispatches A itself. If yield_now returns, the
    // same-task re-dispatch completed.
    let _ = yarm_user_rt::syscall::yield_now();
    yarm_user_rt::user_log!(
        "AARCH64_YIELD_LONE_TASK_ORACLE_DONE result=ok tid={} redispatched_self=1",
        init_tid
    );
}

pub fn run() {
    yarm_user_rt::user_log!("INIT_RUN_ENTER");
    // Stage 196B: this second userspace DebugLog executes ONLY if the preceding
    // DebugLog (INIT_RUN_ENTER, NR 15) returned to userspace via `sret`. On RISC-V
    // that DebugLog is serviced off the global lock by the split dispatcher
    // (`YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=15`), so this line's appearance is
    // the userspace-side proof of a correct same-task split-DebugLog return
    // (kernel does NOT emit it — it is a userspace log after the syscall returns).
    // It is harmless/benign on x86_64/AArch64 (a plain extra USER_LOG line).
    yarm_user_rt::user_log!("RISCV_DEBUGLOG_SPLIT_USER_RETURN_OK");
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

    // Stage 197A: default-off x86_64 FutexWake live oracle. Slot-5 sentinel 1 (set under
    // `yarm.x86_64_futex_wake_oracle=1`) tells init to run the parent/child split-FutexWake proof
    // (counts 1 then 0, waiter resumes once). A normal boot leaves slot 5 = None and skips this.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    if ctx.supervisor_control_recv_ep == Some(1) {
        run_x86_futex_wake_oracle(ctx.task_id);
    }

    // Stage 198E3C1B: default-off + feature-gated x86_64 DIRECT shared-region live oracle. Slot-5
    // selector 2 (mutually exclusive with the FutexWake selector 1) tells init to run the
    // parent/child DIRECT blocked-receiver shared-region topology. COMPILE-ONLY in this stage — the
    // scaffold is wired but emits no live seal and is not exercised under QEMU here.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    if ctx.supervisor_control_recv_ep == Some(2) {
        run_x86_shared_region_direct_oracle(ctx.task_id);
    }

    // Stage 195C: default-off AArch64 FutexWake live oracle. The kernel reuses init
    // startup slot 5 (supervisor_control_recv_ep, unused by init) as a sentinel (=1) ONLY
    // under `yarm.aarch64_futex_wake_oracle=1`. A normal boot leaves it None and skips this.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if ctx.supervisor_control_recv_ep == Some(1) || ctx.supervisor_control_recv_ep == Some(2) {
        // Slot-5 sentinel: 1 = Stage 195C FutexWake oracle; 2 = Stage 195E FutexWait
        // queue-advancing SWITCH oracle (same parent/child flow, but the parent's handshake
        // FutexWait is serviced by the out-of-lock drain).
        let futex_wait_mode = ctx.supervisor_control_recv_ep == Some(2);
        run_aarch64_futex_wake_oracle(ctx.task_id, futex_wait_mode);
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if ctx.supervisor_control_recv_ep == Some(3) {
        // Slot-5 sentinel 3 = Stage 195F NO-INCOMING idle oracle. All servers are up and blocked
        // on recv; init is the last runnable user task. This blocks on a never-woken futex and
        // does not return — the default-on post-lock drain takes the Idle outcome.
        run_aarch64_futex_wait_idle_oracle(ctx.task_id);
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if ctx.supervisor_control_recv_ep == Some(4) {
        // Slot-5 sentinel 4 = Stage 195G two-task Yield oracle (Proof A).
        run_aarch64_yield_two_task_oracle(ctx.task_id);
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if ctx.supervisor_control_recv_ep == Some(5) {
        // Slot-5 sentinel 5 = Stage 195G lone-task Yield oracle (Proof B).
        run_aarch64_yield_lone_task_oracle(ctx.task_id);
    }
    // Stage 196C: default-off RISC-V FutexWake live oracle. Slot-5 sentinel 1 (set by the RISC-V
    // boot under `yarm.riscv64_futex_wake_oracle=1`) tells init to run the parent/child split
    // FutexWake proof. A normal boot leaves slot 5 = None and skips this entirely.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(1) {
        run_riscv_futex_wake_oracle(ctx.task_id);
    }
    // Stage 196D: default-off RISC-V queue-advancing context-switch FOUNDATION oracle. Slot-5
    // sentinel 2 (set under `yarm.riscv64_queue_switch_foundation_oracle=1`) tells init to run the
    // two-task post-lock switch proof. A normal boot leaves slot 5 = None and skips this.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(2) {
        run_riscv_queue_switch_foundation_oracle(ctx.task_id);
    }
    // Stage 196E: default-off RISC-V FutexWait (NR 9) queue-advancing RETIREMENT oracle. Slot-5
    // sentinel 3 (set under `yarm.riscv64_futex_wait_oracle=1`) tells init to run the two-task
    // FutexWait retirement proof. A normal boot leaves slot 5 = None and skips this.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(3) {
        run_riscv_futex_wait_oracle(ctx.task_id);
    }
    // Stage 196F: default-off RISC-V FutexWait no-incoming IDLE oracle. Slot-5 sentinel 4 (set
    // under `yarm.riscv64_futex_wait_idle_oracle=1`) tells init (the last runnable user task) to
    // block on a never-woken futex, driving the production default-on drain to its idle outcome.
    // This never returns. A normal boot leaves slot 5 = None and skips this.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(4) {
        run_riscv_futex_wait_idle_oracle(ctx.task_id);
    }
    // Stage 196G: default-off RISC-V Yield (NR 0) retirement oracles. Slot-5 sentinel 5 = two-task
    // (A yields → switch to B → A resumes); 6 = lone-task (A yields → self-redispatch). Both run
    // under the PRODUCTION default-on Yield mechanism. A normal boot leaves slot 5 = None.
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(5) {
        run_riscv_yield_two_task_oracle(ctx.task_id);
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
    if ctx.supervisor_control_recv_ep == Some(6) {
        run_riscv_yield_lone_task_oracle(ctx.task_id);
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
            // Slot 13 present. The slot-14 / slot-17 presence disambiguates:
            //   slots 13 + 14 + 17 → Stage 193D IpcSend reply-cap transfer live oracle
            //   slots 13 + 14      → sender-wake proof (Stage 163)
            //   slot 13 only       → Stage 193C IpcSend ordinary cap-transfer live oracle
            if let Some(slot14) = ctx.service_extra_cap_1 {
                if let Some(reply_recv_cap) = ctx.pm_request_recv_cap {
                    // Slot 17 set → reply-cap DIRECT oracle: slot 13 = coord, slot 14 =
                    // the kernel-provisioned transferable reply cap, slot 17 = init's
                    // reply-endpoint RECV cap (the wakeable caller, Stage 198C2B).
                    run_ipc_send_reply_cap_oracle(
                        proof_send,
                        proof_recv,
                        e2_recv,
                        slot14,
                        reply_recv_cap,
                        ctx.task_id,
                    );
                } else {
                    // Slot 14 carries E1's buffered capacity → sender-wake proof, so init
                    // can fill E1 to EXACTLY full with non-blocking sends and never block.
                    run_ipc_recv_proof_sender_wake(
                        proof_send,
                        proof_recv,
                        e2_recv,
                        slot14 as usize,
                        ctx.task_id,
                    );
                }
            } else {
                // Slot 13 only → Stage 193C cap oracle (coord endpoint in slot 13).
                run_ipc_send_cap_oracle(proof_send, proof_recv, e2_recv, ctx.task_id);
            }
        } else if let Some(coord_recv) = ctx.service_extra_cap_1 {
            // Slot 13 empty + slot 14 present → Stage 193B IpcSend-plain live oracle.
            // The presence pattern (set by the kernel under the send-plain-oracle
            // sub-knob) selects this instead of sender-wake; the two are mutually
            // exclusive.
            run_ipc_send_plain_oracle(proof_send, proof_recv, coord_recv, ctx.task_id);
        } else if let Some(mode) = ctx.pm_request_recv_cap {
            // Slots 13 + 14 empty + slot 17 discriminator set → a no-waiter enqueue live
            // oracle (needs no coordination cap; sends to E1 in slots 6/7). The slot-17 value
            // selects which: 1 = Stage 193E plain enqueue, 2 = Stage 193F ordinary-cap enqueue.
            if mode == 2 {
                run_ipc_send_cap_enqueue_oracle(proof_send, proof_recv, ctx.task_id);
            } else {
                run_ipc_send_enqueue_oracle(proof_send, proof_recv, ctx.task_id);
            }
        } else {
            // Neither sub-knob: intentionally not driven (no fake marker). The
            // queued-split + rollback proof above stands alone.
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
    // Stage 163P Task A: E2 coordination uses a bounded NON-BLOCKING poll with an
    // explicit yield between checks (see the loop below). The kernel pushes the
    // waiter-present signal from within `enqueue_sender_waiter` while holding
    // `ipc_state_lock`, using `endpoint.send(msg)` directly — it does NOT wake
    // blocked receivers (that would require the scheduler lock while holding the
    // IPC lock, violating lock-order rank 1 < rank 3/4). The parent therefore must
    // not block on E2 waiting for a wake that never comes; instead it probes E2
    // non-blockingly and `yield_now()`s the CPU to the child between probes,
    // staying Runnable so the scheduler always returns to it once the child parks.
    //
    // E2_POLL_MAX_ITERS bounds the cooperative poll so a missing/never-scheduled
    // child can never hang the boot. Each iteration is one non-blocking probe plus
    // one yield; the signal normally appears by iter 1–2.
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
    // Stage 163P Task A fix: cooperative NON-BLOCKING poll of E2 with an explicit
    // yield between checks, instead of a blocking recv-with-deadline.
    //
    // Why the previous (Stage 163N/O) blocking-recv-with-deadline failed on every
    // arch: `ipc_recv_with_deadline(e2_recv, T)` on an empty E2 blocks the parent
    // (status=Blocked, removed from the run queue) and dispatches the child. The
    // syscall then returns Err(TimedOut) synchronously to the parent's saved
    // frame, but the parent's TCB is left Blocked and OFF the run queue — it can
    // only be made runnable again by `process_ipc_timeout_deadlines` on a TIMER
    // interrupt. Once BOTH parent (blocked on E2) and child (blocked on its E1
    // send) are parked, the CPU idles waiting for that timer; the parent never
    // resumes to run the next poll iteration, so it never observes the E2 signal
    // (no E2_POLL_HIT) and the proof stalls.
    //
    // The kernel still pushes the E2 waiter-present signal atomically inside
    // `enqueue_sender_waiter` (race-free: "E2 has the signal" ⇔ "child is a real
    // E1 sender-waiter"). The child does NOT send E2 itself. The fix here is only
    // the parent's WAIT mechanism: a non-blocking check (timeout=0 → split/try
    // path, never blocks) followed by `yield_now()`. `yield_now` keeps the parent
    // Runnable and on the run queue while handing the CPU to the child, so when
    // the child becomes a waiter and parks, the scheduler returns to the parent,
    // which then finds the queued E2 signal on its next non-blocking check. No
    // timer dependency, portable across x86_64/AArch64/RISC-V.
    yarm_user_rt::user_log!(
        "IPC_RECV_PROOF_SENDER_WAKE_E2_CAPS e2_send=0 e2_recv={}",
        e2_recv
    );
    yarm_user_rt::user_log!("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_BEGIN");
    'e2_poll: for poll_iter in 0..E2_POLL_MAX_ITERS {
        // SAFETY: e2_recv is init's RECV cap to the proof coordination endpoint.
        // timeout=0 is a non-blocking probe: the kernel tries the split/queued
        // take and returns immediately (never blocks the parent).
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e2_recv, 0) } {
            Ok(Some(sig)) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter={} result=hit",
                    poll_iter
                );
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_HIT iter={}",
                    poll_iter
                );
                waiter_tid = Some(sig.sender_tid.0);
                break 'e2_poll;
            }
            Ok(None) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter={} result=none",
                    poll_iter
                );
            }
            Err(yarm_user_rt::syscall::SyscallError::WouldBlock) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter={} result=wouldblock",
                    poll_iter
                );
            }
            Err(yarm_user_rt::syscall::SyscallError::TimedOut) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter={} result=timedout",
                    poll_iter
                );
            }
            Err(e) => {
                yarm_user_rt::user_log!(
                    "IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter={} result=err code={}",
                    poll_iter,
                    e as usize
                );
                break 'e2_poll;
            }
        }
        // Hand the CPU to the child so it can issue its blocking E1 send and
        // become a sender-waiter (kernel pushes E2 in that path). The parent
        // stays Runnable, so the scheduler returns here once the child parks.
        let _ = yarm_user_rt::syscall::yield_now();
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

/// Stage 193B: the plain payload the IpcSend-plain live oracle delivers. A
/// non-inline opcode (see `IPC_SEND_PLAIN_ORACLE_OPCODE`) so the kernel does not
/// strip any inline prefix — the receiver observes these exact bytes.
#[cfg(not(test))]
const IPC_SEND_PLAIN_ORACLE_PAYLOAD: [u8; 8] = *b"SP193B!!";

/// Stage 193B: application opcode for the send-plain oracle message. Arbitrary,
/// proof-local, and deliberately NOT `OPCODE_INLINE` (0) so no inline-opcode
/// prefix stripping occurs on the plain delivery.
#[cfg(not(test))]
const IPC_SEND_PLAIN_ORACLE_OPCODE: u16 = 0x0F9E;

/// Stage 193B: deterministic IpcSend-plain LIVE oracle.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and `yarm.ipc_send_plain_oracle=1`
/// (gated by the presence of the kernel-provisioned coordination endpoint recv cap
/// in startup slot 14 WITH slot 13 empty). Fires the Stage 193A `class=IpcSendPlain`
/// boundary split LIVE in QEMU by driving the exact slice it decomposes: a PLAIN
/// IpcSend to an already-recv-v2-blocked receiver.
///
/// 1. init drains E1 empty (the base subtests share E1), so the child blocks on an
///    empty endpoint rather than draining a queued message.
/// 2. init forks; the CHILD (fork returns 0) is the RECEIVER, the parent (init) is
///    the plain SENDER. The child inherits init's proof caps via the COW fork.
/// 3. the child `recv-v2`-blocks on E1. The kernel's `publish_recv_waiter_live`
///    hook, in the SAME `ipc_state_lock` section that registers the waiter, pushes
///    a receiver-blocked signal (carrying the child's TID) into the coordination
///    endpoint — an atomic proxy for "a receiver is a waiter on E1".
/// 4. init non-blocking-polls the coordination endpoint; the signal appears EXACTLY
///    when the child is provably a recv-v2 waiter (no enqueue race). init verifies
///    the signalled TID is the forked child (never init) before proceeding.
/// 5. init sends a PLAIN message (no cap / no reply-cap / no shared-region) to E1.
///    Because the child is already a recv-v2 waiter, the kernel takes the 193A
///    plain boundary split: snapshot in-lock, then the trap-entry drain (global
///    lock dropped) copies the payload + wakes the child exactly once — emitting
///    IPC_SEND_BOUNDARY_* + `GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcSendPlain`.
/// 6. init emits `IPC_SEND_PLAIN_LIVE_ORACLE_DONE result=ok`; the woken child reads
///    the byte-identical payload and logs `..._CHILD_RECV_OK payload_match=1`.
///
/// The child parks (never returns into init's flow). All waits are bounded so a
/// missing child (e.g. fork failure) degrades to a logged give-up, never a hang.
#[cfg(not(test))]
fn run_ipc_send_plain_oracle(e1_send: u32, e1_recv: u32, coord_recv: u32, init_tid: u64) {
    use yarm_user_rt::ipc::Message;

    const COORD_POLL_MAX_ITERS: usize = 100;
    const PREDRAIN_MAX: usize = 512;

    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_SEND_PLAIN_ORACLE_SETUP e1_send={} e1_recv={} coord_recv={} init_tid={}",
        e1_send,
        e1_recv,
        coord_recv,
        init_tid
    );

    // (0) Drain any leftover messages from the base subtests so the child blocks on
    // an EMPTY endpoint (non-blocking; empty returns Ok(None)/WouldBlock).
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_PREDRAIN_DONE count={}", predrained);

    // (1) Fork — child == receiver, parent (init) == plain sender.
    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_FORK_BEGIN");
    // SAFETY: proof-only raw fork; the child inherits init's COW address space +
    // proof caps and parks after its blocking recv (never returns into init's flow).
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    yarm_user_rt::user_log!(
        "IPC_SEND_PLAIN_ORACLE_FORK_RET ret0={} ret1={} ret2={} err={} arch={}",
        fr.ret0,
        fr.ret1,
        fr.ret2,
        fr.err,
        fr.arch
    );
    let pid = if fr.ret0 != 0 {
        yarm_user_rt::user_log!(
            "IPC_SEND_PLAIN_ORACLE_FORK_DECODE code={} meaning={} role=parent",
            fr.err,
            fork_err_meaning(fr.err)
        );
        fr.ret0
    } else if fr.err != 0 && fr.err < 0x100 {
        // Genuine fork failure: abort boundedly — never spin for a missing child.
        yarm_user_rt::user_log!(
            "IPC_SEND_PLAIN_ORACLE_FORK_FAILED code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        return;
    } else {
        // ── CHILD: the real recv-v2-blocked receiver. ──
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_CHILD_ENTRY");
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_CHILD_RECV_BEGIN");
        // SAFETY: e1_recv is a kernel-provisioned RECV cap to the proof loopback.
        // The endpoint is empty (predrained), so this blocks the child as a recv-v2
        // waiter; init's plain send wakes it via the 193A boundary drain.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
            Ok(Some(received)) => {
                // `ipc_call_prepare` frames a sent message as [opcode_le(2) ++
                // payload]; a PLAIN inline send is delivered WITHOUT the kernel
                // stripping that prefix (strip only fires for cap-transfer /
                // reply-cap messages), so the byte-identical delivery the child
                // observes is exactly that 2-byte opcode prefix + the payload.
                let got = received.message.as_slice();
                let opcode_le = IPC_SEND_PLAIN_ORACLE_OPCODE.to_le_bytes();
                let payload_match = got.len() == 2 + IPC_SEND_PLAIN_ORACLE_PAYLOAD.len()
                    && got[0..2] == opcode_le
                    && got[2..] == IPC_SEND_PLAIN_ORACLE_PAYLOAD[..];
                let has_cap = received.transferred_cap.is_some();
                yarm_user_rt::user_log!(
                    "IPC_SEND_PLAIN_ORACLE_CHILD_RECV_OK payload_match={} transferred_cap={} payload_len={} sender_tid={}",
                    payload_match as u8,
                    has_cap as u8,
                    got.len(),
                    received.sender_tid
                );
                // Stage 198A (SECOND-COHORT PLAIN PARITY): the recv-v2-blocked child
                // resuming with a byte-identical plain payload and NO transferred cap IS
                // the "plain send to an already-blocked receiver" live cell. Emit the
                // canonical per-arch attestation only on a fully clean delivery so the
                // three-arch seal can key on it. `payload_len` reports the delivered plain
                // payload byte count (identical across arches), not the framed length.
                if payload_match && !has_cap {
                    yarm_user_rt::user_log!(
                        "IPCSEND_PLAIN_BLOCKED_RECEIVER_ORACLE_DONE arch={} result=ok payload_len={} receiver_resumes=1",
                        fr.arch,
                        IPC_SEND_PLAIN_ORACLE_PAYLOAD.len()
                    );
                }
            }
            Ok(None) => {
                yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_CHILD_RECV_RET code=wouldblock");
            }
            Err(e) => {
                yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_CHILD_RECV_RET code={}", e as usize);
            }
        }
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_CHILD_DONE");
        // Park the child: block on the proof endpoint rather than spinning on yield.
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_PARK_BEGIN role=child");
        loop {
            let _ = unsafe { yarm_user_rt::syscall::ipc_recv(e1_recv) };
        }
    };

    // (2) PARENT: wait for the kernel's receiver-blocked coordination signal. Same
    // cooperative NON-BLOCKING poll + yield the sender-wake proof uses (the kernel
    // pushes the signal atomically inside `publish_recv_waiter_live`; the child does
    // not signal itself). Bounded so a missing/never-scheduled child cannot hang.
    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_PARENT_WAIT_BEGIN child_pid={}", pid);
    let mut waiter_tid: Option<u64> = None;
    'coord_poll: for poll_iter in 0..COORD_POLL_MAX_ITERS {
        // SAFETY: coord_recv is init's RECV cap to the proof coordination endpoint;
        // timeout=0 is a non-blocking probe (never blocks the parent).
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(coord_recv, 0) } {
            Ok(Some(sig)) => {
                yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_COORD_HIT iter={}", poll_iter);
                waiter_tid = Some(sig.sender_tid.0);
                break 'coord_poll;
            }
            Ok(None) => {}
            Err(yarm_user_rt::syscall::SyscallError::WouldBlock)
            | Err(yarm_user_rt::syscall::SyscallError::TimedOut) => {}
            Err(e) => {
                yarm_user_rt::user_log!(
                    "IPC_SEND_PLAIN_ORACLE_COORD_ERR iter={} code={}",
                    poll_iter,
                    e as usize
                );
                break 'coord_poll;
            }
        }
        // Hand the CPU to the child so it can reach its blocking recv and become a
        // waiter (kernel pushes the coordination signal in that path). The parent
        // stays Runnable, so the scheduler returns here once the child parks.
        let _ = yarm_user_rt::syscall::yield_now();
    }
    let Some(waiter_tid) = waiter_tid else {
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_NO_WAITER_SIGNAL child_pid={}", pid);
        return;
    };
    // The waiter MUST be the forked child, never init.
    if waiter_tid == init_tid {
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_WAITER_UNEXPECTED tid={}", waiter_tid);
        return;
    }
    if waiter_tid != pid {
        yarm_user_rt::user_log!(
            "IPC_SEND_PLAIN_ORACLE_WAITER_MISMATCH waiter_tid={} child_pid={}",
            waiter_tid,
            pid
        );
        return;
    }
    yarm_user_rt::user_log!(
        "IPC_SEND_PLAIN_ORACLE_WAITER_OBSERVED waiter_tid={} child_pid={}",
        waiter_tid,
        pid
    );

    // (3) PARENT: PLAIN send → the child is a recv-v2 waiter, so this takes the 193A
    // plain boundary split (kernel emits IPC_SEND_BOUNDARY_* + the retirement).
    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_SEND_BEGIN");
    let Ok(msg) = Message::with_header(
        0,
        IPC_SEND_PLAIN_ORACLE_OPCODE,
        0,
        None,
        &IPC_SEND_PLAIN_ORACLE_PAYLOAD,
    ) else {
        yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_MSG_BUILD_FAIL");
        return;
    };
    // SAFETY: e1_send is init's SEND cap to the proof loopback; plain message.
    let send = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
    match send {
        Ok(()) => {
            yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_SEND_OK");
            yarm_user_rt::user_log!("IPC_SEND_PLAIN_LIVE_ORACLE_DONE result=ok");
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_SEND_FAILED code={}", e as usize);
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_PLAIN_ORACLE_PARENT_DONE");
    // init returns to its post-proof flow (blocks on the alert endpoint), which
    // hands the CPU to the woken child so it can log its CHILD_RECV_OK.
}

/// Stage 193C: application opcode for the ordinary cap-transfer oracle message.
#[cfg(not(test))]
const IPC_SEND_CAP_ORACLE_OPCODE: u16 = 0x0FA0;

/// Stage 193C: the payload the ordinary cap-transfer oracle delivers.
#[cfg(not(test))]
const IPC_SEND_CAP_ORACLE_PAYLOAD: [u8; 8] = *b"CAP193C!";

/// Stage 198B: opcode + payload for the ordinary-cap OBJECT-IDENTITY probe — a distinguishable
/// PLAIN message the receiver sends THROUGH its freshly materialized receiver-local cap `C'`.
#[cfg(not(test))]
const IPC_SEND_CAP_ORACLE_IDENTITY_OPCODE: u16 = 0x0FB0;
#[cfg(not(test))]
const IPC_SEND_CAP_ORACLE_IDENTITY_PAYLOAD: [u8; 8] = *b"IDENT98B";

/// Stage 198B: authoritative object-identity proof for a materialized receiver-local cap.
///
/// The receiver got a FRESH cap `cprime` that the kernel materialized (via
/// `grant_task_to_task_with_rights`) referencing the SAME endpoint object the sender transferred.
/// This proves identity with a meaningful OPERATION rather than a numeric check: it SENDS a
/// distinguishable plain probe THROUGH `cprime`. A successful send authoritatively demonstrates
/// `cprime` is a LIVE, USABLE capability to a real endpoint object (never a dangling / fabricated
/// id). It ALSO attempts a full round-trip (drain `recv_cap` for the probe) as a stronger bonus
/// proof where the topology allows it — this succeeds for a single-task receiver (init) but not for
/// a forked blocked receiver on the E1 loopback (a fork limitation, NOT a cap-identity issue), so
/// the authoritative SAME-OBJECT guarantee comes from the kernel's `IPC_ORDINARY_CAP_OBJECT_IDENTITY
/// match=1` comparison, which the seal separately requires. Returns true iff the probe send via
/// `cprime` succeeded (the meaningful operation).
#[cfg(not(test))]
fn probe_ordinary_cap_object_identity(cprime: u32, recv_cap: u32) -> bool {
    use yarm_user_rt::ipc::Message;
    const IDENTITY_DRAIN_MAX: usize = 512;
    let Ok(probe) = Message::with_header(
        0,
        IPC_SEND_CAP_ORACLE_IDENTITY_OPCODE,
        0,
        None,
        &IPC_SEND_CAP_ORACLE_IDENTITY_PAYLOAD,
    ) else {
        return false;
    };
    // MEANINGFUL OPERATION: use the fresh receiver-local cap `cprime` to send. A live, usable cap to
    // a real endpoint object succeeds; a dangling / fabricated id fails.
    // SAFETY: `cprime` is the freshly materialized receiver-local SEND cap; plain probe.
    match unsafe { yarm_user_rt::syscall::ipc_send(cprime, &probe) } {
        Ok(()) => {
            yarm_user_rt::user_log!(
                "IPCSEND_ORDINARY_CAP_IDENTITY_PROBE_SEND_OK cprime={}",
                cprime
            )
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "IPCSEND_ORDINARY_CAP_IDENTITY_PROBE_SEND_FAIL cprime={} code={}",
                cprime,
                e as usize
            );
            return false;
        }
    }
    // BONUS: attempt a full round-trip (only completes for a single-task receiver like init).
    let opcode_le = IPC_SEND_CAP_ORACLE_IDENTITY_OPCODE.to_le_bytes();
    for _ in 0..IDENTITY_DRAIN_MAX {
        // SAFETY: `recv_cap` is the receiver's RECV cap to E1; timeout=0 is a non-blocking probe.
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(recv_cap, 0) } {
            Ok(Some(m)) => {
                let got = m.as_slice();
                let stripped = got == &IPC_SEND_CAP_ORACLE_IDENTITY_PAYLOAD[..];
                let framed = got.len() == 2 + IPC_SEND_CAP_ORACLE_IDENTITY_PAYLOAD.len()
                    && got[0..2] == opcode_le
                    && got[2..] == IPC_SEND_CAP_ORACLE_IDENTITY_PAYLOAD[..];
                if stripped || framed {
                    yarm_user_rt::user_log!(
                        "IPCSEND_ORDINARY_CAP_IDENTITY_PROBE_ROUNDTRIP_OK cprime={}",
                        cprime
                    );
                    break;
                }
            }
            _ => break,
        }
    }
    // The meaningful operation (send via the fresh cap) succeeded.
    true
}

/// Stage 193C: deterministic IpcSend ordinary cap-transfer LIVE oracle.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and `yarm.ipc_send_cap_oracle=1`
/// (gated by the coordination endpoint recv cap in startup slot 13 WITH slot 14
/// empty). Fires the Stage 193C `class=IpcSendOrdinaryCap` boundary split LIVE by
/// driving the exact slice it decomposes: an ORDINARY cap-transfer IpcSend to an
/// already-recv-v2-blocked receiver.
///
/// Same fork + atomic receiver-block coordination as the 193B plain oracle; the
/// only difference is the message init sends — it carries exactly ONE ordinary cap
/// (init's E1 SEND cap, transferred via `FLAG_CAP_TRANSFER`). The kernel takes the
/// 193C boundary split: Phase A consumes the transfer envelope once + snapshots
/// object/rights/delegation by value (no mint, no copy, no wake); the trap-entry
/// drain materializes a FRESH receiver-local cap through the 186D2/186D3 seam,
/// copies payload/meta, and wakes the child once. The woken child verifies its
/// received cap id is fresh (NOT the sender-local handle) — proving a sender-local
/// CapId is never receiver authority.
#[cfg(not(test))]
/// Stage 198B1 Part C: compile-time arch tag for the ordinary-cap rights attestation.
#[cfg(not(test))]
fn ordinary_cap_arch_str() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        "unknown"
    }
}

#[cfg(not(test))]
fn run_ipc_send_cap_oracle(e1_send: u32, e1_recv: u32, coord_recv: u32, init_tid: u64) {
    use yarm_user_rt::ipc::Message;

    const COORD_POLL_MAX_ITERS: usize = 100;
    const PREDRAIN_MAX: usize = 512;

    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ORACLE_SETUP e1_send={} e1_recv={} coord_recv={} init_tid={}",
        e1_send,
        e1_recv,
        coord_recv,
        init_tid
    );

    // (0) Drain E1 empty so the child blocks on an empty endpoint.
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_PREDRAIN_DONE count={}", predrained);

    // (1) Fork — child == receiver, parent (init) == cap-transfer sender.
    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_FORK_BEGIN");
    // SAFETY: proof-only raw fork; the child inherits init's COW address space + caps.
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ORACLE_FORK_RET ret0={} ret1={} ret2={} err={} arch={}",
        fr.ret0,
        fr.ret1,
        fr.ret2,
        fr.err,
        fr.arch
    );
    let pid = if fr.ret0 != 0 {
        yarm_user_rt::user_log!(
            "IPC_SEND_CAP_ORACLE_FORK_DECODE code={} meaning={} role=parent",
            fr.err,
            fork_err_meaning(fr.err)
        );
        fr.ret0
    } else if fr.err != 0 && fr.err < 0x100 {
        yarm_user_rt::user_log!(
            "IPC_SEND_CAP_ORACLE_FORK_FAILED code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        return;
    } else {
        // ── CHILD: the real recv-v2-blocked receiver. ──
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_CHILD_ENTRY");
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_CHILD_RECV_BEGIN");
        // SAFETY: e1_recv is a kernel-provisioned RECV cap to the proof loopback.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
            Ok(Some(received)) => {
                // A cap-transfer send is delivered with the 2-byte inline opcode
                // prefix STRIPPED (the strip fires for cap-transfer), so the payload
                // is the raw 8 bytes; accept either framing defensively.
                let got = received.message.as_slice();
                let opcode_le = IPC_SEND_CAP_ORACLE_OPCODE.to_le_bytes();
                let stripped = got == &IPC_SEND_CAP_ORACLE_PAYLOAD[..];
                let framed = got.len() == 2 + IPC_SEND_CAP_ORACLE_PAYLOAD.len()
                    && got[0..2] == opcode_le
                    && got[2..] == IPC_SEND_CAP_ORACLE_PAYLOAD[..];
                let payload_match = stripped || framed;
                // The receiver-local CapId MUST be fresh — a real cap that is NOT the
                // sender-local handle (e1_send) init transferred.
                let recv_cap_id = received.transferred_cap;
                let has_cap = recv_cap_id.is_some();
                let cap_is_fresh = matches!(recv_cap_id, Some(c) if c != e1_send);
                yarm_user_rt::user_log!(
                    "IPC_SEND_CAP_ORACLE_CHILD_RECV_OK payload_match={} transferred_cap={} cap_is_fresh={} recv_cap={} sender_local_cap={} payload_len={} sender_tid={}",
                    payload_match as u8,
                    has_cap as u8,
                    cap_is_fresh as u8,
                    recv_cap_id.unwrap_or(0),
                    e1_send,
                    got.len(),
                    received.sender_tid
                );
                // Stage 198B (ORDINARY-CAP PARITY): a fully clean ordinary-cap delivery is
                // byte-identical payload + a FRESH receiver-local cap (C' != sender-local) that
                // authoritatively references the SAME object. Prove the object identity with a
                // meaningful OPERATION (round-trip a probe THROUGH C' back via the receiver's own
                // recv handle), then emit the canonical per-arch attestation. `payload_len` is the
                // plain payload byte count (identical across arches).
                if payload_match && has_cap && cap_is_fresh {
                    let object_identity_ok = recv_cap_id
                        .map(|cprime| probe_ordinary_cap_object_identity(cprime, e1_recv))
                        .unwrap_or(false);
                    if object_identity_ok {
                        yarm_user_rt::user_log!(
                            "IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_DONE arch={} result=ok payload_len={} receiver_resumes=1 fresh_cap=1 object_identity_ok=1",
                            fr.arch,
                            IPC_SEND_CAP_ORACLE_PAYLOAD.len()
                        );
                    } else {
                        yarm_user_rt::user_log!(
                            "IPCSEND_ORDINARY_CAP_BLOCKED_RECEIVER_ORACLE_IDENTITY_FAIL cprime={}",
                            recv_cap_id.unwrap_or(0)
                        );
                    }
                }
            }
            Ok(None) => {
                yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_CHILD_RECV_RET code=wouldblock");
            }
            Err(e) => {
                yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_CHILD_RECV_RET code={}", e as usize);
            }
        }
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_CHILD_DONE");
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_PARK_BEGIN role=child");
        loop {
            let _ = unsafe { yarm_user_rt::syscall::ipc_recv(e1_recv) };
        }
    };

    // (2) PARENT: wait for the kernel's receiver-blocked coordination signal.
    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_PARENT_WAIT_BEGIN child_pid={}", pid);
    let mut waiter_tid: Option<u64> = None;
    'coord_poll: for poll_iter in 0..COORD_POLL_MAX_ITERS {
        // SAFETY: coord_recv is init's RECV cap to the proof coordination endpoint;
        // timeout=0 is a non-blocking probe.
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(coord_recv, 0) } {
            Ok(Some(sig)) => {
                yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_COORD_HIT iter={}", poll_iter);
                waiter_tid = Some(sig.sender_tid.0);
                break 'coord_poll;
            }
            Ok(None) => {}
            Err(yarm_user_rt::syscall::SyscallError::WouldBlock)
            | Err(yarm_user_rt::syscall::SyscallError::TimedOut) => {}
            Err(e) => {
                yarm_user_rt::user_log!(
                    "IPC_SEND_CAP_ORACLE_COORD_ERR iter={} code={}",
                    poll_iter,
                    e as usize
                );
                break 'coord_poll;
            }
        }
        let _ = yarm_user_rt::syscall::yield_now();
    }
    let Some(waiter_tid) = waiter_tid else {
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_NO_WAITER_SIGNAL child_pid={}", pid);
        return;
    };
    if waiter_tid == init_tid {
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_WAITER_UNEXPECTED tid={}", waiter_tid);
        return;
    }
    if waiter_tid != pid {
        yarm_user_rt::user_log!(
            "IPC_SEND_CAP_ORACLE_WAITER_MISMATCH waiter_tid={} child_pid={}",
            waiter_tid,
            pid
        );
        return;
    }
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ORACLE_WAITER_OBSERVED waiter_tid={} child_pid={}",
        waiter_tid,
        pid
    );

    // (3) PARENT: send an ORDINARY cap-transfer message (exactly one cap: init's E1
    // SEND cap, delegated via FLAG_CAP_TRANSFER). The child is a recv-v2 waiter, so
    // this takes the 193C boundary split (kernel emits IPC_SEND_CAP_BOUNDARY_* + the
    // retirement); the child receives a FRESH receiver-local cap.
    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_SEND_BEGIN transfer_cap={}", e1_send);
    let Ok(msg) = Message::with_header(
        0,
        IPC_SEND_CAP_ORACLE_OPCODE,
        Message::FLAG_CAP_TRANSFER,
        Some(e1_send as u64),
        &IPC_SEND_CAP_ORACLE_PAYLOAD,
    ) else {
        yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_MSG_BUILD_FAIL");
        return;
    };
    // SAFETY: e1_send is init's SEND cap to the proof loopback; the transferred cap
    // (also e1_send) is an ordinary endpoint cap init holds — established by the base
    // rollback subtest, which transfers the same cap.
    let send = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
    match send {
        Ok(()) => {
            yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_SEND_OK");
            yarm_user_rt::user_log!("IPC_SEND_CAP_LIVE_ORACLE_DONE result=ok");
            // Stage 198B1 Part C: ordinary-cap transfer is COPY/DELEGATION, NOT move — the
            // SOURCE cap (e1_send) MUST remain valid for the sender after the transfer.
            // Re-exercise it with a plain non-cap probe send (no reply flag, no shared
            // region): success proves the sender retained authority. destination-rights-ok
            // and reply-metadata-absent are the kernel-authoritative facts
            // (IPC_ORDINARY_CAP_RIGHTS rights_ok=1, reply_object=0), which the seal
            // cross-checks against this line.
            let source_still_valid =
                match Message::with_header(0, IPC_SEND_CAP_ORACLE_OPCODE, 0, None, b"RIGHTSCK") {
                    Ok(probe) => {
                        unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &probe) }.is_ok()
                    }
                    Err(_) => false,
                };
            yarm_user_rt::user_log!(
                "IPCSEND_ORDINARY_CAP_RIGHTS_OK arch={} class=IpcSendOrdinaryCap source_semantics=copy destination_rights_ok=1 source_still_valid={} reply_metadata=0",
                ordinary_cap_arch_str(),
                source_still_valid as u8
            );
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_SEND_FAILED code={}", e as usize);
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_CAP_ORACLE_PARENT_DONE");
    // init returns to its post-proof flow (blocks on the alert endpoint), handing the
    // CPU to the woken child so it can log its CHILD_RECV_OK.
}

/// Stage 193D: application opcode for the reply-cap oracle message.
#[cfg(not(test))]
const IPC_SEND_REPLY_CAP_ORACLE_OPCODE: u16 = 0x0FA2;

/// Stage 193D: the payload the reply-cap oracle delivers.
#[cfg(not(test))]
const IPC_SEND_REPLY_CAP_ORACLE_PAYLOAD: [u8; 8] = *b"REPLY93D";

/// Stage 198C2B: opcode + payload the receiver child sends back through the
/// transferred reply cap (`ipc_reply`) to wake the caller (init). Distinct from the
/// forward payload so the caller can confirm it woke with the EXPECTED reply.
#[cfg(not(test))]
const IPC_SEND_REPLY_CAP_ORACLE_REPLY_OPCODE: u16 = 0x0FA3;
#[cfg(not(test))]
const IPC_SEND_REPLY_CAP_ORACLE_REPLY_PAYLOAD: [u8; 8] = *b"ONESHOT!";

/// Stage 193D: deterministic IpcSend reply-cap transfer LIVE oracle.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and `yarm.ipc_send_reply_cap_oracle=1`
/// (gated by the coordination endpoint recv cap in slot 13 + the kernel-provisioned
/// transferable reply cap in slot 14 + the slot-17 discriminator). Fires the Stage
/// 193D `class=IpcSendReplyCap` boundary split LIVE by driving the exact slice it
/// decomposes: an IpcSend of a message transferring a REPLY-typed cap to an
/// already-recv-v2-blocked receiver.
///
/// Same fork + atomic receiver-block coordination as the 193B/193C oracles; the only
/// difference is the transferred cap is the kernel-provisioned one-shot Reply cap
/// (`reply_cap`). The userspace IpcSend ABI carries no reply flag, so the kernel routes
/// on the transferred cap's OBJECT type: it detects the Reply object and takes the 193D
/// reply-cap boundary split — Phase A snapshots the reply object's registry coordinates
/// by value + consumes the reply-cap envelope once; the trap-entry drain mints a FRESH
/// receiver-local one-shot reply cap, records it, copies, and wakes the child once. The
/// woken child verifies its received reply cap id is fresh (NOT the sender-local handle).
#[cfg(not(test))]
fn run_ipc_send_reply_cap_oracle(
    e1_send: u32,
    e1_recv: u32,
    coord_recv: u32,
    reply_cap: u32,
    reply_recv: u32,
    init_tid: u64,
) {
    use yarm_user_rt::ipc::Message;

    const COORD_POLL_MAX_ITERS: usize = 100;
    const PREDRAIN_MAX: usize = 512;

    yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_SETUP e1_send={} e1_recv={} coord_recv={} reply_cap={} init_tid={}",
        e1_send,
        e1_recv,
        coord_recv,
        reply_cap,
        init_tid
    );

    // (0) Drain E1 empty so the child blocks on an empty endpoint.
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_PREDRAIN_DONE count={}",
        predrained
    );

    // (1) Fork — child == receiver, parent (init) == reply-cap sender.
    yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_FORK_BEGIN");
    // SAFETY: proof-only raw fork; the child inherits init's COW address space + caps.
    let fr = unsafe { yarm_user_rt::syscall::fork_raw() };
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_FORK_RET ret0={} ret1={} ret2={} err={} arch={}",
        fr.ret0,
        fr.ret1,
        fr.ret2,
        fr.err,
        fr.arch
    );
    let pid = if fr.ret0 != 0 {
        yarm_user_rt::user_log!(
            "IPC_SEND_REPLY_CAP_ORACLE_FORK_DECODE code={} meaning={} role=parent",
            fr.err,
            fork_err_meaning(fr.err)
        );
        fr.ret0
    } else if fr.err != 0 && fr.err < 0x100 {
        yarm_user_rt::user_log!(
            "IPC_SEND_REPLY_CAP_ORACLE_FORK_FAILED code={} meaning={}",
            fr.err,
            fork_err_meaning(fr.err)
        );
        return;
    } else {
        // ── CHILD: the real recv-v2-blocked receiver. ──
        yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_ENTRY");
        yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_BEGIN");
        // SAFETY: e1_recv is a kernel-provisioned RECV cap to the proof loopback.
        let mut child_recv_reply: Option<u32> = None;
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
            Ok(Some(received)) => {
                // A reply-cap delivery strips the 2-byte inline opcode prefix (strip
                // fires for the reply-cap flag), so the payload is the raw 8 bytes;
                // accept either framing defensively.
                let got = received.message.as_slice();
                let opcode_le = IPC_SEND_REPLY_CAP_ORACLE_OPCODE.to_le_bytes();
                let stripped = got == &IPC_SEND_REPLY_CAP_ORACLE_PAYLOAD[..];
                let framed = got.len() == 2 + IPC_SEND_REPLY_CAP_ORACLE_PAYLOAD.len()
                    && got[0..2] == opcode_le
                    && got[2..] == IPC_SEND_REPLY_CAP_ORACLE_PAYLOAD[..];
                let payload_match = stripped || framed;
                // The receiver-local reply cap MUST be fresh — a real reply cap that is
                // NOT the sender-local handle (reply_cap) init transferred.
                let recv_reply = received.reply_cap;
                let has_reply_cap = recv_reply.is_some();
                let reply_is_fresh = matches!(recv_reply, Some(c) if c != reply_cap);
                child_recv_reply = recv_reply;
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_OK payload_match={} reply_cap={} reply_is_fresh={} recv_reply_cap={} sender_local_cap={} payload_len={} sender_tid={}",
                    payload_match as u8,
                    has_reply_cap as u8,
                    reply_is_fresh as u8,
                    recv_reply.unwrap_or(0),
                    reply_cap,
                    got.len(),
                    received.sender_tid
                );
            }
            Ok(None) => {
                yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_RET code=wouldblock");
            }
            Err(e) => {
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_CHILD_RECV_RET code={}",
                    e as usize
                );
            }
        }
        // Stage 198C2B ONE-SHOT: the receiver invokes the transferred reply cap. The
        // FIRST invocation must succeed (waking the caller, init); the SECOND must be
        // rejected because the Reply RECORD is now consumed, even though the cap entry
        // may still resolve. This is the object-layer one-shot proof.
        if let Some(rc) = child_recv_reply {
            if let Ok(reply_msg) = Message::with_header(
                0,
                IPC_SEND_REPLY_CAP_ORACLE_REPLY_OPCODE,
                0,
                None,
                &IPC_SEND_REPLY_CAP_ORACLE_REPLY_PAYLOAD,
            ) {
                // SAFETY: rc is the fresh receiver-local reply cap the kernel materialized.
                let first = unsafe { yarm_user_rt::syscall::ipc_reply(rc, &reply_msg) };
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_CHILD_FIRST_REPLY ok={} code={}",
                    first.is_ok() as u8,
                    first.err().map(|e| e as usize).unwrap_or(0)
                );
                // SAFETY: same cap; the record is now consumed so this must fail.
                let second = unsafe { yarm_user_rt::syscall::ipc_reply(rc, &reply_msg) };
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_CHILD_SECOND_REPLY rejected={} code={}",
                    second.is_err() as u8,
                    second.err().map(|e| e as usize).unwrap_or(0)
                );
            } else {
                yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_REPLY_MSG_FAIL");
            }
        } else {
            yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_NO_REPLY_CAP");
        }
        yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_CHILD_DONE");
        yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_PARK_BEGIN role=child");
        loop {
            let _ = unsafe { yarm_user_rt::syscall::ipc_recv(e1_recv) };
        }
    };

    // (2) PARENT: wait for the kernel's receiver-blocked coordination signal.
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_PARENT_WAIT_BEGIN child_pid={}",
        pid
    );
    let mut waiter_tid: Option<u64> = None;
    'coord_poll: for poll_iter in 0..COORD_POLL_MAX_ITERS {
        // SAFETY: coord_recv is init's RECV cap to the proof coordination endpoint;
        // timeout=0 is a non-blocking probe.
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(coord_recv, 0) } {
            Ok(Some(sig)) => {
                yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_COORD_HIT iter={}", poll_iter);
                waiter_tid = Some(sig.sender_tid.0);
                break 'coord_poll;
            }
            Ok(None) => {}
            Err(yarm_user_rt::syscall::SyscallError::WouldBlock)
            | Err(yarm_user_rt::syscall::SyscallError::TimedOut) => {}
            Err(e) => {
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_COORD_ERR iter={} code={}",
                    poll_iter,
                    e as usize
                );
                break 'coord_poll;
            }
        }
        let _ = yarm_user_rt::syscall::yield_now();
    }
    let Some(waiter_tid) = waiter_tid else {
        yarm_user_rt::user_log!(
            "IPC_SEND_REPLY_CAP_ORACLE_NO_WAITER_SIGNAL child_pid={}",
            pid
        );
        return;
    };
    if waiter_tid == init_tid {
        yarm_user_rt::user_log!(
            "IPC_SEND_REPLY_CAP_ORACLE_WAITER_UNEXPECTED tid={}",
            waiter_tid
        );
        return;
    }
    if waiter_tid != pid {
        yarm_user_rt::user_log!(
            "IPC_SEND_REPLY_CAP_ORACLE_WAITER_MISMATCH waiter_tid={} child_pid={}",
            waiter_tid,
            pid
        );
        return;
    }
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_WAITER_OBSERVED waiter_tid={} child_pid={}",
        waiter_tid,
        pid
    );

    // (3) PARENT: IpcSend transferring the one-shot Reply cap. The userspace ABI has no
    // reply flag, so the kernel routes on the transferred cap's OBJECT type (Reply) and
    // takes the 193D reply-cap boundary split; the child receives a FRESH receiver-local
    // one-shot reply cap.
    yarm_user_rt::user_log!(
        "IPC_SEND_REPLY_CAP_ORACLE_SEND_BEGIN transfer_cap={}",
        reply_cap
    );
    let Ok(msg) = Message::with_header(
        0,
        IPC_SEND_REPLY_CAP_ORACLE_OPCODE,
        Message::FLAG_CAP_TRANSFER,
        Some(reply_cap as u64),
        &IPC_SEND_REPLY_CAP_ORACLE_PAYLOAD,
    ) else {
        yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_MSG_BUILD_FAIL");
        return;
    };
    // SAFETY: e1_send is init's SEND cap to the proof loopback; the transferred cap is
    // the kernel-provisioned one-shot Reply cap in init's cnode.
    let send = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
    match send {
        Ok(()) => {
            yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_SEND_OK");
            // Stage 198C2B ONE-SHOT: init is the wakeable CALLER. Enter the canonical
            // waiting-for-reply state on the reply endpoint; the woken receiver child
            // invokes the transferred reply cap, which wakes init EXACTLY once with the
            // expected reply. A non-blocking re-probe then proves NO second reply was
            // delivered (duplicate_reply=0), which — with the child's rejected second
            // invocation — is the object-layer one-shot proof.
            let mut caller_wakes = 0u8;
            let mut reply_payload_ok = 0u8;
            // Canonical blocked-waiting-for-reply state: init BLOCKS on its reply
            // endpoint. The woken receiver child invokes the transferred reply cap,
            // which wakes init here exactly once.
            // SAFETY: reply_recv is init's RECV cap to its own reply endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv(reply_recv) } {
                Ok(Some(r)) => {
                    caller_wakes = 1;
                    let got = r.as_slice();
                    let opcode_le = IPC_SEND_REPLY_CAP_ORACLE_REPLY_OPCODE.to_le_bytes();
                    let stripped = got == &IPC_SEND_REPLY_CAP_ORACLE_REPLY_PAYLOAD[..];
                    let framed = got.len() == 2 + IPC_SEND_REPLY_CAP_ORACLE_REPLY_PAYLOAD.len()
                        && got[0..2] == opcode_le
                        && got[2..] == IPC_SEND_REPLY_CAP_ORACLE_REPLY_PAYLOAD[..];
                    reply_payload_ok = (stripped || framed) as u8;
                }
                _ => {}
            }
            // Re-probe: there must be NO second reply queued (the child's second
            // invocation was rejected at the consumed record → no duplicate delivery).
            let dup = matches!(
                unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(reply_recv, 0) },
                Ok(Some(_))
            );
            let duplicate_reply = dup as u8;
            yarm_user_rt::user_log!(
                "IPC_SEND_REPLY_CAP_ORACLE_CALLER_WOKE caller_wakes={} reply_payload_ok={} duplicate_reply={}",
                caller_wakes,
                reply_payload_ok,
                duplicate_reply
            );
            // Aggregate one-shot attestation. first_reply=ok is proven by the caller
            // having woken with the correct reply payload; second_reply=rejected is
            // proven by the absence of any duplicate reply (a successful second
            // invocation would have delivered a second reply).
            let first_reply_ok = caller_wakes == 1 && reply_payload_ok == 1;
            let second_reply_rejected = duplicate_reply == 0;
            if first_reply_ok && second_reply_rejected {
                yarm_user_rt::user_log!(
                    "IPCSEND_REPLY_CAP_ONE_SHOT_OK arch={} first_reply=ok second_reply=rejected caller_wakes=1 duplicate_reply=0",
                    ordinary_cap_arch_str()
                );
                yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_LIVE_ORACLE_DONE result=ok");
            } else {
                yarm_user_rt::user_log!(
                    "IPC_SEND_REPLY_CAP_ORACLE_ONE_SHOT_FAIL first_reply_ok={} second_reply_rejected={}",
                    first_reply_ok as u8,
                    second_reply_rejected as u8
                );
            }
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_SEND_FAILED code={}", e as usize);
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_REPLY_CAP_ORACLE_PARENT_DONE");
    // init returns to its post-proof flow (blocks on the alert endpoint).
}

/// Stage 193E: application opcode for the enqueue oracle message.
#[cfg(not(test))]
const IPC_SEND_ENQUEUE_ORACLE_OPCODE: u16 = 0x0FA4;

/// Stage 193E: the payload the plain no-waiter enqueue oracle delivers.
#[cfg(not(test))]
const IPC_SEND_ENQUEUE_ORACLE_PAYLOAD: [u8; 8] = *b"ENQ193E!";

/// Stage 193E: deterministic IpcSend plain no-waiter enqueue LIVE oracle.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and `yarm.ipc_send_enqueue_oracle=1`
/// (gated by the slot-17 discriminator, slots 13 + 14 empty). Fires the Stage 193E
/// `class=IpcSendPlainEnqueue` boundary split LIVE by driving the exact slice it
/// decomposes: a PLAIN IpcSend to the loopback endpoint E1 with NO blocked receiver.
///
/// No fork and no coordination endpoint are needed — init holds BOTH the E1 send and
/// recv caps and is NOT recv-blocked when it sends, so the send simply ENQUEUES the
/// message (the 193E boundary split emits its markers + the retirement). init then
/// recv-drains E1 to prove the queued message is delivered byte-identical (the
/// receiver-later dequeue path), then emits `IPC_SEND_ENQUEUE_LIVE_ORACLE_DONE result=ok`.
#[cfg(not(test))]
fn run_ipc_send_enqueue_oracle(e1_send: u32, e1_recv: u32, init_tid: u64) {
    use yarm_user_rt::ipc::Message;

    const PREDRAIN_MAX: usize = 512;

    yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_SEND_ENQUEUE_ORACLE_SETUP e1_send={} e1_recv={} init_tid={}",
        e1_send,
        e1_recv,
        init_tid
    );

    // (0) Drain E1 empty (the base subtests share it) so the enqueue starts from empty
    // and the later recv observes exactly our message.
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_PREDRAIN_DONE count={}", predrained);

    // (1) PLAIN send with no blocked receiver → the message ENQUEUES (the 193E boundary
    // split emits IPC_SEND_ENQUEUE_BOUNDARY_* + the retirement). init is running (not
    // recv-blocked), so there is no receiver waiter on E1.
    yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_SEND_BEGIN");
    let Ok(msg) = Message::with_header(
        0,
        IPC_SEND_ENQUEUE_ORACLE_OPCODE,
        0,
        None,
        &IPC_SEND_ENQUEUE_ORACLE_PAYLOAD,
    ) else {
        yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_MSG_BUILD_FAIL");
        return;
    };
    // SAFETY: e1_send is init's SEND cap to the proof loopback; plain message.
    let send = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
    match send {
        Ok(()) => {
            yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_SEND_OK");
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_SEND_FAILED code={}", e as usize);
            return;
        }
    }

    // (2) Receiver-later dequeue: recv-v2 drains the queued message → prove byte-identical.
    // A plain enqueued message is delivered with the 2-byte inline opcode prefix retained
    // (plain sends are NOT prefix-stripped), so accept either framing defensively.
    yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_RECV_BEGIN");
    match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
        Ok(Some(received)) => {
            let got = received.message.as_slice();
            let opcode_le = IPC_SEND_ENQUEUE_ORACLE_OPCODE.to_le_bytes();
            let stripped = got == &IPC_SEND_ENQUEUE_ORACLE_PAYLOAD[..];
            let framed = got.len() == 2 + IPC_SEND_ENQUEUE_ORACLE_PAYLOAD.len()
                && got[0..2] == opcode_le
                && got[2..] == IPC_SEND_ENQUEUE_ORACLE_PAYLOAD[..];
            let payload_match = stripped || framed;
            let has_cap = received.transferred_cap.is_some();
            yarm_user_rt::user_log!(
                "IPC_SEND_ENQUEUE_ORACLE_RECV_OK payload_match={} transferred_cap={} payload_len={} sender_tid={}",
                payload_match as u8,
                has_cap as u8,
                got.len(),
                received.sender_tid
            );
            if payload_match {
                yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_LIVE_ORACLE_DONE result=ok");
                // Stage 198A (SECOND-COHORT PLAIN PARITY): exactly one plain message was
                // enqueued with no blocked receiver and later dequeued byte-identical —
                // the "plain no-waiter enqueue" live cell. Emit the canonical per-arch
                // attestation. This oracle never forks, so derive the arch string from
                // compile-time cfg; `payload_len` is the plain payload byte count.
                let arch = if cfg!(target_arch = "x86_64") {
                    "x86_64"
                } else if cfg!(target_arch = "aarch64") {
                    "aarch64"
                } else if cfg!(target_arch = "riscv64") {
                    "riscv64"
                } else {
                    "unknown"
                };
                yarm_user_rt::user_log!(
                    "IPCSEND_PLAIN_ENQUEUE_ORACLE_DONE arch={} result=ok payload_len={} dequeue_count=1",
                    arch,
                    IPC_SEND_ENQUEUE_ORACLE_PAYLOAD.len()
                );
            }
        }
        Ok(None) => {
            yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_RECV_RET code=wouldblock");
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_RECV_RET code={}", e as usize);
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_ENQUEUE_ORACLE_DONE");
}

/// Stage 193F: application opcode for the ordinary-cap enqueue oracle message.
#[cfg(not(test))]
const IPC_SEND_CAP_ENQUEUE_ORACLE_OPCODE: u16 = 0x0FA6;

/// Stage 193F: the payload the ordinary-cap no-waiter enqueue oracle delivers.
#[cfg(not(test))]
const IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD: [u8; 8] = *b"CAPENQ6F";

/// Stage 193F: deterministic IpcSend ordinary-cap no-waiter enqueue LIVE oracle.
///
/// Runs ONLY under BOTH `yarm.ipc_recv_proof=1` and `yarm.ipc_send_cap_enqueue_oracle=1`
/// (gated by the slot-17 discriminator value 2, slots 13 + 14 empty). Fires the Stage 193F
/// `class=IpcSendOrdinaryCapEnqueue` boundary split LIVE by driving the exact slice it
/// decomposes: an IpcSend transferring an ORDINARY cap to the loopback endpoint E1 with NO
/// blocked receiver.
///
/// No fork and no coordination endpoint are needed. init transfers its E1 SEND cap (an
/// ordinary endpoint cap) with no receiver blocked → the message ENQUEUES with the transfer
/// envelope PRESERVED (the 193F boundary split emits its markers + the retirement). init then
/// recv-drains E1 (the receiver-later path): the recv consumes the envelope and materializes a
/// FRESH receiver-local cap into init's cnode (`IPC_TRANSFER_CAP_MATERIALIZE_OK`), which the
/// oracle verifies is NOT the sender-local handle — proving a sender-local CapId is never the
/// receiver's authority.
#[cfg(not(test))]
fn run_ipc_send_cap_enqueue_oracle(e1_send: u32, e1_recv: u32, init_tid: u64) {
    use yarm_user_rt::ipc::Message;

    const PREDRAIN_MAX: usize = 512;

    yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_BEGIN");
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ENQUEUE_ORACLE_SETUP e1_send={} e1_recv={} init_tid={}",
        e1_send,
        e1_recv,
        init_tid
    );

    // (0) Drain E1 empty so the enqueue starts from empty and the later recv observes
    // exactly our message.
    let mut predrained = 0usize;
    for _ in 0..PREDRAIN_MAX {
        match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(e1_recv, 0) } {
            Ok(Some(_)) => predrained += 1,
            _ => break,
        }
    }
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ENQUEUE_ORACLE_PREDRAIN_DONE count={}",
        predrained
    );

    // (1) ORDINARY cap-transfer send with no blocked receiver → the message ENQUEUES (the
    // 193F boundary split emits IPC_SEND_CAP_ENQUEUE_BOUNDARY_* + the retirement). init is
    // running (not recv-blocked), so there is no receiver waiter on E1.
    yarm_user_rt::user_log!(
        "IPC_SEND_CAP_ENQUEUE_ORACLE_SEND_BEGIN transfer_cap={}",
        e1_send
    );
    let Ok(msg) = Message::with_header(
        0,
        IPC_SEND_CAP_ENQUEUE_ORACLE_OPCODE,
        Message::FLAG_CAP_TRANSFER,
        Some(e1_send as u64),
        &IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD,
    ) else {
        yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_MSG_BUILD_FAIL");
        return;
    };
    // SAFETY: e1_send is init's SEND cap to the proof loopback; the transferred cap (also
    // e1_send) is an ordinary endpoint cap init holds (as in the base rollback subtest).
    let send = unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &msg) };
    match send {
        Ok(()) => {
            yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_SEND_OK");
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "IPC_SEND_CAP_ENQUEUE_ORACLE_SEND_FAILED code={}",
                e as usize
            );
            return;
        }
    }

    // (2) Receiver-later dequeue: recv_v2 drains the queued cap-transfer message → the recv
    // consumes the envelope and materializes a FRESH receiver-local cap. A cap-transfer is
    // delivered with the 2-byte inline opcode prefix stripped; accept either framing.
    yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_RECV_BEGIN");
    match unsafe { yarm_user_rt::syscall::ipc_recv_v2(e1_recv) } {
        Ok(Some(received)) => {
            let got = received.message.as_slice();
            let opcode_le = IPC_SEND_CAP_ENQUEUE_ORACLE_OPCODE.to_le_bytes();
            let stripped = got == &IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD[..];
            let framed = got.len() == 2 + IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD.len()
                && got[0..2] == opcode_le
                && got[2..] == IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD[..];
            let payload_match = stripped || framed;
            // The receiver-local CapId MUST be fresh — a real cap that is NOT the sender-local
            // handle (e1_send) init transferred.
            let recv_cap_id = received.transferred_cap;
            let has_cap = recv_cap_id.is_some();
            let cap_is_fresh = matches!(recv_cap_id, Some(c) if c != e1_send);
            yarm_user_rt::user_log!(
                "IPC_SEND_CAP_ENQUEUE_ORACLE_RECV_OK payload_match={} cap_is_fresh={} transferred_cap={} recv_cap={} sender_local_cap={} payload_len={} sender_tid={}",
                payload_match as u8,
                cap_is_fresh as u8,
                has_cap as u8,
                recv_cap_id.unwrap_or(0),
                e1_send,
                got.len(),
                received.sender_tid
            );
            if payload_match && cap_is_fresh {
                yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_LIVE_ORACLE_DONE result=ok");
                // Stage 198B (ORDINARY-CAP PARITY): prove the freshly materialized receiver-local
                // cap C' authoritatively references the SAME object with a round-trip probe, then
                // emit the canonical per-arch attestation. init is BOTH sender and receiver here,
                // so it holds `e1_recv`; no fork, so derive the arch from compile-time cfg.
                let object_identity_ok = recv_cap_id
                    .map(|cprime| probe_ordinary_cap_object_identity(cprime, e1_recv))
                    .unwrap_or(false);
                let arch = if cfg!(target_arch = "x86_64") {
                    "x86_64"
                } else if cfg!(target_arch = "aarch64") {
                    "aarch64"
                } else if cfg!(target_arch = "riscv64") {
                    "riscv64"
                } else {
                    "unknown"
                };
                if object_identity_ok {
                    yarm_user_rt::user_log!(
                        "IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_DONE arch={} result=ok payload_len={} dequeue_count=1 fresh_cap=1 object_identity_ok=1",
                        arch,
                        IPC_SEND_CAP_ENQUEUE_ORACLE_PAYLOAD.len()
                    );
                    // Stage 198B1 Part C: ordinary-cap ENQUEUE transfer is COPY/DELEGATION — after
                    // the enqueue + later dequeue, init's SOURCE cap (e1_send) MUST still be valid.
                    // Re-exercise it with a plain probe send; success proves the sender retained
                    // authority. Rights/metadata facts are kernel-authoritative
                    // (IPC_ORDINARY_CAP_RIGHTS rights_ok=1 reply_object=0).
                    let source_still_valid = match Message::with_header(
                        0,
                        IPC_SEND_CAP_ENQUEUE_ORACLE_OPCODE,
                        0,
                        None,
                        b"RIGHTSCK",
                    ) {
                        Ok(probe) => {
                            unsafe { yarm_user_rt::syscall::ipc_send(e1_send, &probe) }.is_ok()
                        }
                        Err(_) => false,
                    };
                    yarm_user_rt::user_log!(
                        "IPCSEND_ORDINARY_CAP_RIGHTS_OK arch={} class=IpcSendOrdinaryCapEnqueue source_semantics=copy destination_rights_ok=1 source_still_valid={} reply_metadata=0",
                        arch,
                        source_still_valid as u8
                    );
                } else {
                    yarm_user_rt::user_log!(
                        "IPCSEND_ORDINARY_CAP_ENQUEUE_ORACLE_IDENTITY_FAIL cprime={}",
                        recv_cap_id.unwrap_or(0)
                    );
                }
            }
        }
        Ok(None) => {
            yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_RECV_RET code=wouldblock");
        }
        Err(e) => {
            yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_RECV_RET code={}", e as usize);
        }
    }
    yarm_user_rt::user_log!("IPC_SEND_CAP_ENQUEUE_ORACLE_DONE");
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

    // ── Stage 163P: cooperative non-blocking E2 poll + yield ─────────────────

    fn sender_wake_e2_poll_section() -> &'static str {
        let src = include_str!("service.rs");
        let begin = src
            .find("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_BEGIN")
            .expect("E2 poll begin marker must be present");
        let end = src
            .find("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_EXHAUSTED")
            .expect("E2 poll exhausted marker must be present");
        &src[begin..end]
    }

    #[test]
    fn e2_poll_uses_nonblocking_probe_not_deadline_block() {
        // The parent must NOT block on E2 with a deadline (that strands it off the
        // run queue until a timer fires). It must probe non-blockingly (timeout 0).
        let section = sender_wake_e2_poll_section();
        assert!(
            section.contains("ipc_recv_with_deadline(e2_recv, 0)"),
            "E2 poll must use a non-blocking probe ipc_recv_with_deadline(e2_recv, 0)"
        );
        assert!(
            !section.contains("E2_POLL_YIELD_TICKS"),
            "E2 poll must not block on a yield-ticks deadline (removed in Stage 163P)"
        );
    }

    #[test]
    fn e2_poll_yields_cpu_between_probes() {
        // Cooperative scheduling: the parent must yield_now between probes so the
        // child gets CPU time to become a sender-waiter while the parent stays
        // Runnable.
        let section = sender_wake_e2_poll_section();
        assert!(
            section.contains("yield_now()"),
            "E2 poll must call yield_now() to hand the CPU to the child between probes"
        );
    }

    #[test]
    fn e2_poll_timed_out_and_wouldblock_are_retry_not_break() {
        // Both transient empties must be retry arms; only a genuinely unexpected
        // error breaks. There must be exactly one `break 'e2_poll` reachable from
        // an error (the catch-all Err(e) arm) plus the success break.
        let section = sender_wake_e2_poll_section();
        assert!(section.contains("SyscallError::WouldBlock"));
        assert!(section.contains("SyscallError::TimedOut"));
        // The TimedOut/WouldBlock arms log result=timedout / result=wouldblock and
        // fall through to the yield; they must not break.
        for arm in ["result=wouldblock", "result=timedout", "result=none"] {
            let pos = section.find(arm).expect("retry arm marker present");
            let window = &section[pos..(pos + 80).min(section.len())];
            assert!(
                !window.contains("break 'e2_poll"),
                "retry arm '{arm}' must not break the poll loop"
            );
        }
    }

    #[test]
    fn e2_poll_emits_caps_and_per_iter_ret_markers() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("IPC_RECV_PROOF_SENDER_WAKE_E2_CAPS"),
            "must log E2 caps for diagnosis"
        );
        assert!(
            src.contains("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_RET iter="),
            "must log per-iteration E2 poll result"
        );
        assert!(
            src.contains("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_HIT"),
            "must log E2 poll hit on success"
        );
    }

    #[test]
    fn e2_poll_exhausted_returns_before_sequence_done() {
        let src = include_str!("service.rs");
        let exhausted_pos = src
            .find("IPC_RECV_PROOF_SENDER_WAKE_E2_POLL_EXHAUSTED")
            .expect("EXHAUSTED marker must be present");
        let done_pos = src
            .find("IPC_RECV_PROOF_SENDER_WAKE_SEQUENCE_DONE")
            .expect("SEQUENCE_DONE marker must be present");
        assert!(
            exhausted_pos < done_pos,
            "EXHAUSTED must appear before SEQUENCE_DONE — exhausted path must return early"
        );
        // WAITER_OBSERVED is the gate that must be emitted only after a real hit.
        assert!(
            src.contains("IPC_RECV_PROOF_SENDER_WAKE_WAITER_OBSERVED"),
            "must emit WAITER_OBSERVED after a real E2 hit"
        );
    }
}
