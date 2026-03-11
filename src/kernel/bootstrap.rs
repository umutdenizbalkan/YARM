use super::capabilities::{CapId, CapObject, CapRights, Capability, CapabilitySpace};
use super::ipc::{Endpoint, EndpointMode, Message};
use super::scheduler::{CpuId, SmpScheduler};
use super::smp::{CrossCpuWorkQueue, MAX_CROSS_CPU_WORK, WorkItem};
use super::syscall::{SyscallError, dispatch as dispatch_syscall};
use super::task::{TaskClass, TaskStatus, ThreadControlBlock, WaitReason};
use super::timer::Timer;
use super::trap::{FaultInfo, Trap, TrapAction, TrapEvent, route_trap};
use super::trapframe::TrapFrame;
use super::vm::{
    AddressSpace, AddressSpaceManager, Asid, Mapping, PageFlags, PhysAddr, VirtAddr, VmError,
};

const MAX_ENDPOINTS: usize = 16;
const MAX_TASKS: usize = 64;
const MAX_TASK_MEM_ENTRIES: usize = 2048;
const MAX_MEMORY_OBJECTS: usize = 128;
const MAX_NOTIFICATIONS: usize = 16;
const MAX_IRQ_LINES: usize = 64;
const MAX_DRIVERS: usize = 32;
const RESTART_ESCALATION_THRESHOLD: u32 = 3;

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
pub struct DeviceServerDelegation {
    pub server_tid: u64,
    pub irq_line: u16,
    pub mem_cap: CapId,
    pub dma_offset: usize,
    pub dma_len: usize,
    pub iova_cap: CapId,
    pub iova_base: usize,
    pub iova_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcFastpathResult {
    pub switched_to_waiter: bool,
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
    tid: u64,
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
    tid: u64,
    irq_cap: Option<CapId>,
    dma_cap: Option<CapId>,
    dma_iova_base: Option<usize>,
    dma_iova_len: Option<usize>,
    iova_space_cap: Option<CapId>,
}

#[derive(Debug)]
pub struct KernelState {
    pub kernel_aspace: AddressSpace,
    pub scheduler: SmpScheduler,
    pub cspace: CapabilitySpace,
    pub timer: Timer,
    pub user_spaces: AddressSpaceManager,
    cross_cpu_work: CrossCpuWorkQueue,
    endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
    endpoint_waiters: [Option<u64>; MAX_ENDPOINTS],
    endpoint_sender_waiters: [Option<(u64, Message)>; MAX_ENDPOINTS],
    endpoint_generations: [u64; MAX_ENDPOINTS],
    notifications: [Option<NotificationObject>; MAX_NOTIFICATIONS],
    notification_generations: [u64; MAX_NOTIFICATIONS],
    irq_routes: [Option<usize>; MAX_IRQ_LINES],
    tcbs: [Option<ThreadControlBlock>; MAX_TASKS],
    task_mem: [Option<TaskMemByte>; MAX_TASK_MEM_ENTRIES],
    memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],
    next_memory_object_id: u64,
    next_iova_space_id: u64,
    next_anon_phys: usize,
    tlb_shootdown_count: u64,
    last_fault: Option<FaultInfo>,
    fault_handler_endpoint: Option<usize>,
    linux_proc_mgr_request_send: Option<CapId>,
    linux_proc_mgr_reply_recv: Option<CapId>,
    linux_vfs_request_send: Option<CapId>,
    linux_vfs_reply_recv: Option<CapId>,
    supervisor_endpoint: Option<usize>,
    driver_records: [Option<DriverRecord>; MAX_DRIVERS],
    next_restart_token: u64,
    fault_policy: FaultPolicy,
    app_restart_policy: RestartPolicy,
    driver_restart_policy: RestartPolicy,
    system_restart_policy: RestartPolicy,
    app_escalation_threshold: u32,
    driver_escalation_threshold: u32,
    system_escalation_threshold: u32,
}

pub struct Bootstrap;

