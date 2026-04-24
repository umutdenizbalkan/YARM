// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

mod launch;
mod mount;
mod policy;

use yarm_fs_servers::common::service::FsService;
use yarm_fs_servers::common::vfs_ipc::{
    OpenAtRequest, ReadWriteRequest, StatxRequest, openat_message, statx_message, write_message,
};
use yarm_fs_servers::devfs::service::run_request_loop as run_devfs_request_loop;
use yarm_fs_servers::devfs::{DevFsBackend, DevFsService};
use yarm_fs_servers::ext4::{Ext4Backend, Ext4Service};
use yarm_fs_servers::fat::{FatBackend, FatService};
use yarm_fs_servers::initramfs::service::run_request_loop as run_initramfs_request_loop;
use yarm_fs_servers::initramfs::{InitramfsBackend, InitramfsService};
use yarm_fs_servers::ramfs::{RamFsBackend, RamFsService};
#[cfg(test)]
use yarm::kernel::boot::{KernelError, KernelState, UserImageSpec};
#[cfg(not(test))]
use yarm_user_rt::runtime::KernelIpcError as KernelError;
use yarm_user_rt::capability::CapId;
#[cfg(test)]
use yarm_user_rt::capability::CapRights;
#[cfg(test)]
use yarm_user_rt::task::{TaskClass, TaskStatus};
#[cfg(test)]
use yarm_user_rt::vm::Asid;
#[cfg(test)]
use yarm_user_rt::vm::PAGE_SIZE;
use alloc::boxed::Box;
#[cfg(test)]
use yarm_ipc_abi::supervisor_abi::{
    InitAlert, InitAlertKind, RegisterCoreServiceRequest, RegisterDriverRequest,
    SUPERVISOR_OP_REGISTER_CORE_SERVICE, SUPERVISOR_OP_REGISTER_DRIVER, TaskExitedEvent,
};
use yarm_srv_common::vfs_reply::VfsReply;

pub use launch::{CoreLaunchReport, CoreServiceGraph, CoreServiceHandles, CoreServiceImagePlan};
pub use mount::{MountPlan, MountRecoveryReport, MountServiceKind};
pub use policy::{
    CoreLaunchStrategy, CoreServiceKind, CoreServicePolicyTable, InitBootPhase, InitFaultHandoff,
    RestartOwner, ServiceRestartPolicy, StartupCap, StartupCapSet,
};

#[cfg(test)]
fn map_task_status(status: TaskStatus) -> TaskStatus {
    match status {
        TaskStatus::Runnable => TaskStatus::Runnable,
        TaskStatus::Running => TaskStatus::Running,
        TaskStatus::Blocked => TaskStatus::Blocked,
        TaskStatus::Faulted => TaskStatus::Faulted,
        TaskStatus::Exited(code) => TaskStatus::Exited(code),
        TaskStatus::Dead => TaskStatus::Dead,
    }
}

#[cfg(test)]
fn to_kernel_task_class(class: TaskClass) -> TaskClass {
    match class {
        TaskClass::App => TaskClass::App,
        TaskClass::Driver => TaskClass::Driver,
        TaskClass::SystemServer => TaskClass::SystemServer,
    }
}

#[cfg(test)]
fn map_kernel_asid(asid: Asid) -> Asid {
    Asid(asid.0)
}

