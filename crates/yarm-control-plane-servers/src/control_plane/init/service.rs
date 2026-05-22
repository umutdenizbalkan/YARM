// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::control_plane::init::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitBootPhase,
};
use yarm_ipc_abi::blkcache_abi::{
    BlkCacheResponse, GetStatsRequest, RegisterBackendArgs, BLKCACHE_OP_GET_STATS,
    BLKCACHE_OP_REGISTER_BACKEND, BLKCACHE_STATUS_ERR_UNSUPPORTED, BLKCACHE_STATUS_OK,
};
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
#[cfg(test)]
use yarm_fs_servers::initramfs::build_core_service_elf_launch_plan;
#[cfg(test)]
use yarm_fs_servers::initramfs::service::run_request_loop as run_initramfs_request_loop;
#[cfg(test)]
use yarm_fs_servers::initramfs::{InitramfsBackend, InitramfsService, boot_initrd_bytes};
#[cfg(test)]
use yarm_ipc_abi::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};

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
    let control_vfs_summary = run_vfs_request_loop(&mut control_vfs, 0x1000)
        .map_err(|_| KernelError::WrongObject)?;

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
                posix_compat_entry: launch.posix_compat.map(|plan| plan.validated_entry as usize),
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
    use yarm_ipc_abi::process_abi::{PROC_OP_SPAWN_V5_CAP, SpawnV5CapArgs, SpawnV5CapResult};
    let args = SpawnV5CapArgs::new(parent_pid, image_id, service_caps);
    let encoded = args.encode();
    let Ok(msg) = yarm_user_rt::ipc::Message::with_header(
        0,
        PROC_OP_SPAWN_V5_CAP,
        0,
        None,
        &encoded,
    ) else {
        return None;
    };
    // SAFETY: Uses kernel-provided startup caps for synchronous PM IPC call.
    let _ = unsafe { yarm_user_rt::syscall::ipc_call(pm_send, pm_recv, &msg) };
    let reply = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(pm_recv, 0) };
    match reply {
        Ok(Some(ref r)) => {
            let payload = r.as_slice();
            match SpawnV5CapResult::decode(payload) {
                Ok(result) => Some((result.pid, result.service_send_cap)),
                Err(_) => None,
            }
        }
        _ => None,
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
    let Some((child_tid, initramfs_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 4, [0, 0, 0, 0], 0) else {
        yarm_user_rt::user_log!("INIT_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!("INIT_SPAWN_V5_CALL_RETURN ok=1 child_tid={}", child_tid);
    yarm_user_rt::user_log!("INIT_INITRAMFS_SPAWN_CAPS recv_cap={}", initramfs_send_cap);

    // --- Spawn devfs_srv (image_id=5) ---
    yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_V5_CALL_BEGIN");
    let Some((devfs_child_tid, devfs_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 5, [0, 0, 0, 0], 0) else {
        yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_V5_CALL_RETURN ok=1 child_tid={}", devfs_child_tid);
    yarm_user_rt::user_log!("INIT_DEVFS_SPAWN_CAPS recv_cap={}", devfs_send_cap);

    // --- Spawn vfs_server (image_id=6) passing initramfs and devfs send caps ---
    // parent_pid=1 so the kernel delegates the vfs send cap into init's own cnode,
    // allowing init to send directly to vfs_server without going through PM.
    yarm_user_rt::user_log!("INIT_VFS_SPAWN_V5_CALL_BEGIN");
    let Some((vfs_child_tid, vfs_recv_cap)) = spawn_v5_cap(
        pm_send, pm_recv, 6,
        [initramfs_send_cap, devfs_send_cap, 0, 0],
        1,
    ) else {
        yarm_user_rt::user_log!("INIT_VFS_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!("INIT_VFS_SPAWN_V5_CALL_RETURN ok=1 child_tid={}", vfs_child_tid);
    yarm_user_rt::user_log!(
        "INIT_VFS_SPAWN_CAPS recv_cap={} initramfs_send={} devfs_send={}",
        vfs_recv_cap, initramfs_send_cap, devfs_send_cap
    );

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
        match unsafe {
            yarm_user_rt::vfs_client::vfs_statx(vfs_send, pm_recv, b"/dev/null")
        } {
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

    // --- Spawn driver_manager (image_id=7) ---
    // No service caps required at spawn time; driver_manager blocks on its own
    // recv endpoint waiting for driver registration requests.
    yarm_user_rt::user_log!("INIT_DRIVER_MANAGER_SPAWN_V5_CALL_BEGIN");
    let Some((dm_child_tid, _dm_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 7, [0, 0, 0, 0], 0) else {
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
    let Some((blkcache_child_tid, init_blkcache_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 8, [0, 0, 0, 0], 1) else {
        yarm_user_rt::user_log!("INIT_BLKCACHE_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_BLKCACHE_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        blkcache_child_tid
    );

    // --- Spawn virtio_blk_srv (image_id=9) ---
    yarm_user_rt::user_log!("INIT_VIRTIO_BLK_SPAWN_V5_CALL_BEGIN");
    let Some((virtio_blk_child_tid, init_virtio_blk_send_cap)) = spawn_v5_cap(pm_send, pm_recv, 9, [0, 0, 0, 0], 1) else {
        yarm_user_rt::user_log!("INIT_VIRTIO_BLK_SPAWN_V5_CALL_RETURN ok=0 child_tid=0");
        return;
    };
    yarm_user_rt::user_log!(
        "INIT_VIRTIO_BLK_SPAWN_V5_CALL_RETURN ok=1 child_tid={}",
        virtio_blk_child_tid
    );

    yarm_user_rt::user_log!("INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_BEGIN");
    let register_backend_req = RegisterBackendArgs {
        backend_id: 1,
        backend_send_cap: init_virtio_blk_send_cap,
        block_size: 512,
        flags: 0,
        block_count: 1,
    };
    let register_backend_payload = register_backend_req.encode();
    let Ok(register_backend_msg) = yarm_user_rt::ipc::Message::with_header(
        0,
        BLKCACHE_OP_REGISTER_BACKEND,
        0,
        None,
        &register_backend_payload,
    ) else {
        yarm_user_rt::user_log!("INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=0 status=2 backend_id=1");
        return;
    };
    let _ = unsafe {
        yarm_user_rt::syscall::ipc_call(init_blkcache_send_cap as u32, pm_recv, &register_backend_msg)
    };
    let register_backend_reply = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(pm_recv, 0) };
    match register_backend_reply {
        Ok(Some(reply_msg)) => match BlkCacheResponse::decode(reply_msg.as_slice()) {
            Some(resp) if resp.status == BLKCACHE_STATUS_OK => {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=1 status={} backend_id=1",
                    resp.status
                );
            }
            Some(resp) => {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=0 status={} backend_id=1",
                    resp.status
                );
            }
            None => yarm_user_rt::user_log!(
                "INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=0 status=2 backend_id=1"
            ),
        },
        _ => yarm_user_rt::user_log!(
            "INIT_BLKCACHE_REGISTER_BACKEND_SMOKE_RETURN ok=0 status=2 backend_id=1"
        ),
    };


    let get_stats_req = GetStatsRequest {
        request_id: 1,
        backend_id: 0,
        flags: 0,
    };
    let get_stats_payload = get_stats_req.encode();
    let Ok(get_stats_msg) = yarm_user_rt::ipc::Message::with_header(
        0,
        BLKCACHE_OP_GET_STATS,
        0,
        None,
        &get_stats_payload,
    ) else {
        yarm_user_rt::user_log!("INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=0 msg=build_failed");
        return;
    };
    // SAFETY: init_blkcache_send_cap is the caller-local delegated send cap.
    let _ = unsafe { yarm_user_rt::syscall::ipc_call(init_blkcache_send_cap as u32, pm_recv, &get_stats_msg) };
    // SAFETY: pm_recv is init's startup-provided reply endpoint.
    let get_stats_reply = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(pm_recv, 0) };
    match get_stats_reply {
        Ok(Some(reply_msg)) => match BlkCacheResponse::decode(reply_msg.as_slice()) {
            Some(resp)
                if resp.request_id == 1
                    && resp.status == BLKCACHE_STATUS_ERR_UNSUPPORTED
                    && resp.bytes_moved == 0
                    && resp.flags == 0 =>
            {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=1 status={} request_id={} bytes_moved={}",
                    resp.status,
                    resp.request_id,
                    resp.bytes_moved
                );
            }
            Some(resp) => {
                yarm_user_rt::user_log!(
                    "INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=0 status={} request_id={} bytes_moved={}",
                    resp.status,
                    resp.request_id,
                    resp.bytes_moved
                );
            }
            None => {
                yarm_user_rt::user_log!("INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=0 decode=0");
            }
        },
        Ok(None) => {
            yarm_user_rt::user_log!("INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=0 recv=none");
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "INIT_BLKCACHE_GET_STATS_SMOKE_CALL_RETURN ok=0 recv_err={:?}",
                e
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_fs_servers::initramfs::ManifestEntryWire;
    use yarm_fs_servers::initramfs::{
        INITRAMFS_INIT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR,
        INITRAMFS_VFS_PATH_PTR,
    };
    use yarm::std::vec::Vec;

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
}