impl Bootstrap {
    pub fn init() -> Result<KernelState, KernelError> {
        let mut kernel_aspace = AddressSpace::new_kernel();
        kernel_aspace
            .map_page(
                VirtAddr(0xFFFF_0000),
                Mapping {
                    phys: PhysAddr(0x0),
                    flags: PageFlags::KERNEL_RW,
                },
            )
            .map_err(|err| match err {
                VmError::Full => KernelError::VmFull,
                other => KernelError::Vm(other),
            })?;

        let mut scheduler = SmpScheduler::default();
        scheduler
            .enqueue_on(CpuId(0), 0)
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
            timer: Timer::new(10),
            user_spaces: AddressSpaceManager::default(),
            cross_cpu_work: CrossCpuWorkQueue::default(),
            endpoints: [const { None }; MAX_ENDPOINTS],
            endpoint_waiters: [None; MAX_ENDPOINTS],
            endpoint_sender_waiters: [None; MAX_ENDPOINTS],
            endpoint_generations: [0; MAX_ENDPOINTS],
            notifications: [const { None }; MAX_NOTIFICATIONS],
            notification_generations: [0; MAX_NOTIFICATIONS],
            irq_routes: [None; MAX_IRQ_LINES],
            tcbs: [None; MAX_TASKS],
            task_mem: [None; MAX_TASK_MEM_ENTRIES],
            memory_objects: [None; MAX_MEMORY_OBJECTS],
            next_memory_object_id: 1,
            next_iova_space_id: 1,
            next_anon_phys: 0x1000_0000,
            tlb_shootdown_count: 0,
            last_fault: None,
            fault_handler_endpoint: None,
            linux_proc_mgr_request_send: None,
            linux_proc_mgr_reply_recv: None,
            linux_vfs_request_send: None,
            linux_vfs_reply_recv: None,
            supervisor_endpoint: None,
            driver_records: [const { None }; MAX_DRIVERS],
            next_restart_token: 1,
            fault_policy: FaultPolicy::KillTask,
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
        };

        state.register_task(0)?;
        state.dispatch_next_task()?;
        Ok(state)
    }
}

impl KernelState {
    fn tcb_mut(&mut self, tid: u64) -> Option<&mut ThreadControlBlock> {
        self.tcbs.iter_mut().flatten().find(|tcb| tcb.tid == tid)
    }