#[cfg(test)]
fn to_kernel_asid(asid: Asid) -> Asid {
    Asid(asid.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitService {
    phase: InitBootPhase,
    handles: CoreServiceHandles,
    startup_caps: StartupCapSet,
    fault_handoff: Option<InitFaultHandoff>,
    restart_policies: CoreServicePolicyTable,
    launch_strategy: CoreLaunchStrategy,
    launch_order: [Option<CoreServiceKind>; 3],
    launch_count: usize,
    mount_plan: MountPlan,
    mount_status: Option<MountRecoveryReport>,
    process_manager_restart_count: u8,
    vfs_restart_count: u8,
    supervisor_restart_count: u8,
    #[cfg(test)]
    supervisor_replay_log: [Option<SupervisorReplayEntry>; 16],
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorReplayEntry {
    Core(RegisterCoreServiceRequest),
    Driver(RegisterDriverRequest),
}

impl Default for InitService {
    fn default() -> Self {
        Self::new()
    }
}

impl InitService {
    #[cfg(test)]
    fn register_core_service_message(
        sender_tid: u64,
        request: RegisterCoreServiceRequest,
    ) -> Result<yarm_user_rt::ipc::Message, ()> {
        yarm_user_rt::ipc::Message::with_header(
            sender_tid,
            SUPERVISOR_OP_REGISTER_CORE_SERVICE,
            0,
            None,
            &request.encode(),
        )
        .map_err(|_| ())
    }

    #[cfg(test)]
    fn register_driver_message(
        sender_tid: u64,
        request: RegisterDriverRequest,
    ) -> Result<yarm_user_rt::ipc::Message, ()> {
        yarm_user_rt::ipc::Message::with_header(
            sender_tid,
            SUPERVISOR_OP_REGISTER_DRIVER,
            0,
            None,
            &request.encode(),
        )
        .map_err(|_| ())
    }

    pub const fn new() -> Self {
        Self {
            phase: InitBootPhase::Uninitialized,
            handles: CoreServiceHandles {
                init_tid: None,
                process_manager_tid: None,
                vfs_tid: None,
                supervisor_tid: None,
                posix_compat_tid: None,
            },
            startup_caps: StartupCapSet::core_required_minimum(),
            fault_handoff: None,
            restart_policies: CoreServicePolicyTable::baseline(),
            launch_strategy: CoreLaunchStrategy::ProcessManagerFirst,
            launch_order: [None; 3],
            launch_count: 0,
            mount_plan: MountPlan::baseline(),
            mount_status: None,
            process_manager_restart_count: 0,
            vfs_restart_count: 0,
            supervisor_restart_count: 0,
            #[cfg(test)]
            supervisor_replay_log: [None; 16],
        }
    }

    pub const fn phase(&self) -> InitBootPhase {
        self.phase
    }

    pub const fn handles(&self) -> CoreServiceHandles {
        self.handles
    }

    pub const fn startup_caps(&self) -> StartupCapSet {
        self.startup_caps
    }

    pub const fn fault_handoff(&self) -> Option<InitFaultHandoff> {
        self.fault_handoff
    }

    pub const fn restart_policies(&self) -> CoreServicePolicyTable {
        self.restart_policies
    }

    pub fn set_restart_policies(
        &mut self,
        policies: CoreServicePolicyTable,
    ) -> Result<(), KernelError> {
        if !policies.is_sane() {
            return Err(KernelError::InvalidCapability);
        }
        self.restart_policies = policies;
        Ok(())
    }

    pub const fn launch_order(&self) -> [Option<CoreServiceKind>; 3] {
        self.launch_order
    }

    pub const fn launch_strategy(&self) -> CoreLaunchStrategy {
        self.launch_strategy
    }

    pub const fn mount_plan(&self) -> MountPlan {
        self.mount_plan
    }

    pub const fn mount_status(&self) -> Option<MountRecoveryReport> {
        self.mount_status
    }

    pub const fn restart_counts(&self) -> (u8, u8, u8) {
        (
            self.process_manager_restart_count,
            self.vfs_restart_count,
            self.supervisor_restart_count,
        )
    }

    pub fn set_mount_plan(&mut self, plan: MountPlan) -> Result<(), KernelError> {
        if plan.count == 0 || plan.count > plan.order.len() {
            return Err(KernelError::WrongObject);
        }
        self.mount_plan = plan;
        self.mount_status = None;
        Ok(())
    }

    pub fn set_launch_strategy(&mut self, strategy: CoreLaunchStrategy) {
        self.launch_strategy = strategy;
    }

    #[cfg(test)]
    fn record_launch(
        &mut self,
        kernel: &mut KernelState,
        kind: CoreServiceKind,
        tid: u64,
        entry: usize,
        asid: Asid,
    ) -> Result<(), KernelError> {
        self.launch_order[self.launch_count] = Some(kind);
        self.launch_count += 1;
        kernel.spawn_user_task_from_image(UserImageSpec {
            tid,
            entry,
            asid: Some(to_kernel_asid(asid)),
            class: to_kernel_task_class(TaskClass::SystemServer),
            startup_args: UserImageSpec::DEFAULT_STARTUP_ARGS,
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn delegate_process_manager_startup_caps_for_compat(
        kernel: &mut KernelState,
        process_manager_tid: u64,
        compat_tid: u64,
    ) -> Result<[u64; 3], KernelError> {
        // Startup ABI slots for compat server:
        //   arg0 => compat task id
        //   arg1 => process-manager request SEND cap in compat task
        //   arg2 => process-manager reply RECEIVE cap in compat task
        let source_tid = 0;

        let (_, request_send_root, request_recv_root) = kernel.create_endpoint(16)?;
        let request_send_compat = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            request_send_root,
            compat_tid,
            CapRights::SEND,
        )?;
        let _request_recv_process_manager = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            request_recv_root,
            process_manager_tid,
            CapRights::RECEIVE,
        )?;

        let (_, reply_send_root, reply_recv_root) = kernel.create_endpoint(16)?;
        let _reply_send_process_manager = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            reply_send_root,
            process_manager_tid,
            CapRights::SEND,
        )?;
        let reply_recv_compat = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            reply_recv_root,
            compat_tid,
            CapRights::RECEIVE,
        )?;

        Ok([compat_tid, request_send_compat.0, reply_recv_compat.0])
    }

    #[cfg(test)]
    fn allocate_core_service_asids(
        kernel: &mut KernelState,
    ) -> Result<(Asid, Asid, Asid), KernelError> {
        let (proc_asid, _proc_aspace_cap) = kernel.create_user_address_space()?;
        let (vfs_asid, _vfs_aspace_cap) = kernel.create_user_address_space()?;
        let (supervisor_asid, _supervisor_aspace_cap) = kernel.create_user_address_space()?;
        Ok((
            map_kernel_asid(proc_asid),
            map_kernel_asid(vfs_asid),
            map_kernel_asid(supervisor_asid),
        ))
    }

    pub fn execute_mount_plan_with_fail_at(
        &self,
        fail_at: Option<usize>,
    ) -> Result<MountRecoveryReport, KernelError> {
        let mut mounted = 0usize;
        let mut recovered = false;
        for idx in 0..self.mount_plan.count {
            if fail_at == Some(idx) {
                if self.mount_plan.allow_fallback_to_fat {
                    run_mount_service(MountServiceKind::Fat)?;
                    recovered = true;
                    mounted = mounted.saturating_add(1);
                    break;
                }
                return Err(KernelError::WrongObject);
            }
            if let Some(kind) = self.mount_plan.order[idx] {
                run_mount_service(kind)?;
                mounted = mounted.saturating_add(1);
            }
        }
        Ok(MountRecoveryReport {
            mounted_count: mounted,
            recovered_with_fat: recovered,
        })
    }

    pub fn set_startup_caps(&mut self, caps: StartupCapSet) {
        self.startup_caps = caps;
    }

    pub fn validate_startup_caps(&self) -> Result<(), KernelError> {
        let min = StartupCapSet::core_required_minimum();
        if min.endpoint_factory && !self.startup_caps.endpoint_factory {
            return Err(KernelError::MissingRight);
        }
        if min.memory_object_factory && !self.startup_caps.memory_object_factory {
            return Err(KernelError::MissingRight);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn register_core_graph(
        &mut self,
        kernel: &mut KernelState,
        graph: CoreServiceGraph,
    ) -> Result<(), KernelError> {
        self.validate_startup_caps()?;
        kernel.register_task(graph.init_tid)?;
        kernel.register_task(graph.process_manager_tid)?;
        kernel.register_task(graph.vfs_tid)?;
        kernel.register_task(graph.supervisor_tid)?;
        if let Some(posix_tid) = graph.posix_compat_tid {
            kernel.register_task(posix_tid)?;
        }

        self.handles.init_tid = Some(graph.init_tid);
        self.handles.process_manager_tid = Some(graph.process_manager_tid);
        self.handles.vfs_tid = Some(graph.vfs_tid);
        self.handles.supervisor_tid = Some(graph.supervisor_tid);
        self.handles.posix_compat_tid = graph.posix_compat_tid;
        self.phase = InitBootPhase::CoreServicesRegistered;
        Ok(())
    }

    #[cfg(test)]
    pub fn launch_core_services(
        &mut self,
        kernel: &mut KernelState,
        plan: CoreServiceImagePlan,
    ) -> Result<CoreLaunchReport, KernelError> {
        self.launch_core_services_with_mount_fail_at(kernel, plan, None)
    }

    #[cfg(test)]
    pub fn launch_core_services_with_mount_fail_at(
        &mut self,
        kernel: &mut KernelState,
        plan: CoreServiceImagePlan,
        fail_at: Option<usize>,
    ) -> Result<CoreLaunchReport, KernelError> {
        if self.phase != InitBootPhase::CoreServicesRegistered {
            return Err(KernelError::WrongObject);
        }
        self.phase = InitBootPhase::LaunchingCore;
        if !self.restart_policies.is_sane() {
            return Err(KernelError::InvalidCapability);
        }
        self.launch_order = [None; 3];
        self.launch_count = 0;
        let (proc_asid, vfs_asid, supervisor_asid) = Self::allocate_core_service_asids(kernel)?;

        let proc_tid = self
            .handles
            .process_manager_tid
            .ok_or(KernelError::WrongObject)?;
        let vfs_tid = self.handles.vfs_tid.ok_or(KernelError::WrongObject)?;
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;
        let posix_compat_tid = self.handles.posix_compat_tid;
        let posix_compat_entry = plan.posix_compat_entry;
        let mut posix_compat_spawned = false;

        match self.launch_strategy {
            CoreLaunchStrategy::ProcessManagerFirst => {
                self.record_launch(
                    kernel,
                    CoreServiceKind::ProcessManager,
                    proc_tid,
                    plan.process_manager_entry,
                    proc_asid,
                )?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::Vfs,
                    vfs_tid,
                    plan.vfs_entry,
                    vfs_asid,
                )?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::Supervisor,
                    supervisor_tid,
                    plan.supervisor_entry,
                    supervisor_asid,
                )?;
            }
            CoreLaunchStrategy::SupervisorFirst => {
                self.record_launch(
                    kernel,
                    CoreServiceKind::Supervisor,
                    supervisor_tid,
                    plan.supervisor_entry,
                    supervisor_asid,
                )?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::ProcessManager,
                    proc_tid,
                    plan.process_manager_entry,
                    proc_asid,
                )?;
                self.record_launch(
                    kernel,
                    CoreServiceKind::Vfs,
                    vfs_tid,
                    plan.vfs_entry,
                    vfs_asid,
                )?;
            }
        }

        if let (Some(tid), Some(entry)) = (posix_compat_tid, posix_compat_entry) {
            let (compat_asid, _compat_aspace_cap) = kernel.create_user_address_space()?;
            let compat_startup_args = Self::delegate_process_manager_startup_caps_for_compat(
                kernel,
                proc_tid,
                tid,
            )?;
            kernel.spawn_user_task_from_image(UserImageSpec {
                tid,
                entry,
                asid: Some(to_kernel_asid(compat_asid)),
                class: to_kernel_task_class(TaskClass::SystemServer),
                startup_args: compat_startup_args,
            })?;
            posix_compat_spawned = true;
        }

        let mount_status = self.execute_mount_plan_with_fail_at(fail_at)?;
        self.mount_status = Some(mount_status);

        Ok(CoreLaunchReport {
            process_manager_spawned: true,
            vfs_spawned: true,
            supervisor_spawned: true,
            posix_compat_spawned,
        })
    }

    #[cfg(test)]
    pub fn install_fault_handoff(
        &mut self,
        kernel: &mut KernelState,
        restart_window_ticks: u64,
    ) -> Result<InitFaultHandoff, KernelError> {
        if self.phase != InitBootPhase::LaunchingCore {
            return Err(KernelError::WrongObject);
        }
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;
        let init_tid = self.handles.init_tid.ok_or(KernelError::WrongObject)?;
        let (_, _, fault_recv_cap) = kernel.create_endpoint(16)?;
        let source_tid = kernel.current_tid().ok_or(KernelError::TaskMissing)?;
        let local_fault_recv_cap = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            fault_recv_cap,
            init_tid,
            CapRights::RECEIVE,
        )?;
        kernel.set_supervisor_endpoint_for_task(init_tid, local_fault_recv_cap)?;
        let (_, control_send_cap, control_recv_cap) = kernel.create_endpoint(16)?;
        let local_control_send_cap = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            control_send_cap,
            init_tid,
            CapRights::SEND,
        )?;
        let local_control_recv_cap = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            control_recv_cap,
            init_tid,
            CapRights::RECEIVE,
        )?;
        let (_, init_alert_send_cap, init_alert_recv_cap) = kernel.create_endpoint(16)?;
        let local_init_alert_send_cap = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            init_alert_send_cap,
            init_tid,
            CapRights::SEND,
        )?;
        let local_init_alert_recv_cap = kernel.grant_capability_task_to_task_with_rights(
            source_tid,
            init_alert_recv_cap,
            init_tid,
            CapRights::RECEIVE,
        )?;
        let _ = local_control_send_cap;
        let _ = local_control_recv_cap;
        let _ = local_init_alert_send_cap;
        let _ = local_init_alert_recv_cap;
        let handoff = InitFaultHandoff::new(
            supervisor_tid,
            fault_recv_cap,
            control_send_cap,
            control_recv_cap,
            init_alert_send_cap,
            init_alert_recv_cap,
            restart_window_ticks,
        );
        self.fault_handoff = Some(handoff);
        Ok(handoff)
    }

    pub fn fault_endpoint_cap(&self) -> Option<CapId> {
        self.fault_handoff
            .map(|handoff| handoff.supervisor_fault_recv_cap)
    }

    #[cfg(test)]
    pub fn validate_delegation_edges(&self, kernel: &KernelState) -> Result<(), KernelError> {
        let handoff = self.fault_handoff.ok_or(KernelError::WrongObject)?;
        let init_tid = self.handles.init_tid.ok_or(KernelError::WrongObject)?;
        let source_tid = kernel.current_tid();
        let has_right_for = |tid: u64, cap: CapId, right: CapRights| {
            kernel
                .task_capability(tid, cap)
                .map(|capability| capability.has_right(right))
                .unwrap_or(false)
        };
        let has_right = |cap: CapId, right: CapRights| {
            has_right_for(init_tid, cap, right)
                || source_tid
                    .map(|tid| has_right_for(tid, cap, right))
                    .unwrap_or(false)
        };
        if !has_right(handoff.supervisor_fault_recv_cap, CapRights::RECEIVE)
            || !has_right(handoff.supervisor_control_send_cap, CapRights::SEND)
            || !has_right(handoff.supervisor_control_recv_cap, CapRights::RECEIVE)
            || !has_right(handoff.init_alert_send_cap, CapRights::SEND)
            || !has_right(handoff.init_alert_recv_cap, CapRights::RECEIVE)
        {
            return Err(KernelError::MissingRight);
        }
        Ok(())
    }

    #[cfg(test)]
    fn remember_supervisor_replay_entry(
        &mut self,
        entry: SupervisorReplayEntry,
    ) -> Result<(), KernelError> {
        let tid = match entry {
            SupervisorReplayEntry::Core(request) => request.tid,
            SupervisorReplayEntry::Driver(request) => request.tid,
        };
        for slot in &mut self.supervisor_replay_log {
            match slot {
                Some(SupervisorReplayEntry::Core(existing)) if existing.tid == tid => {
                    *slot = Some(entry);
                    return Ok(());
                }
                Some(SupervisorReplayEntry::Driver(existing)) if existing.tid == tid => {
                    *slot = Some(entry);
                    return Ok(());
                }
                None => {
                    *slot = Some(entry);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(KernelError::TaskTableFull)
    }

    #[cfg(test)]
    fn send_supervisor_replay_entry(
        &self,
        kernel: &mut KernelState,
        entry: SupervisorReplayEntry,
    ) -> Result<(), KernelError> {
        let handoff = self.fault_handoff.ok_or(KernelError::WrongObject)?;
        let init_tid = self.handles.init_tid.ok_or(KernelError::WrongObject)?;
        let msg = match entry {
            SupervisorReplayEntry::Core(request) => {
                Self::register_core_service_message(init_tid, request)
                    .map_err(|_| KernelError::WrongObject)?
            }
            SupervisorReplayEntry::Driver(request) => {
                Self::register_driver_message(init_tid, request)
                    .map_err(|_| KernelError::WrongObject)?
            }
        };
        kernel.ipc_send(handoff.supervisor_control_send_cap, msg)
    }

    #[cfg(test)]
    pub fn seed_supervisor_registrations(
        &mut self,
        kernel: &mut KernelState,
    ) -> Result<usize, KernelError> {
        use yarm_ipc_abi::supervisor_abi::{
            CoreServiceRegistrationKind, DEP_VFS, RegisterCoreServiceRequest,
        };

        let proc_tid = self
            .handles
            .process_manager_tid
            .ok_or(KernelError::WrongObject)?;
        let vfs_tid = self.handles.vfs_tid.ok_or(KernelError::WrongObject)?;
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;
        let requests = [
            RegisterCoreServiceRequest {
                tid: proc_tid,
                kind: CoreServiceRegistrationKind::ProcessManager,
                max_restarts: self.restart_policies.process_manager.max_restarts,
                restart_group: 1,
                dependency_mask: DEP_VFS,
                backoff_ticks: self.restart_policies.process_manager.backoff_ticks,
            },
            RegisterCoreServiceRequest {
                tid: vfs_tid,
                kind: CoreServiceRegistrationKind::Vfs,
                max_restarts: self.restart_policies.vfs.max_restarts,
                restart_group: 1,
                dependency_mask: 0,
                backoff_ticks: self.restart_policies.vfs.backoff_ticks,
            },
            RegisterCoreServiceRequest {
                tid: supervisor_tid,
                kind: CoreServiceRegistrationKind::Supervisor,
                max_restarts: self.restart_policies.supervisor.max_restarts,
                restart_group: 2,
                dependency_mask: 0,
                backoff_ticks: self.restart_policies.supervisor.backoff_ticks,
            },
        ];
        let mut replayed = 0usize;
        for request in requests {
            let entry = SupervisorReplayEntry::Core(request);
            self.remember_supervisor_replay_entry(entry)?;
            self.send_supervisor_replay_entry(kernel, entry)?;
            replayed += 1;
        }
        Ok(replayed)
    }

    #[cfg(test)]
    pub fn register_driver_with_supervisor(
        &mut self,
        kernel: &mut KernelState,
        request: RegisterDriverRequest,
    ) -> Result<(), KernelError> {
        let entry = SupervisorReplayEntry::Driver(request);
        self.remember_supervisor_replay_entry(entry)?;
        self.send_supervisor_replay_entry(kernel, entry)
    }

    #[cfg(test)]
    pub fn restore_supervisor_control_plane(
        &self,
        kernel: &mut KernelState,
    ) -> Result<usize, KernelError> {
        let mut replayed = 0usize;
        for entry in self.supervisor_replay_log.iter().flatten().copied() {
            self.send_supervisor_replay_entry(kernel, entry)?;
            replayed += 1;
        }
        Ok(replayed)
    }

    #[cfg(test)]
    fn clear_supervisor_control_queue(&self, kernel: &mut KernelState) -> Result<(), KernelError> {
        let handoff = self.fault_handoff.ok_or(KernelError::WrongObject)?;
        while kernel
            .try_ipc_recv(handoff.supervisor_control_recv_cap)?
            .is_some()
        {}
        Ok(())
    }

    #[cfg(test)]
    pub fn poll_init_alert(
        &self,
        kernel: &mut KernelState,
    ) -> Result<Option<InitAlert>, KernelError> {
        let handoff = self.fault_handoff.ok_or(KernelError::WrongObject)?;
        let Some(msg) = kernel.try_ipc_recv(handoff.init_alert_recv_cap)? else {
            return Ok(None);
        };
        InitAlert::decode(msg.as_slice())
            .ok_or(KernelError::WrongObject)
            .map(Some)
    }

    #[cfg(test)]
    pub fn monitor_supervisor(&mut self, kernel: &mut KernelState) -> Result<bool, KernelError> {
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;
        self.monitor_core_service(kernel, supervisor_tid)
    }

    #[cfg(test)]
    pub fn recover_supervisor_failure(
        &mut self,
        kernel: &mut KernelState,
        restart_token: u64,
    ) -> Result<bool, KernelError> {
        self.recover_core_service_failure(kernel, CoreServiceKind::Supervisor, restart_token)
    }

    pub fn mark_failed(&mut self) {
        self.phase = InitBootPhase::Failed;
    }

    #[cfg(test)]
    fn core_service_kind_for_tid(&self, tid: u64) -> Option<CoreServiceKind> {
        if self.handles.process_manager_tid == Some(tid) {
            Some(CoreServiceKind::ProcessManager)
        } else if self.handles.vfs_tid == Some(tid) {
            Some(CoreServiceKind::Vfs)
        } else if self.handles.supervisor_tid == Some(tid) {
            Some(CoreServiceKind::Supervisor)
        } else {
            None
        }
    }

    #[cfg(test)]
    fn core_service_tid(&self, kind: CoreServiceKind) -> Option<u64> {
        match kind {
            CoreServiceKind::ProcessManager => self.handles.process_manager_tid,
            CoreServiceKind::Vfs => self.handles.vfs_tid,
            CoreServiceKind::Supervisor => self.handles.supervisor_tid,
        }
    }

    #[cfg(test)]
    fn restart_count_for(&self, kind: CoreServiceKind) -> u8 {
        match kind {
            CoreServiceKind::ProcessManager => self.process_manager_restart_count,
            CoreServiceKind::Vfs => self.vfs_restart_count,
            CoreServiceKind::Supervisor => self.supervisor_restart_count,
        }
    }

    #[cfg(test)]
    fn increment_restart_count(&mut self, kind: CoreServiceKind) {
        match kind {
            CoreServiceKind::ProcessManager => {
                self.process_manager_restart_count =
                    self.process_manager_restart_count.saturating_add(1)
            }
            CoreServiceKind::Vfs => {
                self.vfs_restart_count = self.vfs_restart_count.saturating_add(1)
            }
            CoreServiceKind::Supervisor => {
                self.supervisor_restart_count = self.supervisor_restart_count.saturating_add(1)
            }
        }
    }

    #[cfg(test)]
    pub fn recover_core_service_failure(
        &mut self,
        kernel: &mut KernelState,
        kind: CoreServiceKind,
        restart_token: u64,
    ) -> Result<bool, KernelError> {
        let tid = self
            .core_service_tid(kind)
            .ok_or(KernelError::WrongObject)?;
        let policy = self.restart_policies.policy_for(kind);
        if self.restart_count_for(kind) >= policy.max_restarts {
            kernel.mark_task_dead(tid)?;
            self.mark_failed();
            return Ok(false);
        }
        kernel.restart_task(tid, restart_token)?;
        self.increment_restart_count(kind);
        if matches!(kind, CoreServiceKind::Supervisor) {
            if let Some(handoff) = self.fault_handoff {
                if let Some(msg) = kernel.try_ipc_recv(handoff.supervisor_fault_recv_cap)? {
                    let event =
                        TaskExitedEvent::decode(msg.as_slice()).ok_or(KernelError::WrongObject)?;
                    if event.tid != tid {
                        return Err(KernelError::WrongObject);
                    }
                }
                let init_tid = self.handles.init_tid.ok_or(KernelError::WrongObject)?;
                let source_tid = kernel.current_tid().ok_or(KernelError::TaskMissing)?;
                let local_fault_recv = kernel.grant_capability_task_to_task_with_rights(
                    source_tid,
                    handoff.supervisor_fault_recv_cap,
                    init_tid,
                    CapRights::RECEIVE,
                )?;
                kernel.set_supervisor_endpoint_for_task(init_tid, local_fault_recv)?;
                self.clear_supervisor_control_queue(kernel)?;
                let _ = self.restore_supervisor_control_plane(kernel)?;
            }
        }
        Ok(true)
    }

    #[cfg(test)]
    pub fn recover_core_service_failure_by_tid(
        &mut self,
        kernel: &mut KernelState,
        tid: u64,
        restart_token: u64,
    ) -> Result<bool, KernelError> {
        let kind = self
            .core_service_kind_for_tid(tid)
            .ok_or(KernelError::WrongObject)?;
        self.recover_core_service_failure(kernel, kind, restart_token)
    }

    #[cfg(test)]
    pub fn handle_init_alert(
        &mut self,
        kernel: &mut KernelState,
        alert: InitAlert,
    ) -> Result<bool, KernelError> {
        match alert.kind {
            InitAlertKind::CoreServiceRestartRequired | InitAlertKind::SupervisorRestarted => {
                let token = kernel
                    .task_restart_token(alert.tid)
                    .ok_or(KernelError::WrongObject)?;
                self.recover_core_service_failure_by_tid(kernel, alert.tid, token)
            }
            InitAlertKind::ServiceDegraded => {
                self.mark_failed();
                Ok(false)
            }
            InitAlertKind::RedelegationRequired => Ok(false),
        }
    }

    #[cfg(test)]
    pub fn monitor_core_service(
        &mut self,
        kernel: &mut KernelState,
        tid: u64,
    ) -> Result<bool, KernelError> {
        if !matches!(
            kernel.task_status(tid).map(map_task_status),
            Some(TaskStatus::Exited(_))
        ) {
            return Ok(false);
        }
        let token = kernel
            .task_restart_token(tid)
            .ok_or(KernelError::WrongObject)?;
        self.recover_core_service_failure_by_tid(kernel, tid, token)
    }

    #[cfg(test)]
    pub fn monitor_core_failures(
        &mut self,
        kernel: &mut KernelState,
    ) -> Result<usize, KernelError> {
        let mut recovered = 0usize;
        while let Some(alert) = self.poll_init_alert(kernel)? {
            if self.handle_init_alert(kernel, alert)? {
                recovered += 1;
            }
        }
        for kind in [
            CoreServiceKind::ProcessManager,
            CoreServiceKind::Vfs,
            CoreServiceKind::Supervisor,
        ] {
            if CoreServicePolicyTable::restart_owner_for(kind) != RestartOwner::Init {
                continue;
            }
            let Some(tid) = self.core_service_tid(kind) else {
                continue;
            };
            if self.monitor_core_service(kernel, tid)? {
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    #[cfg(test)]
    pub fn validate_boot_contract(&self, kernel: &KernelState) -> Result<(), KernelError> {
        let init_tid = self.handles.init_tid.ok_or(KernelError::WrongObject)?;
        let process_manager_tid = self
            .handles
            .process_manager_tid
            .ok_or(KernelError::WrongObject)?;
        let vfs_tid = self.handles.vfs_tid.ok_or(KernelError::WrongObject)?;
        let supervisor_tid = self
            .handles
            .supervisor_tid
            .ok_or(KernelError::WrongObject)?;
        let _ = init_tid;
        if self.mount_status.is_none() || self.fault_handoff.is_none() {
            return Err(KernelError::WrongObject);
        }
        self.validate_delegation_edges(kernel)?;
        for tid in [process_manager_tid, vfs_tid, supervisor_tid] {
            if kernel.task_status(tid).is_none() {
                return Err(KernelError::TaskMissing);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn validate_begin_running_preconditions(&self) -> Result<(), KernelError> {
        if self.phase != InitBootPhase::LaunchingCore
            || self.fault_handoff.is_none()
            || self.mount_status.is_none()
        {
            return Err(KernelError::WrongObject);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn begin_running(&mut self, kernel: &KernelState) -> Result<(), KernelError> {
        self.validate_begin_running_preconditions()?;
        self.validate_boot_contract(kernel)?;
        self.phase = InitBootPhase::Running;
        Ok(())
    }
}

fn run_mount_service(kind: MountServiceKind) -> Result<(), KernelError> {
    match kind {
        MountServiceKind::Initramfs => run_mount_initramfs(),
        MountServiceKind::RamFs => run_mount_ramfs(),
        MountServiceKind::DevFs => run_mount_devfs(),
        MountServiceKind::Ext4 => run_mount_ext4(),
        MountServiceKind::Fat => run_mount_fat(),
    }
}

fn run_mount_initramfs() -> Result<(), KernelError> {
    let mut service = Box::new(InitramfsService::with_backend(InitramfsBackend::new(4096)));
    let summary =
        run_initramfs_request_loop(service.as_mut()).map_err(|_| KernelError::WrongObject)?;
    if summary.write_allowed {
        return Err(KernelError::WrongObject);
    }
    Ok(())
}

fn run_mount_ramfs() -> Result<(), KernelError> {
    let mut service = Box::new(RamFsService::with_backend(RamFsBackend::new()));
    run_rw_mount_cycle(service.as_mut(), 0xA100, 64)
}

fn run_mount_devfs() -> Result<(), KernelError> {
    let mut service = Box::new(DevFsService::with_backend(DevFsBackend::default()));
    let _ = run_devfs_request_loop(service.as_mut()).map_err(|_| KernelError::WrongObject)?;
    Ok(())
}

fn run_mount_ext4() -> Result<(), KernelError> {
    let mut service = Box::new(Ext4Service::with_backend(Ext4Backend::new()));
    run_rw_mount_cycle(service.as_mut(), 0x4040, 4096)
}

fn run_mount_fat() -> Result<(), KernelError> {
    let mut service = Box::new(FatService::with_backend(FatBackend::new()));
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x5050,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let open_reply = service.handle(open).map_err(|_| KernelError::WrongObject)?;
    let fd = VfsReply::from_opcode_payload_checked(open_reply.opcode, open_reply.as_slice())
        .map_err(|_| KernelError::WrongObject)?
        .as_u64();
    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 33,
    })
    .map_err(|_| KernelError::WrongObject)?;
    service
        .handle(write)
        .map_err(|_| KernelError::WrongObject)?;
    Ok(())
}

fn run_rw_mount_cycle<B: yarm_fs_servers::common::vfs_ipc::VfsBackend>(
    service: &mut FsService<B>,
    path_ptr: u64,
    write_len: u64,
) -> Result<(), KernelError> {
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr,
        flags: 0,
        mode: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    let open_reply = service.handle(open).map_err(|_| KernelError::WrongObject)?;
    let fd = VfsReply::from_opcode_payload_checked(open_reply.opcode, open_reply.as_slice())
        .map_err(|_| KernelError::WrongObject)?
        .as_u64();
    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: write_len,
    })
    .map_err(|_| KernelError::WrongObject)?;
    service
        .handle(write)
        .map_err(|_| KernelError::WrongObject)?;
    let stat = statx_message(StatxRequest {
        dirfd: 0,
        path_ptr,
        flags: 0,
        mask_or_buf: 0,
    })
    .map_err(|_| KernelError::WrongObject)?;
    service.handle(stat).map_err(|_| KernelError::WrongObject)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::kernel::boot::Bootstrap;

    fn run_with_kernel_bootstrap_stack(test: impl FnOnce() + Send + 'static) {
        yarm::std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(test)
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    #[test]
    fn init_server_requires_minimum_startup_caps() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        init.set_startup_caps(StartupCapSet {
            endpoint_factory: false,
            memory_object_factory: true,
            irq_control: false,
            clock: false,
        });
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        assert_eq!(
            init.register_core_graph(&mut state, graph),
            Err(KernelError::MissingRight)
        );
    }

    #[test]
    fn init_server_launch_flow_registers_launches_and_enters_running() {
        run_with_kernel_bootstrap_stack(|| {
            let mut state = Bootstrap::init_boxed().expect("init");
            let mut init = InitService::new();
            let graph = CoreServiceGraph {
                init_tid: 1,
                process_manager_tid: 2,
                vfs_tid: 3,
                supervisor_tid: 4,
            };

            init.register_core_graph(&mut state, graph)
                .expect("register");
            assert_eq!(init.phase(), InitBootPhase::CoreServicesRegistered);

            let report = init
                .launch_core_services(
                    &mut state,
                    CoreServiceImagePlan {
                        process_manager_entry: 0x8000,
                        vfs_entry: 0x9000,
                        supervisor_entry: 0xA000,
                    },
                )
                .expect("launch");
            assert!(report.process_manager_spawned);
            assert!(report.vfs_spawned);
            assert!(report.supervisor_spawned);

            let handoff = init.install_fault_handoff(&mut state, 50).expect("handoff");
            assert_eq!(handoff.supervisor_tid, 4);
            init.begin_running(&state).expect("running");
            assert_eq!(init.phase(), InitBootPhase::Running);
        });
    }

    #[test]
    fn launch_order_is_deterministic() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        let _ = init
            .launch_core_services(
                &mut state,
                CoreServiceImagePlan {
                    process_manager_entry: 0x8000,
                    vfs_entry: 0x9000,
                    supervisor_entry: 0xA000,
                },
            )
            .expect("launch");
        assert_eq!(
            init.launch_order(),
            [
                Some(CoreServiceKind::ProcessManager),
                Some(CoreServiceKind::Vfs),
                Some(CoreServiceKind::Supervisor),
            ]
        );
    }

    #[test]
    fn launch_order_can_prioritize_supervisor() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        init.set_launch_strategy(CoreLaunchStrategy::SupervisorFirst);
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        let _ = init
            .launch_core_services(
                &mut state,
                CoreServiceImagePlan {
                    process_manager_entry: 0x8000,
                    vfs_entry: 0x9000,
                    supervisor_entry: 0xA000,
                },
            )
            .expect("launch");
        assert_eq!(
            init.launch_order(),
            [
                Some(CoreServiceKind::Supervisor),
                Some(CoreServiceKind::ProcessManager),
                Some(CoreServiceKind::Vfs),
            ]
        );
    }

    #[test]
    fn launch_sets_mount_status() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        assert!(init.mount_status().is_some());
    }

    #[test]
    fn begin_running_requires_fault_handoff() {
        let mut init = InitService::new();
        init.phase = InitBootPhase::LaunchingCore;
        init.mount_status = Some(MountRecoveryReport {
            mounted_count: init.mount_plan.count,
            recovered_with_fat: false,
        });

        assert_eq!(
            init.validate_begin_running_preconditions(),
            Err(KernelError::WrongObject)
        );
    }

    #[test]
    fn rejects_invalid_restart_policies() {
        let mut init = InitService::new();
        let bad = CoreServicePolicyTable {
            process_manager: ServiceRestartPolicy {
                max_restarts: 0,
                backoff_ticks: 1,
            },
            vfs: ServiceRestartPolicy {
                max_restarts: 1,
                backoff_ticks: 1,
            },
            supervisor: ServiceRestartPolicy {
                max_restarts: 1,
                backoff_ticks: 1,
            },
        };
        assert_eq!(
            init.set_restart_policies(bad),
            Err(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn init_server_supports_failure_transition() {
        let mut init = InitService::new();
        init.mark_failed();
        assert_eq!(init.phase(), InitBootPhase::Failed);
    }

    #[test]
    fn mount_plan_supports_ext4_to_fat_recovery() {
        let init = InitService::new();
        let report = init
            .execute_mount_plan_with_fail_at(Some(3))
            .expect("mount recovery");
        assert!(report.recovered_with_fat);
        assert!(report.mounted_count >= 4);
    }

    #[test]
    fn supervisor_handoff_binds_kernel_endpoint() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        let token = state.exit_task(2, 5).expect("exit");
        let msg = state
            .try_ipc_recv(handoff.supervisor_fault_recv_cap)
            .expect("recv")
            .expect("msg");
        let event = TaskExitedEvent::decode(msg.as_slice()).expect("event");
        assert_eq!(event.tid, 2);
        assert_eq!(event.restart_token, token);
    }

    #[test]
    fn init_recovers_supervisor_failure_within_budget() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let _handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        init.begin_running(&state).expect("running");
        let token = state.exit_task(4, 99).expect("exit");
        assert!(
            init.recover_supervisor_failure(&mut state, token)
                .expect("recover")
        );
        assert_eq!(state.task_status(4).map(map_task_status), Some(TaskStatus::Runnable));
    }

    #[test]
    fn init_recovers_proc_mgr_failure_within_budget() {
        yarm::std::thread::Builder::new()
            .name("init_recovers_proc_mgr_failure_within_budget".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(run_init_recovers_proc_mgr_failure_within_budget)
            .expect("spawn test thread")
            .join()
            .expect("join test thread");
    }

    fn run_init_recovers_proc_mgr_failure_within_budget() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let _handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        init.begin_running(&state).expect("running");
        let token = state.exit_task(2, 44).expect("exit");
        assert!(
            init.recover_core_service_failure(&mut state, CoreServiceKind::ProcessManager, token)
                .expect("recover")
        );
        assert_eq!(state.task_status(2).map(map_task_status), Some(TaskStatus::Runnable));
    }

    #[test]
    fn init_recovers_vfs_failure_with_generic_core_helper() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        init.begin_running(&state).expect("running");
        let _ = handoff;
        let token = state.exit_task(3, 12).expect("exit");
        assert!(
            init.recover_core_service_failure(&mut state, CoreServiceKind::Vfs, token)
                .expect("recover")
        );
        assert_eq!(state.task_status(3).map(map_task_status), Some(TaskStatus::Runnable));
    }

    #[test]
    fn recovering_supervisor_reseeds_control_plane_requests() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        let token = state.exit_task(4, 99).expect("exit");
        init.recover_supervisor_failure(&mut state, token)
            .expect("recover");

        let first = state
            .try_ipc_recv(handoff.supervisor_control_recv_cap)
            .expect("recv")
            .expect("first");
        let second = state
            .try_ipc_recv(handoff.supervisor_control_recv_cap)
            .expect("recv")
            .expect("second");
        let third = state
            .try_ipc_recv(handoff.supervisor_control_recv_cap)
            .expect("recv")
            .expect("third");
        let fourth = state
            .try_ipc_recv(handoff.supervisor_control_recv_cap)
            .expect("recv");
        assert_eq!(first.opcode, SUPERVISOR_OP_REGISTER_CORE_SERVICE);
        assert_eq!(second.opcode, SUPERVISOR_OP_REGISTER_CORE_SERVICE);
        assert_eq!(third.opcode, SUPERVISOR_OP_REGISTER_CORE_SERVICE);
        assert!(fourth.is_none());
    }

    #[test]
    fn recovering_supervisor_replays_driver_registrations_too() {
        let mut state = Bootstrap::init_boxed().expect("init");
        let mut init = InitService::new();
        let graph = CoreServiceGraph {
            init_tid: 1,
            process_manager_tid: 2,
            vfs_tid: 3,
            supervisor_tid: 4,
        };
        init.register_core_graph(&mut state, graph)
            .expect("register");
        init.launch_core_services(
            &mut state,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
        let handoff = init
            .install_fault_handoff(&mut state, 100)
            .expect("handoff");
        init.seed_supervisor_registrations(&mut state)
            .expect("seed");
        state
            .register_task_with_class(20, to_kernel_task_class(TaskClass::Driver))
            .expect("task");
        state.register_driver(20).expect("driver");
        let (_id, mem) = state.alloc_anonymous_memory_object().expect("mem");
        let iova = state.create_iova_space_cap().expect("iova");
        init.register_driver_with_supervisor(
            &mut state,
            RegisterDriverRequest {
                tid: 20,
                max_restarts: 2,
                restart_group: 2,
                dependency_mask: 0,
                backoff_ticks: 3,
                irq_line: 5,
                mem_cap: mem.0,
                iova_cap: iova.0,
                iova_base: 0x4000,
                dma_len: PAGE_SIZE as u64,
                iova_len: PAGE_SIZE as u64,
            },
        )
        .expect("driver register");
        let token = state.exit_task(4, 99).expect("exit");
        init.recover_supervisor_failure(&mut state, token)
            .expect("recover");

        let mut register_driver_seen = false;
        for _ in 0..4 {
            let msg = state
                .try_ipc_recv(handoff.supervisor_control_recv_cap)
                .expect("recv")
                .expect("msg");
            if msg.opcode == SUPERVISOR_OP_REGISTER_DRIVER {
                register_driver_seen = true;
            }
        }
        assert!(register_driver_seen);
    }

    #[test]
    fn mount_plan_without_fallback_fails() {
        let mut init = InitService::new();
        let mut plan = MountPlan::baseline();
        plan.allow_fallback_to_fat = false;
        init.set_mount_plan(plan).expect("set plan");
        assert_eq!(
            init.execute_mount_plan_with_fail_at(Some(3)),
            Err(KernelError::WrongObject)
        );
    }
}
