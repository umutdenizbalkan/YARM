mod driver_state;
mod exec_state;
mod fault_state;
mod ipc_state;
mod memory_state;
mod restart_state;
mod scheduler_state;
mod task_policy_state;
mod thread_state;
mod user_memory_state;

use super::capabilities::{CNodeId, CapId, CapObject, CapRights, Capability, CapabilitySpace};
#[cfg(test)]
use super::ipc::EndpointMode;
use super::ipc::{Endpoint, IpcError, Message};
use super::scheduler::{CpuId, SchedulerError, SmpScheduler};
use super::scheduler_timer::Timer;
use super::smp::SmpMailbox;
#[cfg(test)]
use super::smp::WorkItem;
use super::syscall::SyscallError;
use super::task::{FaultPolicy, RobustFutexState, TaskClass, TaskStatus, ThreadControlBlock};
#[cfg(test)]
use super::task::{ThreadGroupId, UserRegisterContext, WaitReason};
use super::trap::FaultInfo;
#[cfg(test)]
use super::trap::{FaultAccess, Trap, TrapEvent};
#[cfg(test)]
use super::trapframe::TrapFrame;
use super::vm::{
    AddressSpace, AddressSpaceManager, Asid, Mapping, PageFlags, PhysAddr, VirtAddr, VmError,
};
use crate::arch::{platform_layout, topology};
use crate::kernel::ipc::ThreadId;
#[cfg(feature = "hosted-dev")]
use crate::std::collections::BTreeMap;

#[cfg(feature = "hosted-dev")]
type KernelStorage<T> = crate::std::boxed::Box<T>;
#[cfg(not(feature = "hosted-dev"))]
type KernelStorage<T> = T;

#[cfg(feature = "hosted-dev")]
fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    crate::std::boxed::Box::new(value)
}
#[cfg(not(feature = "hosted-dev"))]
fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    value
}

