use super::capabilities::{CapId, CapObject, CapRights, Capability, CapabilitySpace};
use super::ipc::{Endpoint, EndpointMode, Message};
use super::scheduler::{CpuId, SmpScheduler};
use super::smp::{CrossCpuWorkQueue, WorkItem, MAX_CROSS_CPU_WORK};
use super::syscall::{dispatch as dispatch_syscall, SyscallError};
use super::task::{
    RestartState, RestartToken, RobustFutexState, TaskClass, TaskStatus, ThreadControlBlock,
    ThreadDetachState, TickDuration, TickInstant, UserRegisterContext, WaitReason,
};
use super::timer::Timer;
use super::trap::{FaultAccess, FaultInfo, Trap, TrapEvent};
use super::trapframe::TrapFrame;
use super::vm::{
    AddressSpace, AddressSpaceManager, Asid, Mapping, PageFlags, PhysAddr, VirtAddr, VmError,
};
use crate::arch::platform_layout;
use crate::kernel::ipc::ThreadId;

const MAX_ENDPOINTS: usize = 16;
const MAX_TASKS: usize = 64;
const MAX_TASK_MEM_ENTRIES: usize = 2048;
const MAX_MEMORY_OBJECTS: usize = 128;
const MAX_NOTIFICATIONS: usize = 16;
const MAX_IRQ_LINES: usize = platform_layout::MAX_IRQ_LINES;
const MAX_DRIVERS: usize = 32;
const RESTART_ESCALATION_THRESHOLD: u32 = 3;
const INITIAL_DYNAMIC_TID: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    VmFull,
    SchedulerFull,
    CapabilityFull,
    EndpointFull,
    InvalidCapability,
    MissingRight,
    WrongObject,
    StaleCapability,
    EndpointQueueFull,
    TaskTableFull,
    TaskMissing,
    MemoryObjectFull,
    MemoryObjectMissing,
    Vm(VmError),
    UserMemoryFault,
    WouldBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapHandleError {
    MissingTrapFrame,
    Syscall(SyscallError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPolicy {
    KillTask,
    NotifyAndContinue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassPolicySnapshot {
    pub class: TaskClass,
    pub restart_budget: u8,
    pub restart_backoff_ticks: u64,
    pub escalation_threshold: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserImageSpec {
    pub tid: u64,
    pub entry: usize,
    pub asid: Option<Asid>,
    pub class: TaskClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedUserTask {
    pub tid: u64,
    pub entry: usize,
    pub asid: Option<Asid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRole {
    Init,
    ProcessManager,
    Vfs,
    Driver,
    Supervisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServicePolicyEntry {
    tid: ThreadId,
    role: ServiceRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceServerDelegation {
    pub server_tid: ThreadId,
    pub irq_line: u16,
    pub mem_cap: CapId,
    pub dma_offset: usize,
    pub dma_len: usize,
    pub iova_cap: CapId,
    pub iova_base: usize,
    pub iova_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDelegationBundle {
    pub irq_cap: CapId,
    pub dma_cap: CapId,
    pub iova_cap: CapId,
}

const ALLOWED_SERVICE_DELEGATION_EDGES: &[(ServiceRole, ServiceRole)] = &[
    (ServiceRole::Init, ServiceRole::ProcessManager),
    (ServiceRole::Init, ServiceRole::Vfs),
    (ServiceRole::Init, ServiceRole::Driver),
    (ServiceRole::Init, ServiceRole::Supervisor),
    (ServiceRole::Supervisor, ServiceRole::Driver),
    (ServiceRole::Supervisor, ServiceRole::Vfs),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverBundlePlan {
    pub server_tid: ThreadId,
    pub irq_line: u16,
    pub mem_cap: CapId,
    pub iova_cap: CapId,
    pub iova_base: usize,
    pub iova_len: usize,
}

impl DriverBundlePlan {
    pub const fn standard(
        server_tid: ThreadId,
        irq_line: u16,
        mem_cap: CapId,
        iova_cap: CapId,
        iova_base: usize,
    ) -> Self {
        Self {
            server_tid,
            irq_line,
            mem_cap,
            iova_cap,
            iova_base,
            iova_len: super::vm::PAGE_SIZE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcFastpathResult {
    pub switched_to_waiter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IpcPathTelemetry {
    pub fastpath_attempts: u64,
    pub fastpath_switches: u64,
    pub queued_sends: u64,
    pub blocked_sends: u64,
    pub rendezvous_handoffs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartTelemetry {
    pub budget_remaining: u8,
    pub backoff_ticks: u64,
    pub available_at_tick: u64,
    pub token_outstanding: bool,
    pub denied_count: u32,
    pub escalation_count: u32,
    pub last_exit_code: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskMemByte {
    tid: ThreadId,
    addr: usize,
    value: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryObject {
    id: u64,
    phys: PhysAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NotificationObject {
    endpoint_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RestartPolicy {
    budget: u8,
    backoff_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DriverRecord {
    tid: ThreadId,
    irq_cap: Option<CapId>,
    dma_cap: Option<CapId>,
    dma_iova_base: Option<usize>,
    dma_iova_len: Option<usize>,
    iova_space_cap: Option<CapId>,
}

#[derive(Debug)]
struct IpcSubsystem {
    cross_cpu_work: CrossCpuWorkQueue,
    endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
    endpoint_waiters: [Option<ThreadId>; MAX_ENDPOINTS],
    endpoint_sender_waiters: [Option<(ThreadId, Message, bool)>; MAX_ENDPOINTS],
    endpoint_generations: [u64; MAX_ENDPOINTS],
    notifications: [Option<NotificationObject>; MAX_NOTIFICATIONS],
    notification_generations: [u64; MAX_NOTIFICATIONS],
    irq_routes: [Option<usize>; MAX_IRQ_LINES],
    telemetry: IpcPathTelemetry,
}

#[derive(Debug)]
struct MemorySubsystem {
    task_mem: [Option<TaskMemByte>; MAX_TASK_MEM_ENTRIES],
    memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],
    next_memory_object_id: u64,
    next_anon_phys: u64,
}

#[derive(Debug)]
struct DriverSubsystem {
    driver_records: [Option<DriverRecord>; MAX_DRIVERS],
    next_iova_space_id: u64,
    service_policy: [Option<ServicePolicyEntry>; MAX_TASKS],
}

#[derive(Debug)]
struct FaultSubsystem {
    last_fault: Option<FaultInfo>,
    fault_handler_endpoint: Option<usize>,
    supervisor_endpoint: Option<usize>,
    fault_policy: FaultPolicy,
}

#[derive(Debug)]
struct RestartSubsystem {
    next_restart_token: u64,
    app_restart_policy: RestartPolicy,
    driver_restart_policy: RestartPolicy,
    system_restart_policy: RestartPolicy,
    app_escalation_threshold: u32,
    driver_escalation_threshold: u32,
    system_escalation_threshold: u32,
}

#[derive(Debug)]
pub struct KernelState {
    pub kernel_aspace: AddressSpace,
    pub scheduler: SmpScheduler,
    pub cspace: CapabilitySpace,
    pub timer: Timer,
    pub user_spaces: AddressSpaceManager,
    ipc: IpcSubsystem,
    next_dynamic_tid: u64,
    tcbs: [Option<ThreadControlBlock>; MAX_TASKS],
    memory: MemorySubsystem,
    drivers: DriverSubsystem,
    tlb_shootdown_count: u64,
    faults: FaultSubsystem,
    restart: RestartSubsystem,
}

pub struct Bootstrap;

impl Bootstrap {
    pub fn init() -> Result<KernelState, KernelError> {
        let mut kernel_aspace = AddressSpace::new_kernel();
        kernel_aspace
            .map_page(
                VirtAddr(platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE),
                Mapping {
                    phys: PhysAddr(platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE),
                    flags: PageFlags::KERNEL_RW,
                },
            )
            .map_err(|err| match err {
                VmError::Full => KernelError::VmFull,
                other => KernelError::Vm(other),
            })?;

        let mut scheduler = SmpScheduler::default();
        scheduler
            .enqueue_on(CpuId(platform_layout::BOOTSTRAP_CPU_ID), 0)
            .map_err(|_| KernelError::SchedulerFull)?;

        let mut cspace = CapabilitySpace::default();
        cspace
            .mint(Capability::new(
                "root_scheduler",
                CapObject::Kernel,
                &[CapRights::Schedule],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        let mut state = KernelState {
            kernel_aspace,
            scheduler,
            cspace,
            timer: Timer::new(platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS),
            user_spaces: AddressSpaceManager::default(),
            ipc: IpcSubsystem {
                cross_cpu_work: CrossCpuWorkQueue::default(),
                endpoints: [const { None }; MAX_ENDPOINTS],
                endpoint_waiters: [None; MAX_ENDPOINTS],
                endpoint_sender_waiters: [None; MAX_ENDPOINTS],
                endpoint_generations: [0; MAX_ENDPOINTS],
                notifications: [const { None }; MAX_NOTIFICATIONS],
                notification_generations: [0; MAX_NOTIFICATIONS],
                irq_routes: [None; MAX_IRQ_LINES],
                telemetry: IpcPathTelemetry::default(),
            },
            next_dynamic_tid: INITIAL_DYNAMIC_TID,
            tcbs: [None; MAX_TASKS],
            memory: MemorySubsystem {
                task_mem: [None; MAX_TASK_MEM_ENTRIES],
                memory_objects: [None; MAX_MEMORY_OBJECTS],
                next_memory_object_id: 1,
                next_anon_phys: platform_layout::NEXT_ANON_PHYS_BASE,
            },
            drivers: DriverSubsystem {
                driver_records: [const { None }; MAX_DRIVERS],
                next_iova_space_id: 1,
                service_policy: [const { None }; MAX_TASKS],
            },
            tlb_shootdown_count: 0,
            faults: FaultSubsystem {
                last_fault: None,
                fault_handler_endpoint: None,
                supervisor_endpoint: None,
                fault_policy: FaultPolicy::KillTask,
            },
            restart: RestartSubsystem {
                next_restart_token: 1,
                app_restart_policy: RestartPolicy {
                    budget: 3,
                    backoff_ticks: 10,
                },
                driver_restart_policy: RestartPolicy {
                    budget: 5,
                    backoff_ticks: 20,
                },
                system_restart_policy: RestartPolicy {
                    budget: 8,
                    backoff_ticks: 5,
                },
                app_escalation_threshold: RESTART_ESCALATION_THRESHOLD,
                driver_escalation_threshold: RESTART_ESCALATION_THRESHOLD * 2,
                system_escalation_threshold: RESTART_ESCALATION_THRESHOLD * 3,
            },
        };

        state.register_task(0)?;
        state.dispatch_next_task()?;
        Ok(state)
    }
}

impl KernelState {
    fn switch_to_runnable_tid(&mut self, tid: ThreadId) -> Result<bool, KernelError> {
        let mut spins = 0usize;
        while spins < MAX_TASKS {
            if self.scheduler.current_tid() == Some(tid.0) {
                return Ok(true);
            }
            self.yield_current()?;
            spins += 1;
        }
        Ok(self.scheduler.current_tid() == Some(tid.0))
    }

    fn tcb_mut(&mut self, tid: u64) -> Option<&mut ThreadControlBlock> {
        self.tcbs.iter_mut().flatten().find(|tcb| tcb.tid.0 == tid)
    }

    pub fn task_status(&self, tid: u64) -> Option<TaskStatus> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.status)
    }

    pub fn last_fault(&self) -> Option<FaultInfo> {
        self.faults.last_fault
    }

    pub fn clear_last_fault(&mut self) {
        self.faults.last_fault = None;
    }

    pub fn record_fault(&mut self, fault: FaultInfo) {
        self.faults.last_fault = Some(fault);
    }

    pub fn set_fault_handler(&mut self, recv_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Receive) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.faults.fault_handler_endpoint = Some(endpoint_idx);
        Ok(())
    }

    pub fn set_fault_policy(&mut self, policy: FaultPolicy) {
        self.faults.fault_policy = policy;
    }

    pub fn fault_policy(&self) -> FaultPolicy {
        self.faults.fault_policy
    }

    pub fn set_task_fault_policy(
        &mut self,
        tid: u64,
        policy: Option<FaultPolicy>,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.fault_policy_override = policy;
        Ok(())
    }

    fn effective_fault_policy_for(&self, tid: u64) -> FaultPolicy {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.fault_policy_override)
            .unwrap_or(self.faults.fault_policy)
    }

    pub fn task_asid(&self, tid: u64) -> Option<Asid> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.asid)
    }

    pub fn set_supervisor_endpoint(&mut self, recv_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Receive) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.faults.supervisor_endpoint = Some(endpoint_idx);
        Ok(())
    }

    pub fn register_driver(&mut self, tid: u64) -> Result<(), KernelError> {
        let _ = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .any(|record| record.tid == ThreadId(tid))
        {
            return Ok(());
        }

        if let Some(slot) = self
            .drivers
            .driver_records
            .iter_mut()
            .find(|slot| slot.is_none())
        {
            *slot = Some(DriverRecord {
                tid: ThreadId(tid),
                irq_cap: None,
                dma_cap: None,
                dma_iova_base: None,
                dma_iova_len: None,
                iova_space_cap: None,
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn grant_driver_irq(&mut self, tid: u64, irq_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(irq_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::Irq { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }
        if !capability.has_right(CapRights::Signal) {
            return Err(KernelError::MissingRight);
        }
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.irq_cap = Some(irq_cap);
        Ok(())
    }

    pub fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelError> {
        self.cspace
            .mint(Capability::new(
                "irq",
                CapObject::Irq { line },
                &[CapRights::Signal],
            ))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn create_iova_space_cap(&mut self) -> Result<CapId, KernelError> {
        let id = self.drivers.next_iova_space_id;
        self.drivers.next_iova_space_id =
            self.drivers.next_iova_space_id.checked_add(1).unwrap_or(1);
        self.cspace
            .mint(Capability::new(
                "iova_space",
                CapObject::IovaSpace { id },
                &[CapRights::Map],
            ))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn grant_driver_iova_space(
        &mut self,
        tid: u64,
        iova_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(iova_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::IovaSpace { .. } => {}
            _ => return Err(KernelError::WrongObject),
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.iova_space_cap = Some(iova_cap);
        Ok(())
    }

    pub fn mint_dma_region_cap(
        &mut self,
        mem_cap: CapId,
        offset: usize,
        len: usize,
    ) -> Result<CapId, KernelError> {
        let capability = self
            .cspace
            .get(mem_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let id = match capability.object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::Map)
            || !capability.has_right(CapRights::Read)
            || !capability.has_right(CapRights::Write)
        {
            return Err(KernelError::MissingRight);
        }

        if !offset.is_multiple_of(crate::kernel::vm::PAGE_SIZE)
            || !len.is_multiple_of(crate::kernel::vm::PAGE_SIZE)
            || len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        if offset
            .checked_add(len)
            .ok_or(KernelError::Vm(VmError::Misaligned))?
            > crate::kernel::vm::PAGE_SIZE
        {
            return Err(KernelError::WrongObject);
        }

        self.cspace
            .mint(Capability::new(
                "dma_region",
                CapObject::DmaRegion { id, offset, len },
                &[CapRights::Map, CapRights::Read, CapRights::Write],
            ))
            .map_err(|_| KernelError::CapabilityFull)
    }

    pub fn grant_driver_dma(&mut self, tid: u64, dma_cap: CapId) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(dma_cap)
            .ok_or(KernelError::InvalidCapability)?;
        match capability.object {
            CapObject::DmaRegion { len, .. } if len > 0 => {}
            CapObject::DmaRegion { .. } => return Err(KernelError::WrongObject),
            _ => return Err(KernelError::WrongObject),
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.dma_cap = Some(dma_cap);
        Ok(())
    }

    pub fn delegate_device_server_caps(
        &mut self,
        plan: DeviceServerDelegation,
    ) -> Result<(CapId, CapId), KernelError> {
        self.register_driver(plan.server_tid.0)?;

        let irq_cap = self.mint_irq_cap(plan.irq_line)?;
        self.grant_driver_irq(plan.server_tid.0, irq_cap)?;

        let dma_cap = self.mint_dma_region_cap(plan.mem_cap, plan.dma_offset, plan.dma_len)?;
        self.grant_driver_dma(plan.server_tid.0, dma_cap)?;

        self.grant_driver_iova_space(plan.server_tid.0, plan.iova_cap)?;
        self.configure_driver_dma_window(plan.server_tid.0, plan.iova_base, plan.iova_len)?;

        Ok((irq_cap, dma_cap))
    }

    pub fn delegate_driver_bundle(
        &mut self,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        let (irq_cap, dma_cap) = self.delegate_device_server_caps(DeviceServerDelegation {
            server_tid: plan.server_tid,
            irq_line: plan.irq_line,
            mem_cap: plan.mem_cap,
            dma_offset: 0,
            dma_len: super::vm::PAGE_SIZE,
            iova_cap: plan.iova_cap,
            iova_base: plan.iova_base,
            iova_len: plan.iova_len,
        })?;
        Ok(DriverDelegationBundle {
            irq_cap,
            dma_cap,
            iova_cap: plan.iova_cap,
        })
    }

    pub fn ipc_path_telemetry(&self) -> IpcPathTelemetry {
        self.ipc.telemetry
    }
    pub fn register_service_role(
        &mut self,
        tid: u64,
        role: ServiceRole,
    ) -> Result<(), KernelError> {
        if self.tcb_mut(tid).is_none() {
            return Err(KernelError::TaskMissing);
        }
        let tid = ThreadId(tid);
        if let Some(entry) = self
            .drivers
            .service_policy
            .iter_mut()
            .flatten()
            .find(|entry| entry.tid == tid)
        {
            entry.role = role;
            return Ok(());
        }
        let slot = self
            .drivers
            .service_policy
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(KernelError::TaskTableFull)?;
        *slot = Some(ServicePolicyEntry { tid, role });
        Ok(())
    }

    fn service_role(&self, tid: ThreadId) -> Option<ServiceRole> {
        self.drivers
            .service_policy
            .iter()
            .flatten()
            .find(|entry| entry.tid == tid)
            .map(|entry| entry.role)
    }

    pub const fn allowed_service_delegation_edges() -> &'static [(ServiceRole, ServiceRole)] {
        ALLOWED_SERVICE_DELEGATION_EDGES
    }

    fn can_delegate_service(
        &self,
        delegator_role: ServiceRole,
        receiver_role: ServiceRole,
    ) -> bool {
        Self::allowed_service_delegation_edges()
            .iter()
            .any(|edge| *edge == (delegator_role, receiver_role))
    }

    pub fn validate_service_delegation(
        &self,
        delegator_tid: u64,
        receiver_tid: u64,
    ) -> Result<(), KernelError> {
        let Some(delegator_role) = self.service_role(ThreadId(delegator_tid)) else {
            return Err(KernelError::MissingRight);
        };
        let Some(receiver_role) = self.service_role(ThreadId(receiver_tid)) else {
            return Err(KernelError::WrongObject);
        };
        if self.can_delegate_service(delegator_role, receiver_role) {
            Ok(())
        } else {
            Err(KernelError::MissingRight)
        }
    }

    pub fn delegate_driver_bundle_checked(
        &mut self,
        delegator_tid: u64,
        plan: DriverBundlePlan,
    ) -> Result<DriverDelegationBundle, KernelError> {
        self.validate_service_delegation(delegator_tid, plan.server_tid.0)?;
        if self.service_role(plan.server_tid) != Some(ServiceRole::Driver) {
            return Err(KernelError::WrongObject);
        }
        self.delegate_driver_bundle(plan)
    }

    pub fn validate_driver_bundle_live(
        &self,
        tid: u64,
        bundle: DriverDelegationBundle,
    ) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        if record.irq_cap != Some(bundle.irq_cap)
            || record.dma_cap != Some(bundle.dma_cap)
            || record.iova_space_cap != Some(bundle.iova_cap)
        {
            return Err(KernelError::StaleCapability);
        }

        if self.cspace.get(bundle.irq_cap).is_none()
            || self.cspace.get(bundle.dma_cap).is_none()
            || self.cspace.get(bundle.iova_cap).is_none()
        {
            return Err(KernelError::StaleCapability);
        }

        Ok(())
    }

    pub fn configure_driver_dma_window(
        &mut self,
        tid: u64,
        iova_base: usize,
        iova_len: usize,
    ) -> Result<(), KernelError> {
        if !iova_base.is_multiple_of(super::vm::PAGE_SIZE)
            || !iova_len.is_multiple_of(super::vm::PAGE_SIZE)
            || iova_len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }

        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.dma_iova_base = Some(iova_base);
        record.dma_iova_len = Some(iova_len);
        Ok(())
    }

    pub fn validate_driver_dma_iova(
        &self,
        tid: u64,
        iova_base: usize,
        iova_len: usize,
    ) -> Result<(), KernelError> {
        if !iova_base.is_multiple_of(super::vm::PAGE_SIZE)
            || !iova_len.is_multiple_of(super::vm::PAGE_SIZE)
            || iova_len == 0
        {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let record = self
            .drivers
            .driver_records
            .iter()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        if record.iova_space_cap.is_none() {
            return Err(KernelError::WrongObject);
        }

        match (record.dma_iova_base, record.dma_iova_len) {
            (Some(base), Some(len)) => {
                let end = iova_base
                    .checked_add(iova_len)
                    .ok_or(KernelError::WrongObject)?;
                let window_end = base.checked_add(len).ok_or(KernelError::WrongObject)?;
                if iova_base < base || end > window_end {
                    return Err(KernelError::WrongObject);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn detach_driver_iova_space(&mut self, tid: u64) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;
        record.iova_space_cap = None;
        record.dma_iova_base = None;
        record.dma_iova_len = None;
        Ok(())
    }

    pub fn revoke_driver_runtime_caps(&mut self, tid: u64) -> Result<(), KernelError> {
        let record = self
            .drivers
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == ThreadId(tid))
            .ok_or(KernelError::TaskMissing)?;

        if let Some(cap) = record.irq_cap.take() {
            let _ = self.cspace.revoke(cap);
        }
        if let Some(cap) = record.dma_cap.take() {
            let _ = self.cspace.revoke(cap);
        }
        if let Some(cap) = record.iova_space_cap.take() {
            let _ = self.cspace.revoke(cap);
        }
        record.dma_iova_base = None;
        record.dma_iova_len = None;
        Ok(())
    }

    pub fn report_task_exit_to_supervisor(
        &mut self,
        tid: u64,
        code: u64,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.faults.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..16].copy_from_slice(&code.to_le_bytes());
        let msg = Message::with_header(0, 0xEE, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    fn report_restart_denial_to_supervisor(
        &mut self,
        tid: u64,
        denied_count: u32,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.faults.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..12].copy_from_slice(&denied_count.to_le_bytes());
        let msg = Message::with_header(0, 0xEF, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;
        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    pub fn set_task_restart_policy(
        &mut self,
        tid: u64,
        budget: u8,
        backoff_ticks: u64,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.restart.budget = budget;
        tcb.restart.backoff = TickDuration(backoff_ticks);
        Ok(())
    }

    pub fn task_restart_telemetry(&self, tid: u64) -> Result<RestartTelemetry, KernelError> {
        let tcb = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .ok_or(KernelError::TaskMissing)?;
        Ok(RestartTelemetry {
            budget_remaining: tcb.restart.budget,
            backoff_ticks: tcb.restart.backoff.0,
            available_at_tick: tcb.restart.available_at.0,
            token_outstanding: tcb.restart.token.is_some(),
            denied_count: tcb.restart.denied_count,
            escalation_count: tcb.restart.escalation_count,
            last_exit_code: tcb.last_exit_code,
        })
    }

    pub fn exit_task(&mut self, tid: u64, code: u64) -> Result<u64, KernelError> {
        let token = self.restart.next_restart_token;
        self.restart.next_restart_token =
            self.restart.next_restart_token.checked_add(1).unwrap_or(1);

        let robust = self.robust_futex_state(tid);
        let detached = self.thread_detach_state(tid) == Some(ThreadDetachState::Detached);
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Exited;
        tcb.restart.token = Some(RestartToken(token));
        tcb.last_exit_code = Some(code);
        self.report_task_exit_to_supervisor(tid, code)?;
        if let Some(robust) = robust {
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake(addr, u32::MAX);
                offset += 1;
            }
        }
        let _ = self.wake_joiners_for(tid)?;

        if self.scheduler.current_tid() == Some(tid) {
            let _ = self.scheduler.block_current();
            let _ = self.dispatch_next_task()?;
        }
        if detached {
            self.reap_if_detached(tid)?;
        }

        Ok(token)
    }

    pub fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelError> {
        let now_tick = TickInstant(self.timer.current_ticks().0);
        let (app_threshold, driver_threshold, system_threshold) = (
            self.restart.app_escalation_threshold,
            self.restart.driver_escalation_threshold,
            self.restart.system_escalation_threshold,
        );

        let mut should_notify = None;
        let err = {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            let mut denied = false;
            let err = if tcb.restart.token != Some(RestartToken(token)) {
                denied = true;
                Some(KernelError::WrongObject)
            } else if tcb.restart.budget == 0 {
                denied = true;
                Some(KernelError::WouldBlock)
            } else if now_tick < tcb.restart.available_at {
                denied = true;
                Some(KernelError::WouldBlock)
            } else {
                None
            };

            if denied {
                tcb.restart.denied_count = tcb.restart.denied_count.saturating_add(1);
                let threshold = match tcb.class {
                    TaskClass::App => app_threshold,
                    TaskClass::Driver => driver_threshold,
                    TaskClass::SystemServer => system_threshold,
                };
                if tcb.restart.denied_count.is_multiple_of(threshold) {
                    tcb.restart.escalation_count = tcb.restart.escalation_count.saturating_add(1);
                    should_notify = Some(tcb.restart.denied_count);
                }
            }
            err
        };

        if let Some(count) = should_notify {
            self.report_restart_denial_to_supervisor(tid, count)?;
        }

        if let Some(err) = err {
            return Err(err);
        }

        let _ = self.revoke_driver_runtime_caps(tid);

        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.restart.budget = tcb.restart.budget.saturating_sub(1);
        tcb.restart.available_at = TickInstant(now_tick.0.saturating_add(tcb.restart.backoff.0));
        tcb.restart.token = None;
        tcb.status = TaskStatus::Runnable;
        self.scheduler
            .enqueue(tid)
            .map_err(|_| KernelError::SchedulerFull)
    }

    pub fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
        }
        let _ = self.revoke_driver_runtime_caps(tid);
        Ok(())
    }

    pub fn bind_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError> {
        if self.user_spaces.get(asid).is_none() {
            return Err(KernelError::Vm(VmError::InvalidAsid));
        }
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.asid = Some(asid);
        Ok(())
    }

    pub fn bring_up_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.scheduler
            .bring_up_cpu(cpu)
            .map_err(|_| KernelError::WrongObject)
    }

    pub fn set_current_cpu(&mut self, cpu: CpuId) -> Result<(), KernelError> {
        self.scheduler
            .set_current_cpu(cpu)
            .map_err(|_| KernelError::WrongObject)
    }

    pub fn online_cpu_count(&self) -> usize {
        self.scheduler.online_cpu_count()
    }

    pub fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<(), KernelError> {
        self.scheduler
            .enqueue_on(cpu, tid)
            .map_err(|_| KernelError::SchedulerFull)
    }

    pub fn submit_cross_cpu_work(&self, item: WorkItem) -> Result<(), KernelError> {
        self.ipc
            .cross_cpu_work
            .submit(item)
            .map_err(|_| KernelError::TaskTableFull)
    }

    pub fn drain_cross_cpu_work(&self) -> Option<WorkItem> {
        self.ipc.cross_cpu_work.take()
    }

    pub fn tlb_shootdown_count(&self) -> u64 {
        self.tlb_shootdown_count
    }

    fn apply_cross_cpu_work(&mut self, item: WorkItem) -> Result<(), KernelError> {
        match item {
            WorkItem::Reschedule { target_cpu } => {
                if self.scheduler.current_cpu() == target_cpu {
                    self.yield_current()?;
                }
                Ok(())
            }
            WorkItem::TlbShootdown { .. } => {
                self.tlb_shootdown_count = self.tlb_shootdown_count.wrapping_add(1);
                Ok(())
            }
            WorkItem::WakeTask { target_cpu, tid } => {
                let tcb = self.tcb_mut(tid.0).ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                self.enqueue_on_cpu(target_cpu, tid.0)
            }
        }
    }

    pub fn process_cross_cpu_work_for_cpu(&mut self, cpu: CpuId) -> Result<usize, KernelError> {
        let mut deferred = [None; MAX_CROSS_CPU_WORK];
        let mut deferred_len = 0usize;
        let mut processed = 0usize;

        while let Some(item) = self.ipc.cross_cpu_work.take() {
            let target_cpu = match item {
                WorkItem::Reschedule { target_cpu }
                | WorkItem::TlbShootdown { target_cpu, .. }
                | WorkItem::WakeTask { target_cpu, .. } => target_cpu,
            };

            if target_cpu == cpu {
                self.apply_cross_cpu_work(item)?;
                processed += 1;
            } else if deferred_len < MAX_CROSS_CPU_WORK {
                deferred[deferred_len] = Some(item);
                deferred_len += 1;
            }
        }

        let mut idx = 0;
        while idx < deferred_len {
            if let Some(item) = deferred[idx] {
                self.ipc
                    .cross_cpu_work
                    .submit(item)
                    .map_err(|_| KernelError::TaskTableFull)?;
            }
            idx += 1;
        }

        Ok(processed)
    }

    pub fn write_user_memory(
        &mut self,
        tid: u64,
        ptr: usize,
        data: &[u8],
    ) -> Result<(), KernelError> {
        let _ = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;

        let mut i = 0;
        while i < data.len() {
            let va = ptr + i;
            self.validate_user_access_for_tid(tid, va, true)?;

            let mut found = false;
            for slot in &mut self.memory.task_mem {
                if slot
                    .as_ref()
                    .is_some_and(|entry| entry.tid == ThreadId(tid) && entry.addr == va)
                {
                    slot.as_mut().expect("checked").value = data[i];
                    found = true;
                    break;
                }
            }

            if !found {
                let slot = self
                    .memory
                    .task_mem
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(KernelError::TaskTableFull)?;
                *slot = Some(TaskMemByte {
                    tid: ThreadId(tid),
                    addr: va,
                    value: data[i],
                });
            }
            i += 1;
        }

        Ok(())
    }

    pub fn read_user_memory(
        &self,
        tid: u64,
        ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        if len > Message::MAX_PAYLOAD {
            return Err(KernelError::UserMemoryFault);
        }

        let mut out = [0u8; Message::MAX_PAYLOAD];
        let mut i = 0;
        while i < len {
            let va = ptr + i;
            self.validate_user_access_for_tid(tid, va, false)?;
            let value = self
                .memory
                .task_mem
                .iter()
                .flatten()
                .find(|entry| entry.tid == ThreadId(tid) && entry.addr == va)
                .map(|entry| entry.value)
                .ok_or(KernelError::UserMemoryFault)?;
            out[i] = value;
            i += 1;
        }

        Ok(out)
    }

    fn validate_user_access_for_tid(
        &self,
        tid: u64,
        va: usize,
        need_write: bool,
    ) -> Result<(), KernelError> {
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let aspace = self
            .user_spaces
            .get(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let page_base = va & !(crate::kernel::vm::PAGE_SIZE - 1);
        let mapping = aspace
            .resolve(VirtAddr(page_base as u64))
            .ok_or(KernelError::UserMemoryFault)?;
        if !mapping.flags.user || !mapping.flags.read || (need_write && !mapping.flags.write) {
            return Err(KernelError::UserMemoryFault);
        }
        Ok(())
    }

    pub fn copy_to_current_user(
        &mut self,
        user_ptr: usize,
        bytes: &[u8],
    ) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        self.write_user_memory(tid, user_ptr, bytes)
    }

    pub fn copy_from_current_user(
        &self,
        user_ptr: usize,
        len: usize,
    ) -> Result<[u8; Message::MAX_PAYLOAD], KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        self.read_user_memory(tid, user_ptr, len)
    }

    pub fn set_class_escalation_threshold(&mut self, class: TaskClass, threshold: u32) {
        let bounded = threshold.max(1);
        match class {
            TaskClass::App => self.restart.app_escalation_threshold = bounded,
            TaskClass::Driver => self.restart.driver_escalation_threshold = bounded,
            TaskClass::SystemServer => self.restart.system_escalation_threshold = bounded,
        }
    }

    fn restart_policy_for_class(&self, class: TaskClass) -> RestartPolicy {
        match class {
            TaskClass::App => self.restart.app_restart_policy,
            TaskClass::Driver => self.restart.driver_restart_policy,
            TaskClass::SystemServer => self.restart.system_restart_policy,
        }
    }

    pub fn class_policy_snapshot(&self, class: TaskClass) -> ClassPolicySnapshot {
        let policy = self.restart_policy_for_class(class);
        let escalation_threshold = match class {
            TaskClass::App => self.restart.app_escalation_threshold,
            TaskClass::Driver => self.restart.driver_escalation_threshold,
            TaskClass::SystemServer => self.restart.system_escalation_threshold,
        };
        ClassPolicySnapshot {
            class,
            restart_budget: policy.budget,
            restart_backoff_ticks: policy.backoff_ticks,
            escalation_threshold,
        }
    }

    pub fn set_class_restart_policy(&mut self, class: TaskClass, budget: u8, backoff_ticks: u64) {
        let policy = RestartPolicy {
            budget,
            backoff_ticks,
        };
        match class {
            TaskClass::App => self.restart.app_restart_policy = policy,
            TaskClass::Driver => self.restart.driver_restart_policy = policy,
            TaskClass::SystemServer => self.restart.system_restart_policy = policy,
        }
    }

    pub fn register_task_with_class(
        &mut self,
        tid: u64,
        class: TaskClass,
    ) -> Result<(), KernelError> {
        if self.task_status(tid).is_some() {
            return Ok(());
        }
        let policy = self.restart_policy_for_class(class);
        if let Some(slot) = self.tcbs.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ThreadControlBlock {
                tid: ThreadId(tid),
                thread_group_id: tid,
                class,
                status: TaskStatus::Runnable,
                asid: None,
                tls_base: None,
                tls_restore_pending: false,
                user_entry: None,
                user_stack_top: None,
                user_context: UserRegisterContext::default(),
                detach_state: ThreadDetachState::Joinable,
                robust_futex: None,
                fault_policy_override: None,
                brk_base: None,
                brk_end: None,
                restart: RestartState {
                    token: None,
                    budget: policy.budget,
                    backoff: TickDuration(policy.backoff_ticks),
                    available_at: TickInstant(0),
                    denied_count: 0,
                    escalation_count: 0,
                },
                last_exit_code: None,
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn register_task(&mut self, tid: u64) -> Result<(), KernelError> {
        self.register_task_with_class(tid, TaskClass::App)
    }

    pub fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
        let mut candidate = self.next_dynamic_tid;
        for _ in 0..MAX_TASKS.saturating_mul(4) {
            self.next_dynamic_tid = self.next_dynamic_tid.saturating_add(1);
            if self.task_status(candidate).is_none() {
                return Ok(candidate);
            }
            candidate = self.next_dynamic_tid;
        }
        Err(KernelError::TaskTableFull)
    }

    pub fn thread_group_id(&self, tid: u64) -> Option<u64> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.thread_group_id)
    }

    pub fn thread_tls_base(&self, tid: u64) -> Option<usize> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.tls_base)
    }
    pub fn process_id(&self, tid: u64) -> Option<u64> {
        self.thread_group_id(tid)
    }

    pub fn is_thread_group_leader(&self, tid: u64) -> bool {
        self.process_id(tid) == Some(tid)
    }

    pub fn thread_user_context(&self, tid: u64) -> Option<UserRegisterContext> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.user_context)
    }

    pub fn set_thread_user_context(
        &mut self,
        tid: u64,
        context: UserRegisterContext,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.user_context = context;
        Ok(())
    }

    pub fn tls_restore_pending(&self, tid: u64) -> Option<bool> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.tls_restore_pending)
    }

    pub fn take_tls_restore_request(&mut self, tid: u64) -> Result<Option<usize>, KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if !tcb.tls_restore_pending {
            return Ok(None);
        }
        tcb.tls_restore_pending = false;
        Ok(tcb.tls_base)
    }

    pub fn mark_thread_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.detach_state = ThreadDetachState::Detached;
        Ok(())
    }

    pub fn thread_detach_state(&self, tid: u64) -> Option<ThreadDetachState> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.detach_state)
    }

    pub fn join_thread(&mut self, tid: u64) -> Result<Option<u64>, KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if tcb.detach_state == ThreadDetachState::Detached {
            return Err(KernelError::WrongObject);
        }
        if tcb.status != TaskStatus::Exited {
            let current_tid = self.scheduler.current_tid();
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                let joiner_pid = self
                    .process_id(joiner_tid)
                    .ok_or(KernelError::TaskMissing)?;
                let target_pid = self.process_id(tid).ok_or(KernelError::TaskMissing)?;
                if joiner_pid != target_pid {
                    return Err(KernelError::WrongObject);
                }
            }
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                let joiner = self.tcb_mut(joiner_tid).ok_or(KernelError::TaskMissing)?;
                joiner.status = TaskStatus::Blocked(WaitReason::Join(tid));
                let _ = self.scheduler.block_current();
                self.dispatch_next_task()?;
            }
            return Ok(None);
        }
        let exit_code = tcb.last_exit_code;
        tcb.status = TaskStatus::Dead;
        Ok(exit_code)
    }

    pub fn set_robust_futex_head(
        &mut self,
        tid: u64,
        head: usize,
        len: usize,
    ) -> Result<(), KernelError> {
        if head == 0 || len == 0 {
            return Err(KernelError::WrongObject);
        }
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.robust_futex = Some(RobustFutexState { head, len });
        Ok(())
    }

    pub fn robust_futex_state(&self, tid: u64) -> Option<RobustFutexState> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.robust_futex)
    }

    fn sync_current_thread_from_frame(&mut self, frame: &TrapFrame) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.user_context = frame.capture_user_context();
        Ok(())
    }

    fn apply_current_thread_to_frame(&mut self, frame: &mut TrapFrame) -> Result<(), KernelError> {
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let context = self
            .thread_user_context(tid)
            .ok_or(KernelError::TaskMissing)?;
        frame.apply_user_context(context);
        Ok(())
    }

    pub fn resume_current_thread_with_frame(
        &mut self,
        frame: &mut TrapFrame,
    ) -> Result<Option<usize>, KernelError> {
        self.apply_current_thread_to_frame(frame)?;
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        self.take_tls_restore_request(tid)
    }

    fn wake_joiners_for(&mut self, target_tid: u64) -> Result<u32, KernelError> {
        let mut woken = 0u32;
        for idx in 0..self.tcbs.len() {
            let Some(tcb) = self.tcbs[idx].as_mut() else {
                continue;
            };
            if tcb.status != TaskStatus::Blocked(WaitReason::Join(target_tid)) {
                continue;
            }
            tcb.status = TaskStatus::Runnable;
            self.scheduler
                .enqueue(tcb.tid.0)
                .map_err(|_| KernelError::SchedulerFull)?;
            woken += 1;
        }
        Ok(woken)
    }

    fn reap_if_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        let detached = self
            .thread_detach_state(tid)
            .ok_or(KernelError::TaskMissing)?
            == ThreadDetachState::Detached;
        if detached {
            self.mark_task_dead(tid)?;
        }
        Ok(())
    }

    pub fn set_thread_tls_base(&mut self, tid: u64, tls_base: usize) -> Result<(), KernelError> {
        if tls_base == 0 {
            return Err(KernelError::WrongObject);
        }
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.tls_base = Some(tls_base);
        tcb.tls_restore_pending = true;
        Ok(())
    }

    pub fn spawn_user_thread(
        &mut self,
        parent_tid: u64,
        tls_base: usize,
        user_stack_top: usize,
        user_entry: usize,
    ) -> Result<u64, KernelError> {
        if tls_base == 0 || user_stack_top == 0 || user_entry == 0 {
            return Err(KernelError::WrongObject);
        }
        let parent = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == parent_tid)
            .copied()
            .ok_or(KernelError::TaskMissing)?;
        let tid = self.allocate_thread_id()?;
        self.register_task_with_class(tid, parent.class)?;
        if let Some(tcb) = self.tcb_mut(tid) {
            tcb.thread_group_id = parent.thread_group_id;
            tcb.asid = parent.asid;
            tcb.tls_base = Some(tls_base);
            tcb.tls_restore_pending = true;
            tcb.user_entry = Some(user_entry);
            tcb.user_stack_top = Some(user_stack_top);
            tcb.user_context = UserRegisterContext {
                instruction_ptr: user_entry,
                stack_ptr: user_stack_top,
                arg0: 0,
                arg1: 0,
            };
            tcb.status = TaskStatus::Runnable;
        }
        self.scheduler
            .enqueue(tid)
            .map_err(|_| KernelError::SchedulerFull)?;
        Ok(tid)
    }

    pub fn futex_wait_current(
        &mut self,
        addr: usize,
        expected: u32,
        observed: u32,
    ) -> Result<bool, KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        if expected != observed {
            return Ok(false);
        }
        let tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(addr));
        let _ = self.scheduler.block_current();
        self.dispatch_next_task()?;
        Ok(true)
    }

    pub fn futex_wake(&mut self, addr: usize, max_wake: u32) -> Result<u32, KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        if max_wake == 0 {
            return Ok(0);
        }
        let mut woken = 0u32;
        for idx in 0..self.tcbs.len() {
            if woken >= max_wake {
                break;
            }
            let Some(tcb) = self.tcbs[idx].as_mut() else {
                continue;
            };
            if tcb.status != TaskStatus::Blocked(WaitReason::Futex(addr)) {
                continue;
            }
            tcb.status = TaskStatus::Runnable;
            self.scheduler
                .enqueue(tcb.tid.0)
                .map_err(|_| KernelError::SchedulerFull)?;
            woken += 1;
        }
        Ok(woken)
    }

    pub fn spawn_user_task_from_image(
        &mut self,
        spec: UserImageSpec,
    ) -> Result<SpawnedUserTask, KernelError> {
        self.register_task_with_class(spec.tid, spec.class)?;
        if let Some(tcb) = self.tcb_mut(spec.tid) {
            tcb.thread_group_id = spec.tid;
            tcb.asid = spec.asid;
            tcb.user_entry = Some(spec.entry);
            tcb.user_context.instruction_ptr = spec.entry;
            tcb.status = TaskStatus::Runnable;
        }
        Ok(SpawnedUserTask {
            tid: spec.tid,
            entry: spec.entry,
            asid: spec.asid,
        })
    }

    fn dispatch_next_task(&mut self) -> Result<Option<u64>, KernelError> {
        let next = self.scheduler.dispatch_next();
        if let Some(tid) = next {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Running;
        }
        Ok(next)
    }

    pub fn yield_current(&mut self) -> Result<(), KernelError> {
        if let Some(tid) = self.scheduler.current_tid() {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
        }

        let next_tid = self.scheduler.on_preempt();
        if let Some(tid) = next_tid {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Running;
        }
        Ok(())
    }

    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let Some(endpoint_idx) = self.faults.fault_handler_endpoint else {
            return;
        };
        let Some(fault) = self.faults.last_fault else {
            return;
        };

        let mut payload = [0u8; 17];
        payload[..8].copy_from_slice(&faulted_tid.to_le_bytes());
        let addr_bytes = fault.addr.0.to_le_bytes();
        payload[8..16].copy_from_slice(&addr_bytes);
        payload[16] = match fault.access {
            FaultAccess::Read => 0,
            FaultAccess::Write => 1,
            FaultAccess::Execute => 2,
        };

        let msg = match Message::new(0, &payload) {
            Ok(msg) => msg,
            Err(_) => return,
        };

        let sent = if let Some(endpoint) = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
        {
            endpoint.send(msg).is_ok()
        } else {
            false
        };

        if sent {
            let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        }
    }

    fn fault_current_task(&mut self) -> Result<(), KernelError> {
        let running_tid = self
            .scheduler
            .current_tid()
            .ok_or(KernelError::TaskMissing)?;
        self.emit_fault_report(running_tid);

        if self.effective_fault_policy_for(running_tid) == FaultPolicy::NotifyAndContinue {
            return Ok(());
        }

        let faulted_tid = self
            .scheduler
            .block_current()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(faulted_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Faulted;
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn block_current_on_receive(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
    ) -> Result<(), KernelError> {
        let blocked_tid = self
            .scheduler
            .block_current()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
        self.ipc.endpoint_waiters[endpoint_idx] = Some(ThreadId(blocked_tid));
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn block_current_on_send(
        &mut self,
        endpoint_idx: usize,
        send_cap: CapId,
        msg: Message,
    ) -> Result<(), KernelError> {
        if self.ipc.endpoint_sender_waiters[endpoint_idx].is_some() {
            return Err(KernelError::EndpointQueueFull);
        }

        let blocked_tid = self
            .scheduler
            .block_current()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(send_cap));
        self.ipc.endpoint_sender_waiters[endpoint_idx] = Some((ThreadId(blocked_tid), msg, true));
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn wake_waiter_for_endpoint(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx].take() {
            let tcb = self.tcb_mut(waiter_tid.0).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
            self.scheduler
                .enqueue(waiter_tid.0)
                .map_err(|_| KernelError::SchedulerFull)?;
        }
        Ok(())
    }

    fn wake_sender_waiter(&mut self, sender_tid: ThreadId) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(sender_tid.0).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Runnable;
        self.scheduler
            .enqueue(sender_tid.0)
            .map_err(|_| KernelError::SchedulerFull)
    }

    fn resolve_endpoint_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Endpoint { index, generation } => {
                if index >= MAX_ENDPOINTS {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.endpoint_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }
            CapObject::Kernel
            | CapObject::AddressSpace { .. }
            | CapObject::IovaSpace { .. }
            | CapObject::MemoryObject { .. }
            | CapObject::DmaRegion { .. }
            | CapObject::Notification { .. }
            | CapObject::Irq { .. } => Err(KernelError::WrongObject),
        }
    }

    pub fn destroy_endpoint(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        if endpoint_idx >= MAX_ENDPOINTS || self.ipc.endpoints[endpoint_idx].is_none() {
            return Err(KernelError::WrongObject);
        }
        self.ipc.endpoints[endpoint_idx] = None;
        if self.faults.fault_handler_endpoint == Some(endpoint_idx) {
            self.faults.fault_handler_endpoint = None;
        }
        self.ipc.endpoint_waiters[endpoint_idx] = None;
        self.ipc.endpoint_sender_waiters[endpoint_idx] = None;
        let mut next_generation = self.ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.endpoint_generations[endpoint_idx] = next_generation;
        Ok(())
    }

    pub fn create_endpoint(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        self.create_endpoint_with_mode(max_depth, EndpointMode::Buffered)
    }

    pub fn create_endpoint_with_mode(
        &mut self,
        max_depth: usize,
        mode: EndpointMode,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let mut slot_index = None;
        for (idx, slot) in self.ipc.endpoints.iter().enumerate() {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let endpoint_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation = self.ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.endpoint_generations[endpoint_idx] = next_generation;
        self.ipc.endpoints[endpoint_idx] =
            Some(Endpoint::new_with_mode(max_depth, mode).map_err(|_| KernelError::WrongObject)?);

        let send_cap = self
            .cspace
            .mint(Capability::new(
                "endpoint_send",
                CapObject::Endpoint {
                    index: endpoint_idx,
                    generation: self.ipc.endpoint_generations[endpoint_idx],
                },
                &[CapRights::Send],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        let recv_cap = self
            .cspace
            .mint(Capability::new(
                "endpoint_receive",
                CapObject::Endpoint {
                    index: endpoint_idx,
                    generation: self.ipc.endpoint_generations[endpoint_idx],
                },
                &[CapRights::Receive],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        Ok((endpoint_idx, send_cap, recv_cap))
    }

    pub fn create_notification(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let (endpoint_idx, notif_send_cap, recv_cap) =
            self.create_endpoint_with_mode(max_depth, EndpointMode::Buffered)?;

        let mut slot_index = None;
        for (idx, slot) in self.ipc.notifications.iter().enumerate() {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let notification_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation =
            self.ipc.notification_generations[notification_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.ipc.notification_generations[notification_idx] = next_generation;
        self.ipc.notifications[notification_idx] = Some(NotificationObject { endpoint_idx });

        let notification_cap = self
            .cspace
            .mint(Capability::new(
                "notification",
                CapObject::Notification {
                    index: notification_idx,
                    generation: self.ipc.notification_generations[notification_idx],
                },
                &[CapRights::Signal],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        // Keep and return endpoint send cap for software-side injection/testing paths.
        let _ = notif_send_cap;
        Ok((notification_idx, notification_cap, recv_cap))
    }

    fn resolve_notification_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Notification { index, generation } => {
                if index >= MAX_NOTIFICATIONS || self.ipc.notifications[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.ipc.notification_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn bind_irq_notification(
        &mut self,
        irq_line: u16,
        notification_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(notification_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Signal) {
            return Err(KernelError::MissingRight);
        }

        let notif_idx = self.resolve_notification_index(capability.object)?;
        let irq_idx = irq_line as usize;
        if irq_idx >= MAX_IRQ_LINES {
            return Err(KernelError::WrongObject);
        }
        self.ipc.irq_routes[irq_idx] = Some(notif_idx);
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notification_idx: usize,
        irq_line: u16,
    ) -> Result<(), KernelError> {
        let notif = self.ipc.notifications[notification_idx].ok_or(KernelError::WrongObject)?;
        let payload = irq_line.to_le_bytes();
        let msg = Message::with_header(0, irq_line, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        if let Some(endpoint) = self.ipc.endpoints[notif.endpoint_idx].as_mut() {
            endpoint
                .send(msg)
                .map_err(|_| KernelError::EndpointQueueFull)?;
            let _ = self.wake_waiter_for_endpoint(notif.endpoint_idx);
            Ok(())
        } else {
            Err(KernelError::WrongObject)
        }
    }

    pub fn route_external_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        let irq_idx = irq_line as usize;
        let Some(notification_idx) = self.ipc.irq_routes.get(irq_idx).copied().flatten() else {
            return Ok(());
        };
        self.signal_notification(notification_idx, irq_line)
    }

    pub fn ipc_send(&mut self, send_cap: CapId, msg: Message) -> Result<(), KernelError> {
        let capability = self
            .cspace
            .get(send_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Send) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        let endpoint_mode = self
            .ipc
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?
            .mode();

        if endpoint_mode == EndpointMode::Synchronous {
            if let Some(waiter_tid) = self.ipc.endpoint_waiters[endpoint_idx] {
                self.ipc.telemetry.fastpath_attempts =
                    self.ipc.telemetry.fastpath_attempts.saturating_add(1);
                let endpoint = self
                    .ipc
                    .endpoints
                    .get_mut(endpoint_idx)
                    .and_then(Option::as_mut)
                    .ok_or(KernelError::WrongObject)?;
                endpoint
                    .send(msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                self.ipc.telemetry.rendezvous_handoffs =
                    self.ipc.telemetry.rendezvous_handoffs.saturating_add(1);
                self.wake_waiter_for_endpoint(endpoint_idx)?;
                if self.switch_to_runnable_tid(waiter_tid)? {
                    self.ipc.telemetry.fastpath_switches =
                        self.ipc.telemetry.fastpath_switches.saturating_add(1);
                }
                return Ok(());
            }

            self.block_current_on_send(endpoint_idx, send_cap, msg)?;
            self.ipc.telemetry.blocked_sends = self.ipc.telemetry.blocked_sends.saturating_add(1);
            return Err(KernelError::WouldBlock);
        }

        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;

        self.ipc.telemetry.queued_sends = self.ipc.telemetry.queued_sends.saturating_add(1);
        self.wake_waiter_for_endpoint(endpoint_idx)?;
        Ok(())
    }

    pub fn ipc_send_fastpath(
        &mut self,
        send_cap: CapId,
        msg: Message,
    ) -> Result<IpcFastpathResult, KernelError> {
        let capability = self
            .cspace
            .get(send_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Send) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        let endpoint_mode = self
            .ipc
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?
            .mode();
        let waiter_tid = self.ipc.endpoint_waiters[endpoint_idx];
        let inline_sync_handoff = endpoint_mode == EndpointMode::Synchronous && waiter_tid.is_some();
        if !inline_sync_handoff {
            self.ipc.telemetry.fastpath_attempts =
                self.ipc.telemetry.fastpath_attempts.saturating_add(1);
        }

        self.ipc_send(send_cap, msg)?;

        let switched = if inline_sync_handoff {
            true
        } else if waiter_tid.is_some() {
            self.switch_to_runnable_tid(waiter_tid.expect("checked is_some"))?
        } else {
            false
        };

        if switched && !inline_sync_handoff {
            self.ipc.telemetry.fastpath_switches =
                self.ipc.telemetry.fastpath_switches.saturating_add(1);
        }

        Ok(IpcFastpathResult {
            switched_to_waiter: switched,
        })
    }

    pub fn ipc_send_with_cap_transfer(
        &mut self,
        send_cap: CapId,
        sender_tid: ThreadId,
        opcode: u16,
        transfer_cap: CapId,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        if self.cspace.get(transfer_cap).is_none() {
            return Err(KernelError::InvalidCapability);
        }
        let msg = Message::with_header(
            sender_tid.0,
            opcode,
            Message::FLAG_CAP_TRANSFER,
            Some(transfer_cap.0),
            payload,
        )
        .map_err(|_| KernelError::WrongObject)?;
        self.ipc_send(send_cap, msg)
    }

    pub fn ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        let capability = self
            .cspace
            .get(recv_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::Receive) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;

        let endpoint = self
            .ipc
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        if let Some(msg) = endpoint.recv() {
            if let Some((sender_tid, pending_msg, sender_blocked)) =
                self.ipc.endpoint_sender_waiters[endpoint_idx].take()
            {
                endpoint
                    .send(pending_msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                if sender_blocked {
                    self.wake_sender_waiter(sender_tid)?;
                }
            }
            return Ok(Some(msg));
        }

        if let Some((sender_tid, pending_msg, sender_blocked)) =
            self.ipc.endpoint_sender_waiters[endpoint_idx].take()
        {
            if sender_blocked {
                self.wake_sender_waiter(sender_tid)?;
            }
            return Ok(Some(pending_msg));
        }

        self.block_current_on_receive(endpoint_idx, recv_cap)?;
        Ok(None)
    }

    pub fn create_user_address_space(&mut self) -> Result<(Asid, CapId), KernelError> {
        let asid = self
            .user_spaces
            .create_user_space()
            .map_err(KernelError::Vm)?;
        let map_cap = self
            .cspace
            .mint(Capability::new(
                "aspace_map",
                CapObject::AddressSpace { asid: asid.0 },
                &[CapRights::Map, CapRights::Read, CapRights::Write],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;
        Ok((asid, map_cap))
    }

    pub fn map_user_page(
        &mut self,
        map_cap: CapId,
        virt: VirtAddr,
        mapping: Mapping,
    ) -> Result<Option<Mapping>, KernelError> {
        let capability = self
            .cspace
            .get(map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::Map) {
            return Err(KernelError::MissingRight);
        }

        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        aspace.map_page(virt, mapping).map_err(KernelError::Vm)
    }

    pub fn create_memory_object(&mut self, phys: PhysAddr) -> Result<(u64, CapId), KernelError> {
        if !phys.0.is_multiple_of(crate::kernel::vm::PAGE_SIZE as u64) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let id = self.memory.next_memory_object_id;
        self.memory.next_memory_object_id = self.memory.next_memory_object_id.wrapping_add(1);

        let slot = self
            .memory
            .memory_objects
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(KernelError::MemoryObjectFull)?;
        *slot = Some(MemoryObject { id, phys });

        let cap = self
            .cspace
            .mint(Capability::new(
                "memobj_rw",
                CapObject::MemoryObject { id },
                &[CapRights::Read, CapRights::Write, CapRights::Map],
            ))
            .map_err(|_| KernelError::CapabilityFull)?;

        Ok((id, cap))
    }

    pub fn alloc_anonymous_memory_object(&mut self) -> Result<(u64, CapId), KernelError> {
        let phys = PhysAddr(self.memory.next_anon_phys);
        self.memory.next_anon_phys = self
            .memory
            .next_anon_phys
            .wrapping_add(crate::kernel::vm::PAGE_SIZE as u64);
        self.create_memory_object(phys)
    }

    pub fn task_brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| Some((tcb.brk_base?.0 as usize, tcb.brk_end?.0 as usize)))
    }

    pub fn set_task_brk_bounds(
        &mut self,
        tid: u64,
        base: usize,
        end: usize,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.brk_base = Some(VirtAddr(base as u64));
        tcb.brk_end = Some(VirtAddr(end as u64));
        Ok(())
    }

    fn resolve_memory_object_phys(
        &self,
        mem_cap: CapId,
        flags: PageFlags,
    ) -> Result<PhysAddr, KernelError> {
        let capability = self
            .cspace
            .get(mem_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let id = match capability.object {
            CapObject::MemoryObject { id } | CapObject::DmaRegion { id, .. } => id,
            _ => return Err(KernelError::WrongObject),
        };

        if flags.read && !capability.has_right(CapRights::Read) {
            return Err(KernelError::MissingRight);
        }
        if flags.write && !capability.has_right(CapRights::Write) {
            return Err(KernelError::MissingRight);
        }

        self.memory
            .memory_objects
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map(|entry| entry.phys)
            .ok_or(KernelError::MemoryObjectMissing)
    }

    pub fn map_user_page_with_caps(
        &mut self,
        aspace_map_cap: CapId,
        mem_cap: CapId,
        virt: VirtAddr,
        flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let phys = self.resolve_memory_object_phys(mem_cap, flags)?;
        self.map_user_page(aspace_map_cap, virt, Mapping { phys, flags })
    }

    pub fn unmap_user_page(
        &mut self,
        aspace_map_cap: CapId,
        virt: VirtAddr,
    ) -> Result<Option<Mapping>, KernelError> {
        let capability = self
            .cspace
            .get(aspace_map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::Map) {
            return Err(KernelError::MissingRight);
        }
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        Ok(aspace.unmap_page(virt))
    }

    pub fn protect_user_page(
        &mut self,
        aspace_map_cap: CapId,
        virt: VirtAddr,
        new_flags: PageFlags,
    ) -> Result<Option<Mapping>, KernelError> {
        let capability = self
            .cspace
            .get(aspace_map_cap)
            .ok_or(KernelError::InvalidCapability)?;
        let asid = match capability.object {
            CapObject::AddressSpace { asid } => Asid(asid),
            _ => return Err(KernelError::WrongObject),
        };
        if !capability.has_right(CapRights::Map) {
            return Err(KernelError::MissingRight);
        }
        let aspace = self
            .user_spaces
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        let current = aspace
            .resolve(virt)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        aspace
            .map_page(
                virt,
                Mapping {
                    phys: current.phys,
                    flags: new_flags,
                },
            )
            .map_err(KernelError::Vm)
    }

    pub fn handle_trap(
        &mut self,
        trap: Trap,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        match trap {
            Trap::Syscall => {
                self.clear_last_fault();
                let trapframe = frame.ok_or(TrapHandleError::MissingTrapFrame)?;
                let _ = self.sync_current_thread_from_frame(trapframe);
                dispatch_syscall(self, trapframe).map_err(TrapHandleError::Syscall)?;
                if trapframe.error == SyscallError::PageFault.code() {
                    self.fault_current_task()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::TimerInterrupt => {
                let (_, should_preempt) = self.timer.tick_and_check();
                if should_preempt {
                    self.yield_current()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            Trap::PageFault | Trap::ExternalInterrupt => Ok(()),
        }
    }

    pub fn handle_selected_arch_trap_entry(
        &mut self,
        cpu: CpuId,
        context: crate::arch::trap_entry::ArchTrapContext,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        crate::arch::trap_entry::handle_trap_entry(self, cpu, context, frame)
    }

    pub fn handle_trap_event(
        &mut self,
        event: TrapEvent,
        frame: Option<&mut TrapFrame>,
    ) -> Result<(), TrapHandleError> {
        if let Some(fault) = event.fault {
            self.record_fault(fault);
        }

        match event.trap {
            Trap::PageFault => self
                .fault_current_task()
                .map_err(SyscallError::from)
                .map_err(TrapHandleError::Syscall),
            Trap::ExternalInterrupt => {
                if let Some(irq) = event.irq {
                    self.route_external_irq(irq)
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                self.handle_trap(Trap::ExternalInterrupt, frame)
            }
            other => self.handle_trap(other, frame),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::ipc::ThreadId;

    #[test]
    fn selected_arch_trap_entry_routes_timer() {
        let mut state = Bootstrap::init().expect("init");
        #[cfg(target_arch = "x86_64")]
        let ctx = crate::arch::trap_entry::ArchTrapContext {
            vector: 0x20,
            error_code: 0,
            fault_addr: 0,
        };
        #[cfg(target_arch = "aarch64")]
        let ctx = crate::arch::trap_entry::ArchTrapContext {
            esr_el1: 0,
            far_el1: 0,
            irq_line: None,
            is_timer_irq: true,
        };
        #[cfg(any(
            target_arch = "riscv64",
            not(any(
                target_arch = "riscv64",
                target_arch = "x86_64",
                target_arch = "aarch64"
            ))
        ))]
        let ctx = crate::arch::trap_entry::ArchTrapContext {
            scause: 1usize << (usize::BITS as usize - 1) | 5,
            stval: 0,
        };

        state
            .handle_selected_arch_trap_entry(CpuId(0), ctx, None)
            .expect("trap");
    }

    #[test]
    fn bootstrap_sets_minimal_kernel_state() {
        let state = Bootstrap::init().expect("bootstrap should fit static limits");
        assert_eq!(state.kernel_aspace.mappings(), 1);
        assert_eq!(state.online_cpu_count(), 1);
        assert_eq!(state.scheduler.current_tid().expect("boot task"), 0);
        assert_eq!(state.task_status(0), Some(TaskStatus::Running));
    }

    #[test]
    fn spawn_user_task_from_image_registers_asid_and_class() {
        let mut state = Bootstrap::init().expect("init");
        let spawned = state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 55,
                entry: 0x8000,
                asid: Some(Asid(9)),
                class: TaskClass::SystemServer,
            })
            .expect("spawn");
        assert_eq!(spawned.tid, 55);
        assert_eq!(spawned.entry, 0x8000);
        assert_eq!(spawned.asid, Some(Asid(9)));
        let tcb = state.tcb_mut(55).expect("tcb");
        assert_eq!(tcb.class, TaskClass::SystemServer);
        assert_eq!(tcb.asid, Some(Asid(9)));
    }

    #[test]
    fn can_bring_up_secondary_cpu_and_schedule_on_it() {
        let mut state = Bootstrap::init().expect("init");
        assert!(state.bring_up_cpu(CpuId(1)).is_ok());
        assert_eq!(state.online_cpu_count(), 2);

        state.register_task(42).expect("task42");
        state.enqueue_on_cpu(CpuId(1), 42).expect("enqueue cpu1");

        state.set_current_cpu(CpuId(1)).expect("switch cpu1");
        assert_eq!(state.scheduler.dispatch_next(), Some(42));
        assert_eq!(state.scheduler.current_tid(), Some(42));
        assert_eq!(state.task_status(42), Some(TaskStatus::Runnable));
    }

    #[test]
    fn cross_cpu_work_queue_round_trip() {
        let state = Bootstrap::init().expect("init");
        state
            .submit_cross_cpu_work(WorkItem::Reschedule {
                target_cpu: CpuId(1),
            })
            .expect("submit");

        assert_eq!(
            state.drain_cross_cpu_work(),
            Some(WorkItem::Reschedule {
                target_cpu: CpuId(1)
            })
        );
        assert_eq!(state.drain_cross_cpu_work(), None);
    }

    #[test]
    fn process_cross_cpu_work_applies_matching_cpu_items_only() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(2).expect("task2");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        state
            .submit_cross_cpu_work(WorkItem::WakeTask {
                target_cpu: CpuId(1),
                tid: ThreadId(2),
            })
            .expect("submit wake");
        state
            .submit_cross_cpu_work(WorkItem::TlbShootdown {
                target_cpu: CpuId(0),
                asid: Asid(1),
            })
            .expect("submit tlb");

        let done = state
            .process_cross_cpu_work_for_cpu(CpuId(0))
            .expect("process cpu0");
        assert_eq!(done, 1);
        assert_eq!(state.tlb_shootdown_count(), 1);

        // WakeTask for cpu1 should still be queued.
        let remaining = state.drain_cross_cpu_work();
        assert_eq!(
            remaining,
            Some(WorkItem::WakeTask {
                target_cpu: CpuId(1),
                tid: ThreadId(2)
            })
        );
    }

    #[test]
    fn capability_checked_ipc_round_trip() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let msg = Message::new(7, b"ping").expect("message");

        state.ipc_send(send_cap, msg).expect("send should pass");
        let received = state
            .ipc_recv(recv_cap)
            .expect("recv should pass")
            .expect("message expected");

        assert_eq!(received.sender_tid.0, 7);
        assert_eq!(received.as_slice(), b"ping");
    }

    #[test]
    fn timer_trap_preempts_and_rotates() {
        let mut state = Bootstrap::init().expect("init");
        state.timer = Timer::new(1);
        state.register_task(1).expect("register task 1");
        state.scheduler.enqueue(1).expect("queue task 1");

        let running_before = state.scheduler.current_tid().expect("running");
        state
            .handle_trap(Trap::TimerInterrupt, None)
            .expect("timer trap should be handled");
        let running_after = state.scheduler.current_tid().expect("running");

        assert_ne!(running_before, running_after);
        assert_eq!(state.task_status(running_after), Some(TaskStatus::Running));
    }

    #[test]
    fn normalized_page_fault_event_faults_current_task() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue task1");

        state
            .handle_trap_event(
                TrapEvent::page_fault(FaultInfo {
                    addr: VirtAddr(0x1200),
                    access: super::super::trap::FaultAccess::Read,
                }),
                None,
            )
            .expect("page fault event handled");

        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.scheduler.current_tid(), Some(1));
        assert_eq!(
            state.last_fault(),
            Some(FaultInfo {
                addr: VirtAddr(0x1200),
                access: super::super::trap::FaultAccess::Read,
            })
        );
    }

    #[test]
    fn recv_on_empty_endpoint_blocks_then_send_wakes() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("register task 1");
        state.scheduler.enqueue(1).expect("queue task 1");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        assert_eq!(state.scheduler.current_tid(), Some(0));
        let first_try = state.ipc_recv(recv_cap).expect("recv call should not fail");
        assert!(first_try.is_none());
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap)))
        );
        assert_eq!(state.scheduler.current_tid(), Some(1));

        let msg = Message::new(1, b"ok").expect("msg");
        state
            .ipc_send(send_cap, msg)
            .expect("send should wake waiter");
        assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
    }

    #[test]
    fn synchronous_send_blocks_until_receiver_arrives() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("register task 1");
        state.scheduler.enqueue(1).expect("queue task 1");
        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("sync endpoint");

        let msg = Message::new(0, b"xy").expect("msg");
        let send_result = state.ipc_send(send_cap, msg);
        assert_eq!(send_result, Err(KernelError::WouldBlock));
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Blocked(WaitReason::EndpointSend(send_cap)))
        );
        assert_eq!(state.scheduler.current_tid(), Some(1));

        let recv = state
            .ipc_recv(recv_cap)
            .expect("recv call")
            .expect("direct handoff message");
        assert_eq!(recv.as_slice(), b"xy");
        assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
    }

    #[test]
    fn stale_endpoint_capability_rejected_after_recreate() {
        let mut state = Bootstrap::init().expect("init");
        let (eid, send_cap, _recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Buffered)
            .expect("endpoint");

        state.destroy_endpoint(eid).expect("destroy");
        let _ = state
            .create_endpoint_with_mode(1, EndpointMode::Buffered)
            .expect("recreate");

        let msg = Message::new(1, b"stale").expect("msg");
        assert_eq!(
            state.ipc_send(send_cap, msg),
            Err(KernelError::StaleCapability)
        );
    }

    #[test]
    fn can_derive_and_revoke_endpoint_capability() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");

        let child = state
            .cspace
            .mint_derived(send_cap, "send_child", &[CapRights::Send])
            .expect("derive");
        let msg = Message::new(9, b"ok").expect("msg");
        assert!(state.ipc_send(child, msg).is_ok());

        assert!(state.cspace.revoke(child));
        let msg2 = Message::new(9, b"no").expect("msg");
        assert_eq!(
            state.ipc_send(child, msg2),
            Err(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn ipc_message_header_and_cap_transfer_metadata_are_preserved() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state.create_memory_object(PhysAddr(0xC000)).expect("mem");

        state
            .ipc_send_with_cap_transfer(send_cap, ThreadId(0), 0x55, mem_cap, b"mt")
            .expect("send transfer");
        let msg = state.ipc_recv(recv_cap).expect("recv").expect("message");

        assert_eq!(msg.opcode, 0x55);
        assert_eq!(
            msg.flags & Message::FLAG_CAP_TRANSFER,
            Message::FLAG_CAP_TRANSFER
        );
        assert_eq!(msg.transferred_cap().map(|cap| cap.0), Some(mem_cap.0));
        assert_eq!(msg.as_slice(), b"mt");
    }

    #[test]
    fn syscall_trap_dispatches_ipc_send_recv() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        let send_payload = usize::from_le_bytes([b'h', b'i', 0, 0, 0, 0, 0, 0]);
        let mut send_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcSend as usize,
            [send_cap.0 as usize, 42, 2, send_payload, 0, 0],
        );

        state
            .handle_trap(Trap::Syscall, Some(&mut send_frame))
            .expect("syscall send");
        assert_eq!(send_frame.error, 0);

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall recv");
        assert_eq!(recv_frame.error, 0);
        assert_eq!(recv_frame.ret0 as u64, 0);
        assert_eq!(recv_frame.ret1, 2);
        assert_eq!(recv_frame.args[3] & 0xFF, b'h' as usize);
    }

    #[test]
    fn user_address_space_mapping_enforces_split_and_alignment() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");

        let ok = state.map_user_page(
            aspace_map_cap,
            VirtAddr(0x1000),
            Mapping {
                phys: PhysAddr(0x2000),
                flags: PageFlags {
                    read: true,
                    write: true,
                    execute: true,
                    user: true,
                },
            },
        );
        assert_eq!(ok, Ok(None));

        let bad_range = state.map_user_page(
            aspace_map_cap,
            VirtAddr(0x8000_0000),
            Mapping {
                phys: PhysAddr(0x3000),
                flags: PageFlags::USER_RX,
            },
        );
        assert_eq!(bad_range, Err(KernelError::Vm(VmError::PrivilegeViolation)));

        let misaligned = state.map_user_page(
            aspace_map_cap,
            VirtAddr(0x1001),
            Mapping {
                phys: PhysAddr(0x4000),
                flags: PageFlags::USER_RX,
            },
        );
        assert_eq!(misaligned, Err(KernelError::Vm(VmError::Misaligned)));
    }

    #[test]
    fn user_address_space_mapping_requires_aspace_map_capability() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        let (_eid, send_cap, _recv_cap) = state.create_endpoint(2).expect("endpoint");

        let wrong_object = state.map_user_page(
            send_cap,
            VirtAddr(0x1000),
            Mapping {
                phys: PhysAddr(0x2000),
                flags: PageFlags::USER_RX,
            },
        );
        assert_eq!(wrong_object, Err(KernelError::WrongObject));

        let read_only_cap = state
            .cspace
            .mint_derived(aspace_map_cap, "aspace_read_only", &[CapRights::Read])
            .expect("derive read-only aspace cap");
        let missing_right = state.map_user_page(
            read_only_cap,
            VirtAddr(0x1000),
            Mapping {
                phys: PhysAddr(0x3000),
                flags: PageFlags::USER_RX,
            },
        );
        assert_eq!(missing_right, Err(KernelError::MissingRight));
    }

    #[test]
    fn memory_object_capability_controls_mapping_and_unmap_protect() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        let (_mem_id, mem_cap) = state
            .create_memory_object(PhysAddr(0x9000))
            .expect("memobj");

        let mapped = state.map_user_page_with_caps(
            aspace_map_cap,
            mem_cap,
            VirtAddr(0x2000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
            },
        );
        assert_eq!(mapped, Ok(None));

        let old = state
            .protect_user_page(aspace_map_cap, VirtAddr(0x2000), PageFlags::USER_RX)
            .expect("protect")
            .expect("old mapping");
        assert_eq!(old.flags.write, true);

        let unmapped = state
            .unmap_user_page(aspace_map_cap, VirtAddr(0x2000))
            .expect("unmap")
            .expect("mapped entry");
        assert_eq!(unmapped.phys, PhysAddr(0x9000));
    }

    #[test]
    fn memory_object_mapping_requires_memory_rights() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        let (_mem_id, mem_cap) = state
            .create_memory_object(PhysAddr(0xA000))
            .expect("memobj");

        let readonly_mem = state
            .cspace
            .mint_derived(mem_cap, "mem_ro", &[CapRights::Read])
            .expect("derive ro");

        let res = state.map_user_page_with_caps(
            aspace_map_cap,
            readonly_mem,
            VirtAddr(0x3000),
            PageFlags {
                read: true,
                write: true,
                execute: false,
                user: true,
            },
        );
        assert_eq!(res, Err(KernelError::MissingRight));
    }

    #[test]
    fn syscall_send_can_copy_from_user_memory_when_task_has_asid() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x5000),
                    flags: PageFlags {
                        read: true,
                        write: true,
                        execute: true,
                        user: true,
                    },
                },
            )
            .expect("map");
        state.write_user_memory(0, 0, b"hi").expect("write");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let mut send_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcSend as usize,
            [send_cap.0 as usize, 0, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut send_frame))
            .expect("send syscall");

        let received = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(received.as_slice(), b"hi");
    }

    #[test]
    fn syscall_send_large_payload_uses_shared_region_descriptor_with_cap_transfer() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        let mut send_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                0x2000,
                Message::MAX_PAYLOAD + 16,
                0,
                0,
                mem_cap.0 as usize,
            ],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut send_frame))
            .expect("send syscall");

        let msg = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert!(msg.transferred_cap().is_some());
        let region =
            crate::kernel::ipc::SharedMemoryRegion::decode(msg.as_slice()).expect("region");
        assert_eq!(region.offset, 0x2000);
        assert_eq!(region.len as usize, Message::MAX_PAYLOAD + 16);
    }

    #[test]
    fn syscall_recv_can_copy_to_user_memory_when_task_has_asid() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");

        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x6000),
                    flags: PageFlags {
                        read: true,
                        write: true,
                        execute: false,
                        user: true,
                    },
                },
            )
            .expect("map rw");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(9, b"ok").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 16, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("recv syscall");

        assert_eq!(recv_frame.error, 0);
        let bytes = state.read_user_memory(0, 16, 2).expect("read back");
        assert_eq!(&bytes[..2], b"ok");
    }

    #[test]
    fn syscall_recv_reports_page_fault_on_unwritable_user_buffer() {
        use super::super::syscall::SyscallError;

        let mut state = Bootstrap::init().expect("init");
        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");

        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x7000),
                    flags: PageFlags::USER_RX,
                },
            )
            .expect("map rx only");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("recv syscall should return fault code, not trap error");

        assert_eq!(recv_frame.error, SyscallError::PageFault.code());
        assert_eq!(
            state.last_fault(),
            Some(super::super::trap::FaultInfo {
                addr: VirtAddr(8),
                access: super::super::trap::FaultAccess::Write,
            })
        );
    }

    #[test]
    fn page_fault_syscall_faults_current_task_and_schedules_next() {
        use super::super::syscall::SyscallError;

        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue task1");

        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x7000),
                    flags: PageFlags::USER_RX,
                },
            )
            .expect("map rx");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall handled");

        assert_eq!(recv_frame.error, SyscallError::PageFault.code());
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.scheduler.current_tid(), Some(1));
    }

    #[test]
    fn set_fault_handler_requires_receive_capability() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        assert_eq!(
            state.set_fault_handler(send_cap),
            Err(KernelError::MissingRight)
        );
        assert!(state.set_fault_handler(recv_cap).is_ok());
    }

    #[test]
    fn page_fault_emits_report_to_fault_handler_endpoint() {
        use super::super::syscall::SyscallError;

        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue task1");

        let (_handler_eid, _handler_send, handler_recv) =
            state.create_endpoint(4).expect("handler endpoint");
        state.set_fault_handler(handler_recv).expect("set handler");

        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x7000),
                    flags: PageFlags::USER_RX,
                },
            )
            .expect("map rx");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall handled");

        assert_eq!(recv_frame.error, SyscallError::PageFault.code());
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.scheduler.current_tid(), Some(1));

        let report = state
            .ipc_recv(handler_recv)
            .expect("handler recv")
            .expect("fault report");
        assert_eq!(report.sender_tid.0, 0);
        assert_eq!(report.as_slice()[16], 1);
    }

    #[test]
    fn fault_policy_defaults_to_kill_task() {
        let state = Bootstrap::init().expect("init");
        assert_eq!(state.fault_policy(), FaultPolicy::KillTask);
    }

    #[test]
    fn page_fault_with_notify_and_continue_keeps_current_task_running() {
        use super::super::syscall::SyscallError;

        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue task1");
        state.set_fault_policy(FaultPolicy::NotifyAndContinue);

        let (_handler_eid, _handler_send, handler_recv) =
            state.create_endpoint(4).expect("handler endpoint");
        state.set_fault_handler(handler_recv).expect("set handler");

        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0x7000),
                    flags: PageFlags::USER_RX,
                },
            )
            .expect("map rx");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall handled");

        assert_eq!(recv_frame.error, SyscallError::PageFault.code());
        assert_eq!(state.task_status(0), Some(TaskStatus::Running));
        assert_eq!(state.scheduler.current_tid(), Some(0));

        let report = state
            .ipc_recv(handler_recv)
            .expect("handler recv")
            .expect("fault report");
        assert_eq!(report.sender_tid.0, 0);
    }

    #[test]
    fn task_fault_policy_override_beats_global_policy() {
        use super::super::syscall::SyscallError;

        let mut state = Bootstrap::init().expect("init");
        state.set_fault_policy(FaultPolicy::NotifyAndContinue);
        state
            .set_task_fault_policy(0, Some(FaultPolicy::KillTask))
            .expect("set override");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue task1");

        let (asid, aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        state
            .map_user_page(
                aspace_map_cap,
                VirtAddr(0x0),
                Mapping {
                    phys: PhysAddr(0xB000),
                    flags: PageFlags::USER_RX,
                },
            )
            .expect("map rx");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        state
            .ipc_send(send_cap, Message::new(4, b"pf").expect("msg"))
            .expect("send");

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 2, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall handled");

        assert_eq!(recv_frame.error, SyscallError::PageFault.code());
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.scheduler.current_tid(), Some(1));
    }

    #[test]
    fn notification_irq_route_delivers_message_to_bound_endpoint() {
        let mut state = Bootstrap::init().expect("init");
        let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
        state.bind_irq_notification(11, notif_cap).expect("bind");

        state
            .handle_trap_event(TrapEvent::external_interrupt(11), None)
            .expect("handle irq");

        let msg = state.ipc_recv(notif_recv_cap).expect("recv").expect("msg");
        assert_eq!(msg.opcode, 11);
        assert_eq!(msg.as_slice()[0], 11);
    }

    #[test]
    fn create_notification_rejects_non_signal_cap_for_irq_binding() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, _send_cap, recv_cap) = state.create_endpoint(2).expect("ep");
        let err = state
            .bind_irq_notification(1, recv_cap)
            .expect_err("must fail");
        assert_eq!(err, KernelError::MissingRight);
    }

    #[test]
    fn delegate_device_server_caps_configures_driver_record() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(34).expect("task");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let iova_cap = state.create_iova_space_cap().expect("iova");

        let plan = DeviceServerDelegation {
            server_tid: ThreadId(34),
            irq_line: 10,
            mem_cap,
            dma_offset: 0,
            dma_len: crate::kernel::vm::PAGE_SIZE,
            iova_cap,
            iova_base: crate::kernel::vm::PAGE_SIZE * 8,
            iova_len: crate::kernel::vm::PAGE_SIZE,
        };

        let (irq_cap, dma_cap) = state.delegate_device_server_caps(plan).expect("delegate");
        assert!(state.cspace.get(irq_cap).is_some());
        assert!(state.cspace.get(dma_cap).is_some());
        assert!(state
            .validate_driver_dma_iova(
                34,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE,
            )
            .is_ok());
    }

    #[test]
    fn ipc_fastpath_telemetry_distinguishes_switch_and_queue_paths() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(60).expect("sender");
        state.register_task(61).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(2, EndpointMode::Synchronous)
            .expect("endpoint");

        state.scheduler.enqueue(61).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x9000, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.scheduler.enqueue(60).expect("enqueue sender");
        let msg = Message::new(60, b"fp").expect("msg");
        let fast = state.ipc_send_fastpath(send_cap, msg).expect("fastpath");
        assert!(fast.switched_to_waiter);

        let (_beid, bsend_cap, _brecv_cap) = state.create_endpoint(2).expect("buffered");
        let queued = Message::new(60, b"q").expect("queued");
        state.ipc_send(bsend_cap, queued).expect("queue send");

        let t = state.ipc_path_telemetry();
        assert_eq!(t.fastpath_attempts, 1);
        assert_eq!(t.fastpath_switches, 1);
        assert_eq!(t.queued_sends, 1);
        assert_eq!(t.blocked_sends, 0);
        assert_eq!(t.rendezvous_handoffs, 1);
    }

    #[test]
    fn synchronous_endpoint_blocked_send_updates_telemetry() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(62).expect("sender");

        let (_eid, send_cap, _recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        let msg = Message::new(62, b"blk").expect("msg");
        assert_eq!(state.ipc_send(send_cap, msg), Err(KernelError::WouldBlock));

        let t = state.ipc_path_telemetry();
        assert_eq!(t.blocked_sends, 1);
        assert_eq!(t.queued_sends, 0);
    }

    #[test]
    fn ipc_fastpath_blocked_path_is_measured_without_switch() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(63).expect("sender");

        let (_eid, send_cap, _recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        let msg = Message::new(63, b"fp-block").expect("msg");
        assert_eq!(
            state.ipc_send_fastpath(send_cap, msg),
            Err(KernelError::WouldBlock)
        );

        let t = state.ipc_path_telemetry();
        assert_eq!(t.fastpath_attempts, 1);
        assert_eq!(t.fastpath_switches, 0);
        assert_eq!(t.blocked_sends, 1);
        assert_eq!(t.queued_sends, 0);
        assert_eq!(t.rendezvous_handoffs, 0);
    }

    #[test]
    fn ipc_fastpath_on_buffered_endpoint_queues_without_switch() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(64).expect("sender");

        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        let msg = Message::new(64, b"fp-queued").expect("msg");
        let result = state.ipc_send_fastpath(send_cap, msg).expect("fastpath");
        assert!(!result.switched_to_waiter);

        let delivered = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(delivered.as_slice(), b"fp-queued");

        let t = state.ipc_path_telemetry();
        assert_eq!(t.fastpath_attempts, 1);
        assert_eq!(t.fastpath_switches, 0);
        assert_eq!(t.queued_sends, 1);
        assert_eq!(t.blocked_sends, 0);
    }

    #[test]
    fn delegate_driver_bundle_uses_standard_window_and_revokes_caps() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(59).expect("task");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let iova_cap = state.create_iova_space_cap().expect("iova");

        let bundle = state
            .delegate_driver_bundle(DriverBundlePlan {
                server_tid: ThreadId(59),
                irq_line: 12,
                mem_cap,
                iova_cap,
                iova_base: crate::kernel::vm::PAGE_SIZE * 2,
                iova_len: crate::kernel::vm::PAGE_SIZE,
            })
            .expect("bundle");

        assert!(state.cspace.get(bundle.irq_cap).is_some());
        assert!(state.cspace.get(bundle.dma_cap).is_some());
        assert!(state.cspace.get(bundle.iova_cap).is_some());

        state.revoke_driver_runtime_caps(59).expect("revoke");
        assert!(state.cspace.get(bundle.irq_cap).is_none());
        assert!(state.cspace.get(bundle.dma_cap).is_none());
        assert!(state.cspace.get(bundle.iova_cap).is_none());
    }

    #[test]
    fn delegation_policy_requires_init_and_driver_roles() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(70).expect("init-task");
        state.register_task(71).expect("driver-task");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let iova_cap = state.create_iova_space_cap().expect("iova");

        state
            .register_service_role(70, ServiceRole::Init)
            .expect("role");
        state
            .register_service_role(71, ServiceRole::Driver)
            .expect("role");

        let ok = state.delegate_driver_bundle_checked(
            70,
            DriverBundlePlan {
                server_tid: ThreadId(71),
                irq_line: 3,
                mem_cap,
                iova_cap,
                iova_base: crate::kernel::vm::PAGE_SIZE,
                iova_len: crate::kernel::vm::PAGE_SIZE,
            },
        );
        assert!(ok.is_ok());

        let bad = state.delegate_driver_bundle_checked(
            71,
            DriverBundlePlan {
                server_tid: ThreadId(71),
                irq_line: 4,
                mem_cap,
                iova_cap,
                iova_base: crate::kernel::vm::PAGE_SIZE * 2,
                iova_len: crate::kernel::vm::PAGE_SIZE,
            },
        );
        assert_eq!(bad, Err(KernelError::MissingRight));
    }

    #[test]
    fn rendezvous_delivery_is_single_copy_and_no_sender_stuck() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(80).expect("sender");
        state.register_task(81).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        state.scheduler.enqueue(81).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x1100, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.scheduler.enqueue(80).expect("enqueue sender");
        state
            .ipc_send(send_cap, Message::new(80, b"rv").expect("msg"))
            .expect("send");

        let delivered = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(delivered.as_slice(), b"rv");
        assert!(state.ipc_recv(recv_cap).expect("recv2").is_none());
        assert_eq!(state.task_status(80), Some(TaskStatus::Runnable));

        let t = state.ipc_path_telemetry();
        assert!(t.rendezvous_handoffs >= 1);
        assert!(t.fastpath_attempts >= t.fastpath_switches);
    }

    #[test]
    fn service_delegation_edges_table_is_auditable_and_frozen() {
        let edges = KernelState::allowed_service_delegation_edges();
        assert!(edges.contains(&(ServiceRole::Init, ServiceRole::Driver)));
        assert!(edges.contains(&(ServiceRole::Supervisor, ServiceRole::Vfs)));
        assert!(!edges.contains(&(ServiceRole::Driver, ServiceRole::ProcessManager)));
        assert_eq!(edges.len(), 6);
    }

    #[test]
    fn service_delegation_graph_allows_only_expected_edges() {
        let mut state = Bootstrap::init().expect("init");
        for tid in 90..=93 {
            state.register_task(tid).expect("task");
        }
        state
            .register_service_role(90, ServiceRole::Init)
            .expect("role init");
        state
            .register_service_role(91, ServiceRole::Supervisor)
            .expect("role sup");
        state
            .register_service_role(92, ServiceRole::Driver)
            .expect("role drv");
        state
            .register_service_role(93, ServiceRole::ProcessManager)
            .expect("role proc");

        assert!(state.validate_service_delegation(90, 93).is_ok());
        assert!(state.validate_service_delegation(91, 92).is_ok());
        assert_eq!(
            state.validate_service_delegation(92, 93),
            Err(KernelError::MissingRight)
        );
    }

    #[test]
    fn ipc_send_fastpath_detects_waiter() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(35).expect("sender");
        state.register_task(36).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        state.scheduler.enqueue(36).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x7000, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.scheduler.enqueue(35).expect("enqueue sender");
        let msg = Message::new(35, b"x").expect("msg");
        let result = state.ipc_send_fastpath(send_cap, msg).expect("fastpath");
        assert!(result.switched_to_waiter);
    }

    #[test]
    fn driver_registration_and_capability_grants_work() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(3).expect("task");
        state.register_driver(3).expect("driver");

        let irq_cap = state.mint_irq_cap(9).expect("irq");
        state.grant_driver_irq(3, irq_cap).expect("grant irq");

        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let dma_cap = state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
            .expect("dma");
        state.grant_driver_dma(3, dma_cap).expect("grant dma");
    }

    #[test]
    fn supervisor_receives_task_exit_report() {
        let mut state = Bootstrap::init().expect("init");
        let (_e, send_cap, recv_cap) = state.create_endpoint(4).expect("endpoint");
        state
            .set_supervisor_endpoint(recv_cap)
            .expect("supervisor ep");
        state
            .report_task_exit_to_supervisor(7, 99)
            .expect("report exit");

        let msg = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(msg.opcode, 0xEE);
        assert_eq!(msg.as_slice().len(), 16);
        assert_eq!(
            state
                .ipc_send(send_cap, Message::new(0, b"ok").expect("m"))
                .is_ok(),
            true
        );
    }

    #[test]
    fn exited_task_can_restart_with_token_and_then_be_marked_dead() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(9).expect("task");
        let token = state.exit_task(9, 12).expect("exit");
        assert_eq!(state.task_status(9), Some(TaskStatus::Exited));

        assert!(state.restart_task(9, token).is_ok());
        assert_eq!(state.task_status(9), Some(TaskStatus::Runnable));

        state.mark_task_dead(9).expect("dead");
        assert_eq!(state.task_status(9), Some(TaskStatus::Dead));
    }

    #[test]
    fn dma_region_cap_enforces_window_constraints() {
        let mut state = Bootstrap::init().expect("init");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        assert!(state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
            .is_ok());
        assert!(state
            .mint_dma_region_cap(mem_cap, 1, crate::kernel::vm::PAGE_SIZE)
            .is_err());
        assert!(state.mint_dma_region_cap(mem_cap, 0, 0).is_err());
        assert!(state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE * 2)
            .is_err());
    }

    #[test]
    fn deterministic_mixed_stress_sequence_is_stable() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        let (_nidx, ncap, nrecv) = state.create_notification(8).expect("notif");
        state.bind_irq_notification(5, ncap).expect("bind irq");

        for i in 1..=10u64 {
            state.register_task(i).expect("task");
            state
                .enqueue_on_cpu(CpuId((i % 2) as u8), i)
                .expect("enqueue");
        }

        for _ in 0..8 {
            state
                .submit_cross_cpu_work(WorkItem::Reschedule {
                    target_cpu: CpuId(1),
                })
                .expect("work");
        }
        state
            .process_cross_cpu_work_for_cpu(CpuId(1))
            .expect("process");

        for _ in 0..5 {
            state
                .handle_trap_event(TrapEvent::external_interrupt(5), None)
                .expect("irq");
        }

        let mut irq_msgs = 0usize;
        while state.ipc_recv(nrecv).expect("recv").is_some() {
            irq_msgs += 1;
            if irq_msgs > 16 {
                break;
            }
        }
        assert_eq!(irq_msgs, 5);
        assert_eq!(state.online_cpu_count(), 2);
    }

    #[test]
    fn restart_telemetry_reports_budget_and_backoff() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(11).expect("task");
        state.set_task_restart_policy(11, 2, 5).expect("policy");

        let token = state.exit_task(11, 1).expect("exit");
        let t0 = state.task_restart_telemetry(11).expect("telemetry");
        assert_eq!(t0.budget_remaining, 2);
        assert!(t0.token_outstanding);

        assert!(state.restart_task(11, token).is_ok());
        let t1 = state.task_restart_telemetry(11).expect("telemetry");
        assert_eq!(t1.budget_remaining, 1);
        assert!(!t1.token_outstanding);
        assert!(t1.available_at_tick >= 5);
    }

    #[test]
    fn restart_denial_escalates_to_supervisor_every_threshold() {
        let mut state = Bootstrap::init().expect("init");
        let (_e, _send, recv_cap) = state.create_endpoint(4).expect("endpoint");
        state.set_supervisor_endpoint(recv_cap).expect("supervisor");

        state.register_task(13).expect("task");
        let _token = state.exit_task(13, 77).expect("exit");

        for _ in 0..3 {
            let _ = state.restart_task(13, 0xDEAD);
        }

        let telemetry = state.task_restart_telemetry(13).expect("telemetry");
        assert_eq!(telemetry.denied_count, 3);
        assert_eq!(telemetry.escalation_count, 1);

        let exit_report = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(exit_report.opcode, 0xEE);

        let denial_report = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(denial_report.opcode, 0xEF);
    }

    #[test]
    fn class_restart_policy_applies_on_registration() {
        let mut state = Bootstrap::init().expect("init");
        state.set_class_restart_policy(TaskClass::Driver, 9, 33);
        state
            .register_task_with_class(21, TaskClass::Driver)
            .expect("register");

        let t = state.task_restart_telemetry(21).expect("telemetry");
        assert_eq!(t.budget_remaining, 9);
        assert_eq!(t.backoff_ticks, 33);
    }

    #[test]
    fn driver_restart_revokes_runtime_caps() {
        let mut state = Bootstrap::init().expect("init");
        state
            .register_task_with_class(22, TaskClass::Driver)
            .expect("task");
        state.register_driver(22).expect("driver");

        let irq_cap = state.mint_irq_cap(3).expect("irq");
        state.grant_driver_irq(22, irq_cap).expect("grant irq");

        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let dma_cap = state
            .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
            .expect("dma");
        state.grant_driver_dma(22, dma_cap).expect("grant dma");

        let iova_cap = state.create_iova_space_cap().expect("iova");
        state
            .grant_driver_iova_space(22, iova_cap)
            .expect("grant iova");
        state
            .configure_driver_dma_window(
                22,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE,
            )
            .expect("window");

        let token = state.exit_task(22, 1).expect("exit");
        state.restart_task(22, token).expect("restart");

        assert!(state
            .validate_driver_dma_iova(
                22,
                crate::kernel::vm::PAGE_SIZE * 8,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err());
    }

    #[test]
    fn escalation_threshold_is_class_specific() {
        let mut state = Bootstrap::init().expect("init");
        let (_e, _send, recv_cap) = state.create_endpoint(8).expect("endpoint");
        state.set_supervisor_endpoint(recv_cap).expect("supervisor");
        state.set_class_escalation_threshold(TaskClass::Driver, 2);

        state
            .register_task_with_class(30, TaskClass::Driver)
            .expect("task");
        let _ = state.exit_task(30, 1).expect("exit");
        let _ = state.restart_task(30, 0xBAD);
        let _ = state.restart_task(30, 0xBAD);

        let t = state.task_restart_telemetry(30).expect("telemetry");
        assert_eq!(t.denied_count, 2);
        assert_eq!(t.escalation_count, 1);
    }

    #[test]
    fn detach_iova_space_revokes_dma_window_validation() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(31).expect("task");
        state.register_driver(31).expect("driver");

        let iova = state.create_iova_space_cap().expect("iova");
        state.grant_driver_iova_space(31, iova).expect("grant");
        state
            .configure_driver_dma_window(
                31,
                crate::kernel::vm::PAGE_SIZE * 2,
                crate::kernel::vm::PAGE_SIZE,
            )
            .expect("window");
        assert!(state
            .validate_driver_dma_iova(
                31,
                crate::kernel::vm::PAGE_SIZE * 2,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok());

        state.detach_driver_iova_space(31).expect("detach");
        assert!(state
            .validate_driver_dma_iova(
                31,
                crate::kernel::vm::PAGE_SIZE * 2,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err());
    }

    #[test]
    fn class_policy_snapshot_reports_driver_settings() {
        let mut state = Bootstrap::init().expect("init");
        state.set_class_restart_policy(TaskClass::Driver, 6, 40);
        state.set_class_escalation_threshold(TaskClass::Driver, 4);

        let snap = state.class_policy_snapshot(TaskClass::Driver);
        assert_eq!(snap.class, TaskClass::Driver);
        assert_eq!(snap.restart_budget, 6);
        assert_eq!(snap.restart_backoff_ticks, 40);
        assert_eq!(snap.escalation_threshold, 4);
    }

    #[test]
    fn revoke_driver_runtime_caps_revokes_from_cspace() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(32).expect("task");
        state.register_driver(32).expect("driver");

        let irq = state.mint_irq_cap(4).expect("irq");
        state.grant_driver_irq(32, irq).expect("grant irq");

        let (_id, mem) = state.alloc_anonymous_memory_object().expect("mem");
        let dma = state
            .mint_dma_region_cap(mem, 0, crate::kernel::vm::PAGE_SIZE)
            .expect("dma");
        state.grant_driver_dma(32, dma).expect("grant dma");

        let iova = state.create_iova_space_cap().expect("iova");
        state.grant_driver_iova_space(32, iova).expect("grant iova");

        state.revoke_driver_runtime_caps(32).expect("revoke");
        assert!(state.cspace.get(irq).is_none());
        assert!(state.cspace.get(dma).is_none());
        assert!(state.cspace.get(iova).is_none());
    }

    #[test]
    fn stale_driver_caps_are_rejected_after_revocation() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(33).expect("task");
        state.register_driver(33).expect("driver");

        let irq = state.mint_irq_cap(8).expect("irq");
        state.grant_driver_irq(33, irq).expect("grant irq");
        state.revoke_driver_runtime_caps(33).expect("revoke");

        assert!(state.cspace.get(irq).is_none());
        assert_eq!(
            state.grant_driver_irq(33, irq),
            Err(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn delegation_checked_bundle_requires_redelegation_after_driver_restart() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(110).expect("init-task");
        state.register_task(111).expect("driver-task");

        state
            .register_service_role(110, ServiceRole::Init)
            .expect("role init");
        state
            .register_service_role(111, ServiceRole::Driver)
            .expect("role driver");

        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let iova_cap = state.create_iova_space_cap().expect("iova");

        let first_bundle = state
            .delegate_driver_bundle_checked(110, DriverBundlePlan::standard(
                ThreadId(111),
                14,
                mem_cap,
                iova_cap,
                crate::kernel::vm::PAGE_SIZE * 4,
            ))
            .expect("first bundle");
        state
            .validate_driver_bundle_live(111, first_bundle)
            .expect("bundle live");
        assert!(state.cspace.get(first_bundle.irq_cap).is_some());
        assert!(state.cspace.get(first_bundle.dma_cap).is_some());

        let token = state.exit_task(111, 5).expect("exit");
        state.restart_task(111, token).expect("restart");

        assert_eq!(
            state.validate_driver_bundle_live(111, first_bundle),
            Err(KernelError::StaleCapability)
        );
        assert!(state.cspace.get(first_bundle.irq_cap).is_none());
        assert!(state.cspace.get(first_bundle.dma_cap).is_none());
        assert_eq!(
            state.grant_driver_irq(111, first_bundle.irq_cap),
            Err(KernelError::InvalidCapability)
        );

        assert!(state.cspace.get(iova_cap).is_none());
        let iova_cap2 = state.create_iova_space_cap().expect("iova2");

        let second_bundle = state
            .delegate_driver_bundle_checked(110, DriverBundlePlan::standard(
                ThreadId(111),
                14,
                mem_cap,
                iova_cap2,
                crate::kernel::vm::PAGE_SIZE * 4,
            ))
            .expect("second bundle");
        state
            .validate_driver_bundle_live(111, second_bundle)
            .expect("bundle live after redelegation");

        assert_ne!(first_bundle.irq_cap, second_bundle.irq_cap);
        assert_ne!(first_bundle.dma_cap, second_bundle.dma_cap);
        assert!(state.cspace.get(second_bundle.irq_cap).is_some());
        assert!(state.cspace.get(second_bundle.dma_cap).is_some());
    }

    #[test]
    fn iova_window_validation_requires_iova_space_and_range() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(12).expect("task");
        state.register_driver(12).expect("driver");

        let iova_cap = state.create_iova_space_cap().expect("iova");
        state
            .grant_driver_iova_space(12, iova_cap)
            .expect("grant iova");
        state
            .configure_driver_dma_window(
                12,
                crate::kernel::vm::PAGE_SIZE * 4,
                crate::kernel::vm::PAGE_SIZE,
            )
            .expect("window");

        assert!(state
            .validate_driver_dma_iova(
                12,
                crate::kernel::vm::PAGE_SIZE * 4,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_ok());
        assert!(state
            .validate_driver_dma_iova(
                12,
                crate::kernel::vm::PAGE_SIZE * 3,
                crate::kernel::vm::PAGE_SIZE
            )
            .is_err());
    }

    #[test]
    fn long_run_multi_core_simulation_is_deterministic() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        let (_nidx, ncap, nrecv) = state.create_notification(64).expect("notif");
        state.bind_irq_notification(7, ncap).expect("bind");

        for i in 1..=20u64 {
            state.register_task(i).expect("task");
            state
                .enqueue_on_cpu(CpuId((i % 2) as u8), i)
                .expect("enqueue");
        }

        let mut seed = 0x1234_5678u64;
        for _ in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            match seed % 3 {
                0 => state
                    .submit_cross_cpu_work(WorkItem::Reschedule {
                        target_cpu: CpuId((seed as u8) % 2),
                    })
                    .expect("work"),
                1 => {
                    if state
                        .handle_trap_event(TrapEvent::external_interrupt(7), None)
                        .is_err()
                    {
                        let _ = state.ipc_recv(nrecv);
                    }
                }
                _ => {
                    let cpu = CpuId((seed as u8) % 2);
                    state.process_cross_cpu_work_for_cpu(cpu).expect("process");
                }
            }
        }

        let mut seen = 0usize;
        while state.ipc_recv(nrecv).expect("recv").is_some() {
            seen += 1;
            if seen > 2048 {
                break;
            }
        }
        assert!(seen > 0);
        assert_eq!(state.online_cpu_count(), 2);
    }

    #[test]
    fn yield_current_rotates_to_next_runnable_task() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(40).expect("task");
        state.scheduler.enqueue(40).expect("enqueue");

        assert_eq!(state.scheduler.current_tid(), Some(0));
        state.yield_current().expect("yield");

        assert_eq!(state.scheduler.current_tid(), Some(40));
        assert_eq!(state.task_status(40), Some(TaskStatus::Running));
        assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
    }

    #[test]
    fn restart_task_honors_backoff_window() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(41).expect("task");
        state.set_task_restart_policy(41, 2, 10).expect("policy");

        let first = state.exit_task(41, 1).expect("exit1");
        state.restart_task(41, first).expect("restart1");

        let second = state.exit_task(41, 2).expect("exit2");
        assert_eq!(state.restart_task(41, second), Err(KernelError::WouldBlock));
    }

    #[test]
    fn trap_event_page_fault_records_fault_then_faults_current_task() {
        let mut state = Bootstrap::init().expect("init");
        let fault = FaultInfo {
            addr: VirtAddr(0x4000),
            access: FaultAccess::Execute,
        };

        state
            .handle_trap_event(TrapEvent::page_fault(fault), None)
            .expect("handle page fault");

        assert_eq!(state.last_fault(), Some(fault));
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    }

    #[test]
    fn cross_cpu_work_for_other_cpu_is_deferred_not_dropped() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        state
            .submit_cross_cpu_work(WorkItem::Reschedule {
                target_cpu: CpuId(1),
            })
            .expect("submit");

        let processed_cpu0 = state
            .process_cross_cpu_work_for_cpu(CpuId(0))
            .expect("process cpu0");
        assert_eq!(processed_cpu0, 0);

        let processed_cpu1 = state
            .process_cross_cpu_work_for_cpu(CpuId(1))
            .expect("process cpu1");
        assert_eq!(processed_cpu1, 1);
    }

    #[test]
    fn spawn_user_thread_inherits_group_and_asid_and_sets_tls() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 7,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("parent");

        let tid = state
            .spawn_user_thread(7, 0xDEAD_BEEF, 0x8000_0000, 0x4010)
            .expect("thread");

        assert_eq!(state.thread_group_id(tid), Some(7));
        assert_eq!(state.task_asid(tid), Some(asid));
        assert_eq!(state.thread_tls_base(tid), Some(0xDEAD_BEEF));
        assert_eq!(state.task_status(tid), Some(TaskStatus::Runnable));
    }

    #[test]
    fn futex_wait_blocks_current_and_wake_requeues_waiter() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.scheduler.enqueue(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        state.yield_current().expect("switch");
        assert_eq!(state.scheduler.current_tid(), Some(1));

        assert!(state.futex_wait_current(0x1000, 3, 3).expect("wait"));
        assert_eq!(
            state.task_status(1),
            Some(TaskStatus::Blocked(WaitReason::Futex(0x1000)))
        );
        assert_eq!(state.futex_wake(0x1000, 1).expect("wake"), 1);
        assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
    }

    #[test]
    fn thread_state_helpers_cover_pid_tls_context_join_and_robust_futex() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 9,
                entry: 0x5000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("leader");
        let tid = state
            .spawn_user_thread(9, 0xCAFE_BABE, 0x9000_0000, 0x5010)
            .expect("thread");

        assert_eq!(state.process_id(tid), Some(9));
        assert!(!state.is_thread_group_leader(tid));
        assert_eq!(
            state.take_tls_restore_request(tid).expect("tls request"),
            Some(0xCAFE_BABE)
        );
        assert_eq!(
            state.take_tls_restore_request(tid).expect("tls cleared"),
            None
        );

        state
            .set_thread_user_context(
                tid,
                UserRegisterContext {
                    instruction_ptr: 0x6000,
                    stack_ptr: 0xA000_0000,
                    arg0: 7,
                    arg1: 8,
                },
            )
            .expect("ctx");
        assert_eq!(
            state.thread_user_context(tid).expect("ctx read"),
            UserRegisterContext {
                instruction_ptr: 0x6000,
                stack_ptr: 0xA000_0000,
                arg0: 7,
                arg1: 8,
            }
        );

        state.set_robust_futex_head(tid, 0x7000, 2).expect("robust");
        assert_eq!(
            state.robust_futex_state(tid),
            Some(RobustFutexState {
                head: 0x7000,
                len: 2
            })
        );

        state.mark_thread_detached(tid).expect("detach");
        assert_eq!(
            state.thread_detach_state(tid),
            Some(ThreadDetachState::Detached)
        );
        assert_eq!(state.join_thread(tid), Err(KernelError::WrongObject));

        let joinable = state
            .spawn_user_thread(9, 0xD00D_BEEF, 0x9100_0000, 0x5020)
            .expect("joinable");
        state.exit_task(joinable, 23).expect("exit");
        assert_eq!(state.join_thread(joinable).expect("join"), Some(23));
        assert_eq!(state.task_status(joinable), Some(TaskStatus::Dead));
    }

    #[test]
    fn trap_frame_resume_and_tls_request_are_consumed_for_current_thread() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 20,
                entry: 0x7000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("leader");
        let tid = state
            .spawn_user_thread(20, 0xABCD_0000, 0x8800_0000, 0x7010)
            .expect("thread");
        state.yield_current().expect("switch");
        assert_eq!(state.scheduler.current_tid(), Some(tid));

        let mut frame = TrapFrame::new(0, [11, 22, 0, 0, 0, 0]);
        let tls = state
            .resume_current_thread_with_frame(&mut frame)
            .expect("resume");
        assert_eq!(tls, Some(0xABCD_0000));
        assert_eq!(frame.ret0, 0x7010);
        assert_eq!(frame.ret1, 0x8800_0000);

        frame.ret0 = 0x9000;
        frame.ret1 = 0x9900_0000;
        frame.args[0] = 33;
        frame.args[1] = 44;
        state
            .sync_current_thread_from_frame(&frame)
            .expect("capture");
        assert_eq!(
            state.thread_user_context(tid),
            Some(UserRegisterContext {
                instruction_ptr: 0x9000,
                stack_ptr: 0x9900_0000,
                arg0: 33,
                arg1: 44,
            })
        );
    }

    #[test]
    fn join_blocks_until_target_exits_and_detached_threads_reap_on_exit() {
        let mut state = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .spawn_user_task_from_image(UserImageSpec {
                tid: 30,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("leader");
        let joiner = state
            .spawn_user_thread(30, 0xCAFE_1000, 0x8100_0000, 0x4010)
            .expect("joiner");
        state.yield_current().expect("switch to joiner");
        assert_eq!(state.scheduler.current_tid(), Some(joiner));

        assert_eq!(state.join_thread(30).expect("join pending"), None);
        assert_eq!(
            state.task_status(joiner),
            Some(TaskStatus::Blocked(WaitReason::Join(30)))
        );

        state.exit_task(30, 5).expect("exit leader");
        assert_eq!(state.task_status(joiner), Some(TaskStatus::Runnable));

        state.mark_thread_detached(joiner).expect("detach");
        state.exit_task(joiner, 9).expect("exit detached");
        assert_eq!(state.task_status(joiner), Some(TaskStatus::Dead));
    }

    #[test]
    fn robust_futex_exit_cleanup_wakes_matching_waiters() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(40).expect("owner");
        state.register_task(41).expect("waiter");
        state.set_robust_futex_head(40, 0x2000, 1).expect("robust");
        state.scheduler.enqueue(40).expect("owner enqueue");
        state.scheduler.enqueue(41).expect("waiter enqueue");
        state.dispatch_next_task().expect("dispatch");
        state.yield_current().expect("to owner");
        state.yield_current().expect("to waiter");
        assert_eq!(state.scheduler.current_tid(), Some(41));
        assert!(state.futex_wait_current(0x2000, 1, 1).expect("wait"));
        assert_eq!(
            state.task_status(41),
            Some(TaskStatus::Blocked(WaitReason::Futex(0x2000)))
        );
        state.exit_task(40, 0).expect("owner exit");
        assert_eq!(state.task_status(41), Some(TaskStatus::Runnable));
    }
}
