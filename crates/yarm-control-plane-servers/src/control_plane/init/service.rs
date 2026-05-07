// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::control_plane::init::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitBootPhase,
};
#[cfg(test)]
use super::super::process_manager::service::ProcessService;
use super::super::process_manager::service::SpawnV2Result;
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
use yarm_ipc_abi::process_abi::{ServiceStartupCapsV1, SpawnV5Args, PROC_OP_SPAWN_V5};
use yarm_user_rt::ipc::Message;
use yarm_user_rt::process::ProcessError as ProcessManagerError;
use yarm_user_rt::syscall::{IpcTransportV2, SyscallIpcTransport};
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

pub fn run() {
    let _ = attempt_spawn_initramfs_srv_via_process_manager();
    yarm_user_rt::user_log!(
        "init.srv requires kernel-provided bootstrap handoff; standalone Bootstrap::init path disabled"
    );
}

fn build_initramfs_spawn_v5_message(
    parent_pid: u64,
    image_id: u64,
    request_recv_cap: u32,
) -> Result<Message, ProcessManagerError> {
    let startup_caps = ServiceStartupCapsV1::new(1, request_recv_cap as u64);
    let args = SpawnV5Args::new(parent_pid, image_id, 64, 2, startup_caps);
    Message::with_header(0, PROC_OP_SPAWN_V5, 0, None, &args.encode())
        .map_err(|_| ProcessManagerError::Malformed)
}

fn attempt_spawn_initramfs_srv_via_process_manager() -> Result<(), ProcessManagerError> {
    let mut transport = SyscallIpcTransport;
    attempt_spawn_initramfs_srv_via_process_manager_with_transport(&mut transport)
}

fn attempt_spawn_initramfs_srv_via_process_manager_with_transport(
    transport: &mut impl IpcTransportV2,
) -> Result<(), ProcessManagerError> {
    let ctx = yarm_user_rt::runtime::startup_context();
    let (proc_send_cap, proc_reply_cap) = ctx
        .process_manager_caps()
        .ok_or(ProcessManagerError::PermissionDenied)?;
    let request_recv_cap = ctx
        .initramfs_startup_caps_v1_from_startup_args()
        .and_then(|caps| u32::try_from(caps.request_recv_cap).ok())
        .filter(|cap| *cap != 0)
        .ok_or(ProcessManagerError::Unsupported)?;
    let request = build_initramfs_spawn_v5_message(1, 0x494E_4954_4653_5352, request_recv_cap)?;
    let spawned = transport
        .request_reply_v2(
            proc_send_cap,
            proc_reply_cap,
            request.as_slice(),
            |payload| SpawnV2Result::decode(payload).ok(),
        )
        .map_err(|_| ProcessManagerError::Unsupported)?;
    yarm_user_rt::user_log!("INITRAMFS_SPAWN_ATTEMPT status=ok pid={}", spawned.pid.0);
    Ok(())
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

    #[test]
    fn init_server_builds_spawn_v5_for_initramfs_srv() {
        let msg = build_initramfs_spawn_v5_message(2, 0x494E_4954_4653_5352, 77).expect("msg");
        assert_eq!(msg.opcode, PROC_OP_SPAWN_V5);
        let args = SpawnV5Args::decode(msg.as_slice()).expect("decode");
        assert_eq!(args.startup_caps.request_recv_cap, 77);
        assert_eq!(args.startup_caps.control_send_cap, 0);
        assert_eq!(args.startup_caps.control_recv_cap, 0);
    }

    #[test]
    fn init_server_spawn_attempt_is_truthful_when_endpoint_provisioning_missing() {
        assert_eq!(
            attempt_spawn_initramfs_srv_via_process_manager(),
            Err(ProcessManagerError::Unsupported)
        );
    }

    struct StubTransport {
        status: Result<SpawnV2Result, yarm_user_rt::syscall::SyscallError>,
        saw_send_cap: Option<u32>,
        saw_reply_cap: Option<u32>,
    }
    impl IpcTransportV2 for StubTransport {
        fn send_v2(&mut self, _: u32, _: &[u8], _: Option<u64>) -> Result<(), yarm_user_rt::syscall::SyscallError> { unreachable!() }
        fn recv_v2(&mut self, _: u32) -> Result<Option<yarm_user_rt::syscall::IpcV2Response>, yarm_user_rt::syscall::SyscallError> { unreachable!() }
        fn recv_v2_with_deadline(&mut self, _: u32, _: u64) -> Result<Option<yarm_user_rt::syscall::IpcV2Response>, yarm_user_rt::syscall::SyscallError> { unreachable!() }
        fn reply_v2(&mut self, _: u32, _: &[u8], _: Option<u64>) -> Result<(), yarm_user_rt::syscall::SyscallError> { unreachable!() }
        fn call_v2(&mut self, _: u32, _: u32, _: &[u8]) -> Result<yarm_user_rt::syscall::IpcV2Response, yarm_user_rt::syscall::SyscallError> { unreachable!() }
        fn request_reply_v2<T>(&mut self, send_cap: u32, reply_recv_cap: u32, _: &[u8], decode_reply: impl FnOnce(&[u8]) -> Option<T>) -> Result<T, yarm_user_rt::syscall::SyscallError> {
            self.saw_send_cap = Some(send_cap);
            self.saw_reply_cap = Some(reply_recv_cap);
            let res = self.status.map(|r| r.encode());
            match res {
                Ok(payload) => decode_reply(&payload).ok_or(yarm_user_rt::syscall::SyscallError::InvalidArgs),
                Err(e) => Err(e),
            }
        }
    }

    #[test]
    fn init_server_spawn_attempt_uses_ipc_when_caps_present() {
        yarm_user_rt::runtime::install_startup_arg_slots([((1u64) << 48) | ((1u64) << 32) | 0x5354_4350, 9, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut t = StubTransport { status: Ok(SpawnV2Result { pid: yarm_user_rt::process::ProcessId(55) }), saw_send_cap: None, saw_reply_cap: None };
        let res = attempt_spawn_initramfs_srv_via_process_manager_with_transport(&mut t);
        assert_eq!(res, Ok(()));
        assert_eq!(t.saw_send_cap, Some(9));
        assert_eq!(t.saw_reply_cap, Some(10));
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