const MAX_ENDPOINTS: usize = 16;
const MAX_TASKS: usize = 64;
const MAX_MEMORY_OBJECTS: usize = 128;
const MAX_NOTIFICATIONS: usize = 16;
const MAX_IRQ_LINES: usize = platform_layout::MAX_IRQ_LINES;
const MAX_DRIVERS: usize = 32;
const MAX_TRANSFER_ENVELOPES: usize = 64;
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
struct MemoryObject {
    id: u64,
    phys: PhysAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NotificationObject {
    endpoint_idx: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrkRegionRecord {
    tid: ThreadId,
    base: VirtAddr,
    end: VirtAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RobustFutexRecord {
    tid: ThreadId,
    state: RobustFutexState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TransferEnvelope {
    capability: Capability,
    endpoint: CapObject,
    receiver_tid: Option<ThreadId>,
}

#[derive(Debug)]
struct IpcSubsystem {
    cross_cpu_work: SmpMailbox,
    endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
    endpoint_waiters: [Option<ThreadId>; MAX_ENDPOINTS],
    endpoint_sender_waiters: [Option<(ThreadId, Message, bool)>; MAX_ENDPOINTS],
    endpoint_generations: [u64; MAX_ENDPOINTS],
    notifications: [Option<NotificationObject>; MAX_NOTIFICATIONS],
    notification_generations: [u64; MAX_NOTIFICATIONS],
    irq_routes: [Option<usize>; MAX_IRQ_LINES],
    transfer_envelopes: [Option<TransferEnvelope>; MAX_TRANSFER_ENVELOPES],
    transfer_envelope_generations: [u64; MAX_TRANSFER_ENVELOPES],
    telemetry: IpcPathTelemetry,
}

#[cfg(feature = "hosted-dev")]
type UserMemoryStore = BTreeMap<(u16, u64), u8>;

#[derive(Debug)]
struct MemorySubsystem {
    #[cfg(feature = "hosted-dev")]
    user_memory: KernelStorage<UserMemoryStore>,
    memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],
    brk_regions: [Option<BrkRegionRecord>; MAX_TASKS],
    next_memory_object_id: u64,
    next_anon_phys: u64,
}

#[derive(Debug)]
struct DriverSubsystem {
    driver_records: [Option<DriverRecord>; MAX_DRIVERS],
    next_iova_space_id: u64,
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
}

#[derive(Debug)]
pub struct KernelState {
    pub kernel_aspace: AddressSpace,
    pub scheduler: KernelStorage<SmpScheduler>,
    pub cspace: KernelStorage<CapabilitySpace>,
    pub timer: Timer,
    pub user_spaces: AddressSpaceManager,
    current_cpu: CpuId,
    ipc: KernelStorage<IpcSubsystem>,
    next_dynamic_tid: u64,
    tcbs: [Option<ThreadControlBlock>; MAX_TASKS],
    tls_restore_pending: [Option<ThreadId>; MAX_TASKS],
    robust_futex: [Option<RobustFutexRecord>; MAX_TASKS],
    memory: KernelStorage<MemorySubsystem>,
    drivers: DriverSubsystem,
    tlb_shootdown_count: u64,
    faults: FaultSubsystem,
    restart: RestartSubsystem,
}

pub struct Bootstrap;

fn map_scheduler_error(err: SchedulerError) -> KernelError {
    match err {
        SchedulerError::QueueFull => KernelError::SchedulerFull,
        SchedulerError::InvalidCpu | SchedulerError::CpuOffline => KernelError::WrongObject,
        SchedulerError::AlreadyQueued => KernelError::WouldBlock,
    }
}

fn map_ipc_error(err: IpcError) -> KernelError {
    match err {
        IpcError::EndpointFull => KernelError::EndpointQueueFull,
        IpcError::PayloadTooLarge
        | IpcError::MissingCapTransferFlag
        | IpcError::InconsistentCapTransferFlag
        | IpcError::InvalidEndpointDepth => KernelError::WrongObject,
    }
}

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
        scheduler.set_present_cpu_bitmap(topology::default_present_cpu_bitmap());
        scheduler
            .enqueue_on(
                CpuId(platform_layout::BOOTSTRAP_CPU_ID),
                crate::kernel::ipc::ThreadId(0),
            )
            .map_err(map_scheduler_error)?;

        let mut cspace = CapabilitySpace::default();
        cspace
            .mint(Capability::new(CapObject::Kernel, CapRights::SCHEDULE))
            .map_err(|_| KernelError::CapabilityFull)?;

        let mut state = KernelState {
            kernel_aspace,
            scheduler: store_kernel_value(scheduler),
            cspace: store_kernel_value(cspace),
            timer: Timer::new(platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS),
            user_spaces: AddressSpaceManager::default(),
            current_cpu: CpuId(platform_layout::BOOTSTRAP_CPU_ID),
            ipc: store_kernel_value(IpcSubsystem {
                cross_cpu_work: SmpMailbox::default(),
                endpoints: [const { None }; MAX_ENDPOINTS],
                endpoint_waiters: [None; MAX_ENDPOINTS],
                endpoint_sender_waiters: [None; MAX_ENDPOINTS],
                endpoint_generations: [0; MAX_ENDPOINTS],
                notifications: [const { None }; MAX_NOTIFICATIONS],
                notification_generations: [0; MAX_NOTIFICATIONS],
                irq_routes: [None; MAX_IRQ_LINES],
                transfer_envelopes: [const { None }; MAX_TRANSFER_ENVELOPES],
                transfer_envelope_generations: [0; MAX_TRANSFER_ENVELOPES],
                telemetry: IpcPathTelemetry::default(),
            }),
            next_dynamic_tid: INITIAL_DYNAMIC_TID,
            tcbs: [const { None }; MAX_TASKS],
            tls_restore_pending: [None; MAX_TASKS],
            robust_futex: [None; MAX_TASKS],
            memory: store_kernel_value(MemorySubsystem {
                #[cfg(feature = "hosted-dev")]
                user_memory: store_kernel_value(UserMemoryStore::default()),
                memory_objects: [None; MAX_MEMORY_OBJECTS],
                brk_regions: [None; MAX_TASKS],
                next_memory_object_id: 1,
                next_anon_phys: platform_layout::NEXT_ANON_PHYS_BASE,
            }),
            drivers: DriverSubsystem {
                driver_records: [const { None }; MAX_DRIVERS],
                next_iova_space_id: 1,
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
            if self.current_tid() == Some(tid.0) {
                return Ok(true);
            }
            self.yield_current()?;
            spins += 1;
        }
        Ok(self.current_tid() == Some(tid.0))
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

    pub fn task_restart_token(&self, tid: u64) -> Option<u64> {
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .and_then(|tcb| tcb.restart.token.map(|token| token.0))
    }

    pub fn current_task_cnode(&self) -> Option<CNodeId> {
        let tid = self.current_tid()?;
        self.tcbs
            .iter()
            .flatten()
            .find(|tcb| tcb.tid.0 == tid)
            .map(|tcb| tcb.cnode)
    }

    pub fn current_task_capability(&self, cap: CapId) -> Option<Capability> {
        let _cnode = self.current_task_cnode();
        self.cspace.get(cap)
    }

    pub fn current_task_capability_has_right(&self, cap: CapId, right: CapRights) -> bool {
        self.current_task_capability(cap)
            .map(|capability| capability.has_right(right))
            .unwrap_or(false)
    }

    pub fn stash_transfer_envelope(
        &mut self,
        capability: Capability,
        endpoint: CapObject,
        receiver_tid: Option<ThreadId>,
    ) -> Option<u64> {
        for idx in 0..MAX_TRANSFER_ENVELOPES {
            if self.ipc.transfer_envelopes[idx].is_some() {
                continue;
            }
            let mut generation = self.ipc.transfer_envelope_generations[idx].wrapping_add(1);
            if generation == 0 {
                generation = 1;
            }
            self.ipc.transfer_envelope_generations[idx] = generation;
            self.ipc.transfer_envelopes[idx] = Some(TransferEnvelope {
                capability,
                endpoint,
                receiver_tid,
            });
            let idx_part = u64::try_from(idx).ok()?;
            return Some((generation << 16) | idx_part);
        }
        None
    }

    pub fn take_transfer_envelope(
        &mut self,
        handle: u64,
        endpoint: CapObject,
        receiver_tid: ThreadId,
    ) -> Option<Capability> {
        let idx = usize::try_from(handle & 0xFFFF).ok()?;
        if idx >= MAX_TRANSFER_ENVELOPES {
            return None;
        }
        let generation = handle >> 16;
        if generation == 0 || self.ipc.transfer_envelope_generations[idx] != generation {
            return None;
        }
        let envelope = self.ipc.transfer_envelopes[idx]?;
        if envelope.endpoint != endpoint {
            return None;
        }
        if let Some(bound_receiver) = envelope.receiver_tid {
            if bound_receiver != receiver_tid {
                return None;
            }
        }
        self.ipc.transfer_envelopes[idx] = None;
        Some(envelope.capability)
    }

    pub fn endpoint_waiter_tid(&self, endpoint: CapObject) -> Option<ThreadId> {
        let CapObject::Endpoint { index, generation } = endpoint else {
            return None;
        };
        if index >= MAX_ENDPOINTS {
            return None;
        }
        if self.ipc.endpoint_generations[index] != generation {
            return None;
        }
        self.ipc.endpoint_waiters[index]
    }

    pub fn capability_has_right(&self, cap: CapId, right: CapRights) -> bool {
        self.cspace.has_right(cap, right)
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
        if !capability.has_right(CapRights::RECEIVE) {
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
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        self.faults.supervisor_endpoint = Some(endpoint_idx);
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
        assert_eq!(state.current_tid().expect("boot task"), 0);
        assert_eq!(state.task_status(0), Some(TaskStatus::Running));
    }

    #[test]
    fn transfer_envelope_handles_are_single_use_and_replay_safe() {
        let mut state = Bootstrap::init().expect("init");
        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let (_eid, send_cap, _recv_cap) = state.create_endpoint(1).expect("endpoint");
        let payload = state.cspace.get(mem_cap).expect("payload");
        let endpoint = state.cspace.get(send_cap).expect("send cap").object;

        let first = state
            .stash_transfer_envelope(payload, endpoint, None)
            .expect("stash first");
        assert!(
            state
                .take_transfer_envelope(first, endpoint, ThreadId(0))
                .is_some()
        );
        assert!(
            state
                .take_transfer_envelope(first, endpoint, ThreadId(0))
                .is_none()
        );

        let second = state
            .stash_transfer_envelope(payload, endpoint, None)
            .expect("stash second");
        assert_ne!(first, second);
        assert!(
            state
                .take_transfer_envelope(first, endpoint, ThreadId(0))
                .is_none()
        );
        let wrong_endpoint = CapObject::Endpoint {
            index: usize::MAX,
            generation: 1,
        };
        assert!(
            state
                .take_transfer_envelope(second, wrong_endpoint, ThreadId(0))
                .is_none()
        );
        assert!(
            state
                .take_transfer_envelope(second, endpoint, ThreadId(0))
                .is_some()
        );

        let bound = state
            .stash_transfer_envelope(payload, endpoint, Some(ThreadId(9)))
            .expect("stash bound");
        assert!(
            state
                .take_transfer_envelope(bound, endpoint, ThreadId(8))
                .is_none()
        );
        assert!(
            state
                .take_transfer_envelope(bound, endpoint, ThreadId(9))
                .is_some()
        );
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
        assert_eq!(state.dispatch_next_current_cpu(), Some(42));
        assert_eq!(state.current_tid(), Some(42));
        assert_eq!(state.task_status(42), Some(TaskStatus::Runnable));
    }

    #[test]
    fn cross_cpu_work_queue_round_trip() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");
        state
            .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
            .expect("submit");

        state.set_current_cpu(CpuId(1)).expect("switch cpu1");

        assert_eq!(
            state.drain_cross_cpu_work().expect("drain"),
            Some(WorkItem::Reschedule)
        );
        assert_eq!(state.drain_cross_cpu_work().expect("drain"), None);
    }

    #[test]
    fn destroy_user_address_space_queues_shootdowns_and_retires_asid() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        let (asid, aspace_cap) = state.create_user_address_space().expect("asid");
        state
            .destroy_user_address_space(aspace_cap)
            .expect("destroy aspace");

        assert!(state.user_spaces.get(asid).is_none());
        assert_eq!(
            state
                .user_spaces
                .retired_entry(asid)
                .map(|entry| entry.pending_cpu_bitmap),
            Some(0b11)
        );

        let mut seen = [false; 2];
        for cpu in [CpuId(0), CpuId(1)] {
            state.set_current_cpu(cpu).expect("switch cpu");
            if let Some(WorkItem::TlbShootdown {
                asid: item_asid,
                va_range,
            }) = state.drain_cross_cpu_work().expect("drain")
            {
                assert_eq!(item_asid, asid);
                assert_eq!(va_range, None);
                seen[cpu.0 as usize] = true;
            }
        }
        assert_eq!(seen, [true, true]);

        state
            .submit_cross_cpu_work(
                CpuId(0),
                WorkItem::TlbShootdown {
                    asid,
                    va_range: None,
                },
            )
            .expect("requeue cpu0 shootdown");
        state
            .submit_cross_cpu_work(
                CpuId(1),
                WorkItem::TlbShootdown {
                    asid,
                    va_range: None,
                },
            )
            .expect("requeue cpu1 shootdown");

        state.set_current_cpu(CpuId(0)).expect("switch cpu0");
        state
            .process_cross_cpu_work_for_cpu(CpuId(0))
            .expect("process cpu0");
        assert_eq!(
            state
                .user_spaces
                .retired_entry(asid)
                .map(|entry| entry.pending_cpu_bitmap),
            Some(0b10)
        );

        state.set_current_cpu(CpuId(1)).expect("switch cpu1");
        state
            .process_cross_cpu_work_for_cpu(CpuId(1))
            .expect("process cpu1");
        assert_eq!(state.user_spaces.retired_entry(asid), None);
    }

    #[test]
    fn process_cross_cpu_work_applies_matching_cpu_items_only() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(2).expect("task2");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        state
            .submit_cross_cpu_work(CpuId(1), WorkItem::WakeTask { tid: ThreadId(2) })
            .expect("submit wake");
        state
            .submit_cross_cpu_work(
                CpuId(0),
                WorkItem::TlbShootdown {
                    asid: Asid(1),
                    va_range: None,
                },
            )
            .expect("submit tlb");

        let done = state
            .process_cross_cpu_work_for_cpu(CpuId(0))
            .expect("process cpu0");
        assert_eq!(done, 1);
        assert_eq!(state.tlb_shootdown_count(), 1);

        // WakeTask for cpu1 should still be queued.
        state.set_current_cpu(CpuId(1)).expect("switch cpu1");
        assert_eq!(
            state.drain_cross_cpu_work().expect("drain cpu1"),
            Some(WorkItem::WakeTask { tid: ThreadId(2) })
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
        state.enqueue_current_cpu(1).expect("queue task 1");

        let running_before = state.current_tid().expect("running");
        state
            .handle_trap(Trap::TimerInterrupt, None)
            .expect("timer trap should be handled");
        let running_after = state.current_tid().expect("running");

        assert_ne!(running_before, running_after);
        assert_eq!(state.task_status(running_after), Some(TaskStatus::Running));
    }

    #[test]
    fn normalized_page_fault_event_faults_current_task() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue task1");

        state
            .handle_trap_event(
                TrapEvent::PageFault(FaultInfo {
                    addr: VirtAddr(0x1200),
                    access: super::super::trap::FaultAccess::Read,
                }),
                None,
            )
            .expect("page fault event handled");

        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.current_tid(), Some(1));
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
        state.enqueue_current_cpu(1).expect("queue task 1");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        assert_eq!(state.current_tid(), Some(0));
        let first_try = state.ipc_recv(recv_cap).expect("recv call should not fail");
        assert!(first_try.is_none());
        assert_eq!(
            state.task_status(0),
            Some(TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap)))
        );
        assert_eq!(state.current_tid(), Some(1));

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
        state.enqueue_current_cpu(1).expect("queue task 1");
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
        assert_eq!(state.current_tid(), Some(1));

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
            .mint_derived(send_cap, CapRights::SEND)
            .expect("derive");
        let msg = Message::new(9, b"ok").expect("msg");
        assert!(state.ipc_send(child, msg).is_ok());

        assert_eq!(state.cspace.revoke(child), Ok(()));
        let msg2 = Message::new(9, b"no").expect("msg");
        assert_eq!(
            state.ipc_send(child, msg2),
            Err(KernelError::InvalidCapability)
        );
    }

    #[test]
    fn ipc_message_header_and_cap_transfer_metadata_are_preserved() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state.create_memory_object(PhysAddr(0xC000)).expect("mem");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert_eq!(state.ipc_recv(recv_cap).expect("block recv"), None);
        assert_eq!(state.current_tid(), Some(0));

        state
            .ipc_send_with_cap_transfer(send_cap, ThreadId(0), 0x55, mem_cap, b"mt")
            .expect("send transfer");
        state.yield_current().expect("switch receiver");
        assert_eq!(state.current_tid(), Some(1));
        let msg = state.ipc_recv(recv_cap).expect("recv").expect("message");

        assert_eq!(msg.opcode, 0x55);
        assert_eq!(
            msg.flags & Message::FLAG_CAP_TRANSFER,
            Message::FLAG_CAP_TRANSFER
        );
        assert_ne!(msg.transferred_cap().map(|cap| cap.0), Some(mem_cap.0));
        assert_eq!(msg.as_slice(), b"mt");
    }

    #[test]
    fn syscall_trap_dispatches_ipc_send_recv() {
        let mut state = Bootstrap::init().expect("init");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");

        let send_payload = usize::from_le_bytes([b'h', b'i', 0, 0, 0, 0, 0, 0]);
        let mut send_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcSend as usize,
            [
                send_cap.0 as usize,
                42,
                2,
                send_payload,
                0,
                crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP as usize,
            ],
        );

        state
            .handle_trap(Trap::Syscall, Some(&mut send_frame))
            .expect("syscall send");
        assert_eq!(send_frame.error_code(), None);

        let mut recv_frame = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 0, 0, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_frame))
            .expect("syscall recv");
        assert_eq!(recv_frame.error_code(), None);
        assert_eq!(recv_frame.ret0() as u64, 0);
        assert_eq!(recv_frame.ret1(), 2);
        assert_eq!(recv_frame.arg(3) & 0xFF, b'h' as usize);
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
            .mint_derived(aspace_map_cap, CapRights::READ)
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
            .mint_derived(mem_cap, CapRights::READ)
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
            [
                send_cap.0 as usize,
                0,
                2,
                0,
                0,
                crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP as usize,
            ],
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
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        let (asid, _aspace_map_cap) = state.create_user_address_space().expect("asid");
        state.bind_task_asid(0, asid).expect("bind");
        let (_eid, send_cap, recv_cap) = state.create_endpoint(2).expect("endpoint");
        let (_mem_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        state.yield_current().expect("switch to task1");
        assert_eq!(state.current_tid(), Some(1));
        assert_eq!(state.ipc_recv(recv_cap).expect("block recv"), None);
        assert_eq!(state.current_tid(), Some(0));

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

        state.yield_current().expect("switch receiver");
        assert_eq!(state.current_tid(), Some(1));
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

        assert_eq!(recv_frame.error_code(), None);
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

        assert_eq!(
            recv_frame.error_code(),
            Some(SyscallError::PageFault.code())
        );
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
        state.enqueue_current_cpu(1).expect("enqueue task1");

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

        assert_eq!(
            recv_frame.error_code(),
            Some(SyscallError::PageFault.code())
        );
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.current_tid(), Some(1));
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
        state.enqueue_current_cpu(1).expect("enqueue task1");

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

        assert_eq!(
            recv_frame.error_code(),
            Some(SyscallError::PageFault.code())
        );
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.current_tid(), Some(1));

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
        state.enqueue_current_cpu(1).expect("enqueue task1");
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

        assert_eq!(
            recv_frame.error_code(),
            Some(SyscallError::PageFault.code())
        );
        assert_eq!(state.task_status(0), Some(TaskStatus::Running));
        assert_eq!(state.current_tid(), Some(0));

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
        state.enqueue_current_cpu(1).expect("enqueue task1");

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

        assert_eq!(
            recv_frame.error_code(),
            Some(SyscallError::PageFault.code())
        );
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
        assert_eq!(state.current_tid(), Some(1));
    }

    #[test]
    fn notification_irq_route_delivers_message_to_bound_endpoint() {
        let mut state = Bootstrap::init().expect("init");
        let (_notif_idx, notif_cap, notif_recv_cap) = state.create_notification(4).expect("notif");
        state.bind_irq_notification(11, notif_cap).expect("bind");

        state
            .handle_trap_event(TrapEvent::ExternalInterrupt(11), None)
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
    fn ipc_fastpath_telemetry_distinguishes_switch_and_queue_paths() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(60).expect("sender");
        state.register_task(61).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(2, EndpointMode::Synchronous)
            .expect("endpoint");

        state.enqueue_current_cpu(61).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x9000, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.enqueue_current_cpu(60).expect("enqueue sender");
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
    fn rendezvous_delivery_is_single_copy_and_no_sender_stuck() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(80).expect("sender");
        state.register_task(81).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        state.enqueue_current_cpu(81).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x1100, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.enqueue_current_cpu(80).expect("enqueue sender");
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
    fn ipc_send_fastpath_detects_waiter() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(35).expect("sender");
        state.register_task(36).expect("receiver");

        let (_eid, send_cap, recv_cap) = state
            .create_endpoint_with_mode(1, EndpointMode::Synchronous)
            .expect("endpoint");

        state.enqueue_current_cpu(36).expect("enqueue receiver");
        let mut recv_tf = TrapFrame::new(
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            [recv_cap.0 as usize, 8, 0x7000, 0, 0, 0],
        );
        state
            .handle_trap(Trap::Syscall, Some(&mut recv_tf))
            .expect("recv trap");

        state.enqueue_current_cpu(35).expect("enqueue sender");
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
            .report_task_exit_to_supervisor(7, 99, 55)
            .expect("report exit");

        let msg = state.ipc_recv(recv_cap).expect("recv").expect("msg");
        assert_eq!(msg.opcode, 0xEE);
        assert_eq!(msg.as_slice().len(), 24);
        let event =
            crate::kernel::supervisor_abi::TaskExitedEvent::decode(msg.as_slice()).expect("event");
        assert_eq!(event.tid, 7);
        assert_eq!(event.exit_code, 99);
        assert_eq!(event.restart_token, 55);
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
                .enqueue_on_cpu(CpuId((i % 2) as u8), i)
                .expect("enqueue");
        }

        for _ in 0..8 {
            state
                .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
                .expect("work");
        }
        state
            .process_cross_cpu_work_for_cpu(CpuId(1))
            .expect("process");

        for _ in 0..5 {
            state
                .handle_trap_event(TrapEvent::ExternalInterrupt(5), None)
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

        let (_id, mem_cap) = state.alloc_anonymous_memory_object().expect("mem");
        let iova_cap = state.create_iova_space_cap().expect("iova");

        let first_bundle = state
            .delegate_driver_bundle(DriverBundlePlan::standard(
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
            .delegate_driver_bundle(DriverBundlePlan::standard(
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
                .enqueue_on_cpu(CpuId((i % 2) as u8), i)
                .expect("enqueue");
        }

        let mut seed = 0x1234_5678u64;
        for _ in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            match seed % 3 {
                0 => state
                    .submit_cross_cpu_work(CpuId((seed as u8) % 2), WorkItem::Reschedule)
                    .expect("work"),
                1 => {
                    if state
                        .handle_trap_event(TrapEvent::ExternalInterrupt(7), None)
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
        state.enqueue_current_cpu(40).expect("enqueue");

        assert_eq!(state.current_tid(), Some(0));
        state.yield_current().expect("yield");

        assert_eq!(state.current_tid(), Some(40));
        assert_eq!(state.task_status(40), Some(TaskStatus::Running));
        assert_eq!(state.task_status(0), Some(TaskStatus::Runnable));
    }

    #[test]
    fn trap_event_page_fault_records_fault_then_faults_current_task() {
        let mut state = Bootstrap::init().expect("init");
        let fault = FaultInfo {
            addr: VirtAddr(0x4000),
            access: FaultAccess::Execute,
        };

        state
            .handle_trap_event(TrapEvent::PageFault(fault), None)
            .expect("handle page fault");

        assert_eq!(state.last_fault(), Some(fault));
        assert_eq!(state.task_status(0), Some(TaskStatus::Faulted));
    }

    #[test]
    fn cross_cpu_work_for_other_cpu_is_deferred_not_dropped() {
        let mut state = Bootstrap::init().expect("init");
        state.bring_up_cpu(CpuId(1)).expect("cpu1");

        state
            .submit_cross_cpu_work(CpuId(1), WorkItem::Reschedule)
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

        assert_eq!(state.thread_group_id(tid), Some(ThreadGroupId(7)));
        assert_eq!(state.task_asid(tid), Some(asid));
        assert_eq!(state.thread_tls_base(tid), Some(0xDEAD_BEEF));
        assert_eq!(state.task_status(tid), Some(TaskStatus::Runnable));
    }

    #[test]
    fn futex_wait_blocks_current_and_wake_requeues_waiter() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(1).expect("task1");
        state.enqueue_current_cpu(1).expect("enqueue");
        state.dispatch_next_task().expect("dispatch");
        state.yield_current().expect("switch");
        assert_eq!(state.current_tid(), Some(1));

        assert!(state.futex_wait_current(0x1000, 3, 3).expect("wait"));
        assert_eq!(
            state.task_status(1),
            Some(TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000))))
        );
        assert_eq!(state.futex_wake(0x1000, 1).expect("wake"), 1);
        assert_eq!(state.task_status(1), Some(TaskStatus::Runnable));
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
        assert_eq!(state.current_tid(), Some(tid));

        let mut frame = TrapFrame::new(0, [11, 22, 0, 0, 0, 0]);
        let tls = state
            .resume_current_thread_with_frame(&mut frame)
            .expect("resume");
        assert_eq!(tls, Some(0xABCD_0000));
        assert_eq!(frame.saved_pc(), 0x7010);
        assert_eq!(frame.saved_sp(), 0x8800_0000);

        frame.set_saved_pc(0x9000);
        frame.set_saved_sp(0x9900_0000);
        frame.set_arg(0, 33);
        frame.set_arg(1, 44);
        state
            .sync_current_thread_from_frame(&frame)
            .expect("capture");
        assert_eq!(
            state.thread_user_context(tid),
            Some(UserRegisterContext {
                instruction_ptr: VirtAddr(0x9000),
                stack_ptr: VirtAddr(0x9900_0000),
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
        assert_eq!(state.current_tid(), Some(joiner));

        assert_eq!(state.join_thread(30).expect("join pending"), None);
        assert_eq!(
            state.task_status(joiner),
            Some(TaskStatus::Blocked(WaitReason::Join(ThreadId(30))))
        );

        state.exit_task(30, 5).expect("exit leader");
        assert_eq!(state.task_status(joiner), Some(TaskStatus::Runnable));

        state.mark_thread_detached(joiner).expect("detach");
        state.exit_task(joiner, 9).expect("exit detached");
        assert_eq!(state.task_status(joiner), Some(TaskStatus::Dead));
    }
}