    pub fn task_status(&self, tid: u64) -> Option<TaskStatus> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid == tid)
            .map(|tcb| tcb.status)
    }

    pub fn last_fault(&self) -> Option<FaultInfo> {
        self.last_fault
    }

    pub fn clear_last_fault(&mut self) {
        self.last_fault = None;
    }

    pub fn record_fault(&mut self, fault: FaultInfo) {
        self.last_fault = Some(fault);
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
        self.fault_handler_endpoint = Some(endpoint_idx);
        Ok(())
    }

    pub fn set_fault_policy(&mut self, policy: FaultPolicy) {
        self.fault_policy = policy;
    }

    pub fn fault_policy(&self) -> FaultPolicy {
        self.fault_policy
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
            .find(|tcb| tcb.tid == tid)
            .and_then(|tcb| tcb.fault_policy_override)
            .unwrap_or(self.fault_policy)
    }

    pub fn task_asid(&self, tid: u64) -> Option<Asid> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid == tid)
            .and_then(|tcb| tcb.asid)
    }

    pub fn register_linux_process_manager(
        &mut self,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        if !self.cspace.has_right(request_send_cap, CapRights::Send) {
            return Err(KernelError::MissingRight);
        }
        if !self.cspace.has_right(reply_recv_cap, CapRights::Receive) {
            return Err(KernelError::MissingRight);
        }
        let req_obj = self
            .cspace
            .get(request_send_cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let rep_obj = self
            .cspace
            .get(reply_recv_cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let _ = self.resolve_endpoint_index(req_obj)?;
        let _ = self.resolve_endpoint_index(rep_obj)?;

        self.linux_proc_mgr_request_send = Some(request_send_cap);
        self.linux_proc_mgr_reply_recv = Some(reply_recv_cap);
        Ok(())
    }

    pub fn send_linux_process_manager_request(
        &mut self,
        opcode: u16,
        arg0: u64,
    ) -> Result<(), KernelError> {
        let send_cap = self
            .linux_proc_mgr_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let msg = Message::with_header(0, opcode, 0, None, &arg0.to_le_bytes())
            .map_err(|_| KernelError::WrongObject)?;
        self.ipc_send(send_cap, msg)
    }

    pub fn send_linux_process_manager_request2(
        &mut self,
        opcode: u16,
        arg0: u64,
        arg1: u64,
    ) -> Result<(), KernelError> {
        let send_cap = self
            .linux_proc_mgr_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&arg0.to_le_bytes());
        payload[8..16].copy_from_slice(&arg1.to_le_bytes());
        let msg = Message::with_header(0, opcode, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        self.ipc_send(send_cap, msg)
    }

    pub fn recv_linux_process_manager_reply(&mut self) -> Result<Option<Message>, KernelError> {
        let recv_cap = self
            .linux_proc_mgr_reply_recv
            .ok_or(KernelError::InvalidCapability)?;
        self.ipc_recv(recv_cap)
    }

    pub fn register_linux_vfs_manager(
        &mut self,
        request_send_cap: CapId,
        reply_recv_cap: CapId,
    ) -> Result<(), KernelError> {
        if !self.cspace.has_right(request_send_cap, CapRights::Send) {
            return Err(KernelError::MissingRight);
        }
        if !self.cspace.has_right(reply_recv_cap, CapRights::Receive) {
            return Err(KernelError::MissingRight);
        }
        let req_obj = self
            .cspace
            .get(request_send_cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let rep_obj = self
            .cspace
            .get(reply_recv_cap)
            .ok_or(KernelError::InvalidCapability)?
            .object;
        let _ = self.resolve_endpoint_index(req_obj)?;
        let _ = self.resolve_endpoint_index(rep_obj)?;

        self.linux_vfs_request_send = Some(request_send_cap);
        self.linux_vfs_reply_recv = Some(reply_recv_cap);
        Ok(())
    }

    pub fn send_linux_vfs_request(
        &mut self,
        opcode: u16,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        let send_cap = self
            .linux_vfs_request_send
            .ok_or(KernelError::InvalidCapability)?;
        let msg = Message::with_header(0, opcode, 0, None, payload)
            .map_err(|_| KernelError::WrongObject)?;
        self.ipc_send(send_cap, msg)
    }

    pub fn recv_linux_vfs_reply(&mut self) -> Result<Option<Message>, KernelError> {
        let recv_cap = self
            .linux_vfs_reply_recv
            .ok_or(KernelError::InvalidCapability)?;
        self.ipc_recv(recv_cap)
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
        self.supervisor_endpoint = Some(endpoint_idx);
        Ok(())
    }

    pub fn register_driver(&mut self, tid: u64) -> Result<(), KernelError> {
        let _ = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        if self
            .driver_records
            .iter()
            .flatten()
            .any(|record| record.tid == tid)
        {
            return Ok(());
        }

        if let Some(slot) = self.driver_records.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(DriverRecord {
                tid,
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
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
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
        let id = self.next_iova_space_id;
        self.next_iova_space_id = self.next_iova_space_id.wrapping_add(1).max(1);
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
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
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
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
            .ok_or(KernelError::TaskMissing)?;
        record.dma_cap = Some(dma_cap);
        Ok(())
    }

    pub fn delegate_device_server_caps(
        &mut self,
        plan: DeviceServerDelegation,
    ) -> Result<(CapId, CapId), KernelError> {
        self.register_driver(plan.server_tid)?;

        let irq_cap = self.mint_irq_cap(plan.irq_line)?;
        self.grant_driver_irq(plan.server_tid, irq_cap)?;

        let dma_cap = self.mint_dma_region_cap(plan.mem_cap, plan.dma_offset, plan.dma_len)?;
        self.grant_driver_dma(plan.server_tid, dma_cap)?;

        self.grant_driver_iova_space(plan.server_tid, plan.iova_cap)?;
        self.configure_driver_dma_window(plan.server_tid, plan.iova_base, plan.iova_len)?;

        Ok((irq_cap, dma_cap))
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
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
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
            .driver_records
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
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
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
            .ok_or(KernelError::TaskMissing)?;
        record.iova_space_cap = None;
        record.dma_iova_base = None;
        record.dma_iova_len = None;
        Ok(())
    }

    pub fn revoke_driver_runtime_caps(&mut self, tid: u64) -> Result<(), KernelError> {
        let record = self
            .driver_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
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
        let Some(endpoint_idx) = self.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..16].copy_from_slice(&code.to_le_bytes());
        let msg = Message::with_header(0, 0xEE, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        let endpoint = self
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
        let Some(endpoint_idx) = self.supervisor_endpoint else {
            return Ok(());
        };
        let mut payload = [0u8; 16];
        payload[..8].copy_from_slice(&tid.to_le_bytes());
        payload[8..12].copy_from_slice(&denied_count.to_le_bytes());
        let msg = Message::with_header(0, 0xEF, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        let endpoint = self
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
        tcb.restart_budget = budget;
        tcb.restart_backoff_ticks = backoff_ticks;
        Ok(())
    }

    pub fn task_restart_telemetry(&self, tid: u64) -> Result<RestartTelemetry, KernelError> {
        let tcb = self
            .tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid == tid)
            .ok_or(KernelError::TaskMissing)?;
        Ok(RestartTelemetry {
            budget_remaining: tcb.restart_budget,
            backoff_ticks: tcb.restart_backoff_ticks,
            available_at_tick: tcb.restart_available_at_tick,
            token_outstanding: tcb.restart_token.is_some(),
            denied_count: tcb.restart_denied_count,
            escalation_count: tcb.restart_escalation_count,
            last_exit_code: tcb.last_exit_code,
        })
    }

    pub fn exit_task(&mut self, tid: u64, code: u64) -> Result<u64, KernelError> {
        let token = self.next_restart_token;
        self.next_restart_token = self.next_restart_token.wrapping_add(1).max(1);

        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Exited(code);
        tcb.restart_token = Some(token);
        tcb.last_exit_code = Some(code);
        self.report_task_exit_to_supervisor(tid, code)?;

        if self.scheduler.current_tid() == Some(tid) {
            let _ = self.scheduler.block_current();
            let _ = self.dispatch_next_task()?;
        }

        Ok(token)
    }

    pub fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelError> {
        let now = self.timer.current_ticks();
        let (app_threshold, driver_threshold, system_threshold) = (
            self.app_escalation_threshold,
            self.driver_escalation_threshold,
            self.system_escalation_threshold,
        );

        let mut should_notify = None;
        let err = {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            let mut denied = false;
            let err = if tcb.restart_token != Some(token) {
                denied = true;
                Some(KernelError::WrongObject)
            } else if tcb.restart_budget == 0 {
                denied = true;
                Some(KernelError::WouldBlock)
            } else if now < tcb.restart_available_at_tick {
                denied = true;
                Some(KernelError::WouldBlock)
            } else {
                None
            };

            if denied {
                tcb.restart_denied_count = tcb.restart_denied_count.saturating_add(1);
                let threshold = match tcb.class {
                    TaskClass::App => app_threshold,
                    TaskClass::Driver => driver_threshold,
                    TaskClass::SystemServer => system_threshold,
                };
                if tcb.restart_denied_count.is_multiple_of(threshold) {
                    tcb.restart_escalation_count = tcb.restart_escalation_count.saturating_add(1);
                    should_notify = Some(tcb.restart_denied_count);
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
        tcb.restart_budget = tcb.restart_budget.saturating_sub(1);
        tcb.restart_available_at_tick = now.saturating_add(tcb.restart_backoff_ticks);
        tcb.restart_token = None;
        tcb.status = TaskStatus::Runnable;
        self.scheduler
            .enqueue(tid)
            .map_err(|_| KernelError::SchedulerFull)
    }

    pub fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Dead;
        tcb.restart_token = None;
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
        self.cross_cpu_work
            .submit(item)
            .map_err(|_| KernelError::TaskTableFull)
    }

    pub fn drain_cross_cpu_work(&self) -> Option<WorkItem> {
        self.cross_cpu_work.take()
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
                let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                self.enqueue_on_cpu(target_cpu, tid)
            }
        }
    }

    pub fn process_cross_cpu_work_for_cpu(&mut self, cpu: CpuId) -> Result<usize, KernelError> {
        let mut deferred = [None; MAX_CROSS_CPU_WORK];
        let mut deferred_len = 0usize;
        let mut processed = 0usize;

        while let Some(item) = self.cross_cpu_work.take() {
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
                self.cross_cpu_work
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
            for slot in &mut self.task_mem {
                if slot
                    .as_ref()
                    .is_some_and(|entry| entry.tid == tid && entry.addr == va)
                {
                    slot.as_mut().expect("checked").value = data[i];
                    found = true;
                    break;
                }
            }

            if !found {
                let slot = self
                    .task_mem
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(KernelError::TaskTableFull)?;
                *slot = Some(TaskMemByte {
                    tid,
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
                .task_mem
                .iter()
                .flatten()
                .find(|entry| entry.tid == tid && entry.addr == va)
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
            .resolve(VirtAddr(page_base))
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
            TaskClass::App => self.app_escalation_threshold = bounded,
            TaskClass::Driver => self.driver_escalation_threshold = bounded,
            TaskClass::SystemServer => self.system_escalation_threshold = bounded,
        }
    }

    fn restart_policy_for_class(&self, class: TaskClass) -> RestartPolicy {
        match class {
            TaskClass::App => self.app_restart_policy,
            TaskClass::Driver => self.driver_restart_policy,
            TaskClass::SystemServer => self.system_restart_policy,
        }
    }

    pub fn class_policy_snapshot(&self, class: TaskClass) -> ClassPolicySnapshot {
        let policy = self.restart_policy_for_class(class);
        let escalation_threshold = match class {
            TaskClass::App => self.app_escalation_threshold,
            TaskClass::Driver => self.driver_escalation_threshold,
            TaskClass::SystemServer => self.system_escalation_threshold,
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
            TaskClass::App => self.app_restart_policy = policy,
            TaskClass::Driver => self.driver_restart_policy = policy,
            TaskClass::SystemServer => self.system_restart_policy = policy,
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
                tid,
                class,
                status: TaskStatus::Runnable,
                asid: None,
                fault_policy_override: None,
                brk_base: None,
                brk_end: None,
                restart_token: None,
                restart_budget: policy.budget,
                restart_backoff_ticks: policy.backoff_ticks,
                restart_available_at_tick: 0,
                restart_denied_count: 0,
                restart_escalation_count: 0,
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
        let _ = self.scheduler.on_preempt();
        if let Some(tid) = self.scheduler.current_tid() {
            let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Running;
        }
        Ok(())
    }

    fn emit_fault_report(&mut self, faulted_tid: u64) {
        let Some(endpoint_idx) = self.fault_handler_endpoint else {
            return;
        };
        let Some(fault) = self.last_fault else {
            return;
        };

        let mut payload = [0u8; 17];
        payload[..8].copy_from_slice(&faulted_tid.to_le_bytes());
        let addr_bytes = (fault.addr as u64).to_le_bytes();
        payload[8..16].copy_from_slice(&addr_bytes);
        payload[16] = match fault.access {
            super::trap::FaultAccess::Read => 0,
            super::trap::FaultAccess::Write => 1,
        };

        let msg = match Message::new(0, &payload) {
            Ok(msg) => msg,
            Err(_) => return,
        };

        let sent = if let Some(endpoint) = self
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

    fn block_current_on_receive(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        let blocked_tid = self
            .scheduler
            .block_current()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(endpoint_idx));
        self.endpoint_waiters[endpoint_idx] = Some(blocked_tid);
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn block_current_on_send(
        &mut self,
        endpoint_idx: usize,
        msg: Message,
    ) -> Result<(), KernelError> {
        if self.endpoint_sender_waiters[endpoint_idx].is_some() {
            return Err(KernelError::EndpointQueueFull);
        }

        let blocked_tid = self
            .scheduler
            .block_current()
            .ok_or(KernelError::TaskMissing)?;
        let tcb = self.tcb_mut(blocked_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(endpoint_idx));
        self.endpoint_sender_waiters[endpoint_idx] = Some((blocked_tid, msg));
        let _ = self.dispatch_next_task()?;
        Ok(())
    }

    fn wake_waiter_for_endpoint(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        if let Some(waiter_tid) = self.endpoint_waiters[endpoint_idx].take() {
            let tcb = self.tcb_mut(waiter_tid).ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Runnable;
            self.scheduler
                .enqueue(waiter_tid)
                .map_err(|_| KernelError::SchedulerFull)?;
        }
        Ok(())
    }

    fn wake_sender_waiter(&mut self, sender_tid: u64) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(sender_tid).ok_or(KernelError::TaskMissing)?;
        tcb.status = TaskStatus::Runnable;
        self.scheduler
            .enqueue(sender_tid)
            .map_err(|_| KernelError::SchedulerFull)
    }

    fn resolve_endpoint_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Endpoint { index, generation } => {
                if index >= MAX_ENDPOINTS {
                    return Err(KernelError::WrongObject);
                }
                if self.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.endpoint_generations[index] != generation {
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
        if endpoint_idx >= MAX_ENDPOINTS || self.endpoints[endpoint_idx].is_none() {
            return Err(KernelError::WrongObject);
        }
        self.endpoints[endpoint_idx] = None;
        if self.fault_handler_endpoint == Some(endpoint_idx) {
            self.fault_handler_endpoint = None;
        }
        self.endpoint_waiters[endpoint_idx] = None;
        self.endpoint_sender_waiters[endpoint_idx] = None;
        let mut next_generation = self.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.endpoint_generations[endpoint_idx] = next_generation;
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
        for (idx, slot) in self.endpoints.iter().enumerate() {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let endpoint_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation = self.endpoint_generations[endpoint_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.endpoint_generations[endpoint_idx] = next_generation;
        self.endpoints[endpoint_idx] =
            Some(Endpoint::new_with_mode(max_depth, mode).map_err(|_| KernelError::WrongObject)?);

        let send_cap = self
            .cspace
            .mint(Capability::new(
                "endpoint_send",
                CapObject::Endpoint {
                    index: endpoint_idx,
                    generation: self.endpoint_generations[endpoint_idx],
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
                    generation: self.endpoint_generations[endpoint_idx],
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
        for (idx, slot) in self.notifications.iter().enumerate() {
            if slot.is_none() {
                slot_index = Some(idx);
                break;
            }
        }

        let notification_idx = slot_index.ok_or(KernelError::EndpointFull)?;
        let mut next_generation = self.notification_generations[notification_idx].wrapping_add(1);
        if next_generation == 0 {
            next_generation = 1;
        }
        self.notification_generations[notification_idx] = next_generation;
        self.notifications[notification_idx] = Some(NotificationObject { endpoint_idx });

        let notification_cap = self
            .cspace
            .mint(Capability::new(
                "notification",
                CapObject::Notification {
                    index: notification_idx,
                    generation: self.notification_generations[notification_idx],
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
                if index >= MAX_NOTIFICATIONS || self.notifications[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if self.notification_generations[index] != generation {
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
        self.irq_routes[irq_idx] = Some(notif_idx);
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notification_idx: usize,
        irq_line: u16,
    ) -> Result<(), KernelError> {
        let notif = self.notifications[notification_idx].ok_or(KernelError::WrongObject)?;
        let payload = irq_line.to_le_bytes();
        let msg = Message::with_header(0, irq_line, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        if let Some(endpoint) = self.endpoints[notif.endpoint_idx].as_mut() {
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
        let Some(notification_idx) = self.irq_routes.get(irq_idx).copied().flatten() else {
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
            .endpoints
            .get(endpoint_idx)
            .and_then(Option::as_ref)
            .ok_or(KernelError::WrongObject)?
            .mode();

        if endpoint_mode == EndpointMode::Synchronous
            && self.endpoint_waiters[endpoint_idx].is_none()
        {
            self.block_current_on_send(endpoint_idx, msg)?;
            return Err(KernelError::WouldBlock);
        }

        let endpoint = self
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        endpoint
            .send(msg)
            .map_err(|_| KernelError::EndpointQueueFull)?;

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
        let had_waiter = self.endpoint_waiters[endpoint_idx].is_some();

        self.ipc_send(send_cap, msg)?;

        if had_waiter {
            let _ = self.dispatch_next_task()?;
        }

        Ok(IpcFastpathResult {
            switched_to_waiter: had_waiter,
        })
    }

    pub fn ipc_send_with_cap_transfer(
        &mut self,
        send_cap: CapId,
        sender_tid: u64,
        opcode: u16,
        transfer_cap: CapId,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        if self.cspace.get(transfer_cap).is_none() {
            return Err(KernelError::InvalidCapability);
        }
        let msg = Message::with_header(
            sender_tid,
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
            .endpoints
            .get_mut(endpoint_idx)
            .and_then(Option::as_mut)
            .ok_or(KernelError::WrongObject)?;

        if let Some(msg) = endpoint.recv() {
            if let Some((sender_tid, pending_msg)) =
                self.endpoint_sender_waiters[endpoint_idx].take()
            {
                endpoint
                    .send(pending_msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
                self.wake_sender_waiter(sender_tid)?;
            }
            return Ok(Some(msg));
        }

        if let Some((sender_tid, pending_msg)) = self.endpoint_sender_waiters[endpoint_idx].take() {
            self.wake_sender_waiter(sender_tid)?;
            return Ok(Some(pending_msg));
        }

        self.block_current_on_receive(endpoint_idx)?;
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
        if !phys.0.is_multiple_of(crate::kernel::vm::PAGE_SIZE) {
            return Err(KernelError::Vm(VmError::Misaligned));
        }
        let id = self.next_memory_object_id;
        self.next_memory_object_id = self.next_memory_object_id.wrapping_add(1);

        let slot = self
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
        let phys = PhysAddr(self.next_anon_phys);
        self.next_anon_phys = self
            .next_anon_phys
            .wrapping_add(crate::kernel::vm::PAGE_SIZE);
        self.create_memory_object(phys)
    }

    pub fn task_brk_bounds(&self, tid: u64) -> Option<(usize, usize)> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid == tid)
            .and_then(|tcb| Some((tcb.brk_base?, tcb.brk_end?)))
    }

    pub fn set_task_brk_bounds(
        &mut self,
        tid: u64,
        base: usize,
        end: usize,
    ) -> Result<(), KernelError> {
        let tcb = self.tcb_mut(tid).ok_or(KernelError::TaskMissing)?;
        tcb.brk_base = Some(base);
        tcb.brk_end = Some(end);
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

        self.memory_objects
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
        match route_trap(trap) {
            TrapAction::DispatchSyscall => {
                self.clear_last_fault();
                let trapframe = frame.ok_or(TrapHandleError::MissingTrapFrame)?;
                dispatch_syscall(self, trapframe).map_err(TrapHandleError::Syscall)?;
                if trapframe.error == SyscallError::PageFault.code() {
                    self.fault_current_task()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            TrapAction::TickScheduler => {
                self.timer.tick();
                if self.timer.should_preempt() {
                    self.yield_current()
                        .map_err(SyscallError::from)
                        .map_err(TrapHandleError::Syscall)?;
                }
                Ok(())
            }
            TrapAction::HandlePageFault | TrapAction::HandleDeviceInterrupt => Ok(()),
        }
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

    #[test]
    fn bootstrap_sets_minimal_kernel_state() {
        let state = Bootstrap::init().expect("bootstrap should fit static limits");
        assert_eq!(state.kernel_aspace.mappings(), 1);
        assert_eq!(state.online_cpu_count(), 1);
        assert_eq!(state.scheduler.current_tid().expect("boot task"), 0);
        assert_eq!(state.task_status(0), Some(TaskStatus::Running));
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
                tid: 2,
            })
            .expect("submit wake");
        state
            .submit_cross_cpu_work(WorkItem::TlbShootdown {
                target_cpu: CpuId(0),
                asid: 1,
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
                tid: 2
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
                TrapEvent::with_fault(
                    Trap::PageFault,
                    FaultInfo {
                        addr: 0x1200,
                        access: super::super::trap::FaultAccess::Read,
                    },
                ),
                None,
            )
            .expect("page fault event handled");

        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.scheduler.current_tid(), Some(1));
        assert_eq!(
            state.last_fault(),
            Some(FaultInfo {
                addr: 0x1200,
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
            Some(TaskStatus::Blocked(WaitReason::EndpointReceive(0)))
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
            Some(TaskStatus::Blocked(WaitReason::EndpointSend(0)))
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
            .ipc_send_with_cap_transfer(send_cap, 0, 0x55, mem_cap, b"mt")
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
        assert_eq!(recv_frame.ret1 & 0xFF, b'h' as usize);
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
                addr: 8,
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
            .handle_trap_event(TrapEvent::with_irq(Trap::ExternalInterrupt, 11), None)
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
            server_tid: 34,
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
        assert!(
            state
                .validate_driver_dma_iova(
                    34,
                    crate::kernel::vm::PAGE_SIZE * 8,
                    crate::kernel::vm::PAGE_SIZE,
                )
                .is_ok()
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
        assert_eq!(state.task_status(9), Some(TaskStatus::Exited(12)));

        assert!(state.restart_task(9, token).is_ok());
        assert_eq!(state.task_status(9), Some(TaskStatus::Runnable));

        state.mark_task_dead(9).expect("dead");
        assert_eq!(state.task_status(9), Some(TaskStatus::Dead));
    }

    #[test]
    fn dma_region_cap_enforces_window_constraints() {
        let mut state = Bootstrap::init().expect("init");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");

        assert!(
            state
                .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE)
                .is_ok()
        );
        assert!(
            state
                .mint_dma_region_cap(mem_cap, 1, crate::kernel::vm::PAGE_SIZE)
                .is_err()
        );
        assert!(state.mint_dma_region_cap(mem_cap, 0, 0).is_err());
        assert!(
            state
                .mint_dma_region_cap(mem_cap, 0, crate::kernel::vm::PAGE_SIZE * 2)
                .is_err()
        );
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
                .enqueue_on_cpu(CpuId((i % 2) as usize), i)
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
                .handle_trap_event(TrapEvent::with_irq(Trap::ExternalInterrupt, 5), None)
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

        assert!(
            state
                .validate_driver_dma_iova(
                    22,
                    crate::kernel::vm::PAGE_SIZE * 8,
                    crate::kernel::vm::PAGE_SIZE
                )
                .is_err()
        );
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
        assert!(
            state
                .validate_driver_dma_iova(
                    31,
                    crate::kernel::vm::PAGE_SIZE * 2,
                    crate::kernel::vm::PAGE_SIZE
                )
                .is_ok()
        );

        state.detach_driver_iova_space(31).expect("detach");
        assert!(
            state
                .validate_driver_dma_iova(
                    31,
                    crate::kernel::vm::PAGE_SIZE * 2,
                    crate::kernel::vm::PAGE_SIZE
                )
                .is_err()
        );
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

        assert!(
            state
                .validate_driver_dma_iova(
                    12,
                    crate::kernel::vm::PAGE_SIZE * 4,
                    crate::kernel::vm::PAGE_SIZE
                )
                .is_ok()
        );
        assert!(
            state
                .validate_driver_dma_iova(
                    12,
                    crate::kernel::vm::PAGE_SIZE * 3,
                    crate::kernel::vm::PAGE_SIZE
                )
                .is_err()
        );
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
                .enqueue_on_cpu(CpuId((i % 2) as usize), i)
                .expect("enqueue");
        }

        let mut seed = 0x1234_5678u64;
        for _ in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            match seed % 3 {
                0 => state
                    .submit_cross_cpu_work(WorkItem::Reschedule {
                        target_cpu: CpuId((seed as usize) % 2),
                    })
                    .expect("work"),
                1 => {
                    if state
                        .handle_trap_event(TrapEvent::with_irq(Trap::ExternalInterrupt, 7), None)
                        .is_err()
                    {
                        let _ = state.ipc_recv(nrecv);
                    }
                }
                _ => {
                    let cpu = CpuId((seed as usize) % 2);
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
}
