// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;

#[cfg(feature = "hosted-dev")]
pub(crate) type KernelStorage<T> = alloc::boxed::Box<T>;
#[cfg(not(feature = "hosted-dev"))]
pub(crate) type KernelStorage<T> = T;

#[cfg(feature = "hosted-dev")]
pub(crate) fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    alloc::boxed::Box::new(value)
}
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn store_kernel_value<T>(value: T) -> KernelStorage<T> {
    value
}

#[cfg(feature = "hosted-dev")]
pub(crate) fn kernel_ref<T>(value: &KernelStorage<T>) -> &T {
    value.as_ref()
}

#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn kernel_ref<T>(value: &KernelStorage<T>) -> &T {
    value
}

#[cfg(feature = "hosted-dev")]
pub(crate) fn kernel_mut<T>(value: &mut KernelStorage<T>) -> &mut T {
    value.as_mut()
}

#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn kernel_mut<T>(value: &mut KernelStorage<T>) -> &mut T {
    value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryObject {
    pub(crate) id: u64,
    pub(crate) phys: PhysAddr,
    pub(crate) len: usize,
    pub(crate) cap_refcount: u32,
    pub(crate) map_refcount: u32,
    pub(crate) pin_refcount: u32,
}

#[derive(Debug)]
pub(crate) struct NotificationObject {
    pub(crate) irq_queue: [u16; crate::kernel::ipc::MAX_ENDPOINT_DEPTH],
    pub(crate) head: usize,
    pub(crate) len: usize,
    pub(crate) max_depth: usize,
}

impl NotificationObject {
    pub(crate) fn new(max_depth: usize) -> Result<Self, KernelError> {
        if max_depth == 0 || max_depth > crate::kernel::ipc::MAX_ENDPOINT_DEPTH {
            return Err(KernelError::WrongObject);
        }
        Ok(Self {
            irq_queue: [0; crate::kernel::ipc::MAX_ENDPOINT_DEPTH],
            head: 0,
            len: 0,
            max_depth,
        })
    }

    pub(crate) fn send_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        if self.len >= self.max_depth {
            return Err(KernelError::EndpointQueueFull);
        }
        let tail = (self.head + self.len) & (crate::kernel::ipc::MAX_ENDPOINT_DEPTH - 1);
        self.irq_queue[tail] = irq_line;
        self.len += 1;
        Ok(())
    }

    pub(crate) fn recv(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }
        let irq_line = self.irq_queue[self.head];
        self.head = (self.head + 1) & (crate::kernel::ipc::MAX_ENDPOINT_DEPTH - 1);
        self.len -= 1;
        let payload = irq_line.to_le_bytes();
        Message::with_header(0, irq_line, 0, None, &payload).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DriverRecord {
    pub(crate) tid: ThreadId,
    pub(crate) irq_caps: [Option<CapId>; MAX_DRIVER_IRQ_CAPS],
    pub(crate) dma_caps: [Option<CapId>; MAX_DRIVER_DMA_CAPS],
    pub(crate) dma_iova_base: Option<usize>,
    pub(crate) dma_iova_len: Option<usize>,
    pub(crate) iova_space_cap: Option<CapId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrkRegionRecord {
    pub(crate) tid: ThreadId,
    pub(crate) base: VirtAddr,
    pub(crate) end: VirtAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CowPageRecord {
    pub(crate) asid: Asid,
    pub(crate) virt: VirtAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RobustFutexRecord {
    pub(crate) tid: ThreadId,
    pub(crate) state: RobustFutexState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferEnvelope {
    pub(crate) source_tid: ThreadId,
    pub(crate) source_cap: CapId,
    pub(crate) source_object: CapObject,
    pub(crate) endpoint: CapObject,
    pub(crate) receiver_tid: Option<ThreadId>,
    pub(crate) state: TransferState,
    pub(crate) shared_region: Option<TransferSharedRegion>,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferSharedRegion {
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TransferState {
    Created,
    MappedReceiver,
    MappedBoth,
    Released,
    Revoked,
}

impl TransferEnvelope {
    pub(crate) fn transition(self, next: TransferState) -> Option<Self> {
        use TransferState::*;
        let legal = matches!(
            (self.state, next),
            (Created, MappedReceiver)
                | (Created, Released)
                | (Created, Revoked)
                | (MappedReceiver, MappedBoth)
                | (MappedReceiver, Released)
                | (MappedReceiver, Revoked)
                | (MappedBoth, Released)
                | (MappedBoth, Revoked)
                | (Released, Revoked)
        );
        if !legal {
            return None;
        }
        Some(Self {
            state: next,
            ..self
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SenderWaiter {
    pub(crate) tid: ThreadId,
    pub(crate) msg: Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveTransferMapping {
    pub(crate) owner_tid: ThreadId,
    pub(crate) transfer_cap: CapId,
    pub(crate) base: VirtAddr,
    pub(crate) len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyCapRecord {
    pub(crate) caller_tid: ThreadId,
    pub(crate) reply_endpoint: CapObject,
    pub(crate) responder_tid: Option<ThreadId>,
    /// CapId of the Reply cap that `create_reply_cap_for_caller` minted into the
    /// **caller's** cnode.  Stored here so that `ipc_reply` (which runs in the
    /// **replier's** context) can also revoke it from the caller's cnode, preventing
    /// cnode slot exhaustion on the caller side over many repeated IPC cycles.
    pub(crate) caller_cap_id: CapId,
    /// CapId of the Reply cap that `complete_blocked_recv_for_waiter` (or the
    /// immediate recv path) minted into the **waiter/replier's** cnode when the
    /// FLAG_REPLY_CAP message was delivered.  Stored here so that `ipc_reply`
    /// can fast-revoke the exact slot using a kernel-controlled CapId rather
    /// than relying solely on the user-supplied reply_cap argument.
    ///
    /// `None` if materialization has not yet occurred (e.g. the message is still
    /// queued in the endpoint buffer and the receiver has not yet called recv).
    pub(crate) waiter_cap_id: Option<CapId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTlbShootdownWait {
    pub(crate) sequence: u64,
    pub(crate) pending_cpu_bitmap: u64,
    pub(crate) requester_cpu: CpuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTlbShootdownState {
    pub(crate) next_sequence: u64,
    pub(crate) active: Option<LiveTlbShootdownWait>,
}

#[derive(Debug)]
pub(crate) struct IpcSubsystem {
    pub(crate) cross_cpu_work: SmpMailbox,
    pub(crate) live_tlb_shootdown: LiveTlbShootdownState,
    pub(crate) endpoints: [Option<KernelStorage<Endpoint>>; MAX_ENDPOINTS],
    pub(crate) endpoint_waiters: [Option<ThreadId>; MAX_ENDPOINTS],
    pub(crate) endpoint_sender_waiters:
        [[Option<SenderWaiter>; MAX_ENDPOINT_SENDER_WAITERS]; MAX_ENDPOINTS],
    pub(crate) endpoint_generations: [u64; MAX_ENDPOINTS],
    pub(crate) notifications: [Option<NotificationObject>; MAX_NOTIFICATIONS],
    pub(crate) notification_waiters: [Option<ThreadId>; MAX_NOTIFICATIONS],
    pub(crate) notification_generations: [u64; MAX_NOTIFICATIONS],
    pub(crate) irq_routes: [Option<usize>; MAX_IRQ_LINES],
    pub(crate) transfer_envelopes: [Option<TransferEnvelope>; MAX_TRANSFER_ENVELOPES],
    pub(crate) transfer_envelope_generations: [u64; MAX_TRANSFER_ENVELOPES],
    pub(crate) active_transfer_mappings: [Option<ActiveTransferMapping>; MAX_TRANSFER_ENVELOPES],
    pub(crate) reply_caps: [Option<ReplyCapRecord>; MAX_REPLY_CAPS],
    pub(crate) reply_cap_generations: [u64; MAX_REPLY_CAPS],
    pub(crate) telemetry: IpcPathTelemetry,
}

#[cfg(feature = "hosted-dev")]
pub(crate) type UserMemoryStore = BTreeMap<(u16, u64), u8>;

#[derive(Debug)]
pub(crate) struct MemorySubsystem {
    #[cfg(feature = "hosted-dev")]
    pub(crate) user_memory: KernelStorage<UserMemoryStore>,
    pub(crate) memory_objects: [Option<MemoryObject>; MAX_MEMORY_OBJECTS],
    pub(crate) brk_regions: [Option<BrkRegionRecord>; MAX_TASKS],
    pub(crate) cow_pages: KernelStorage<[Option<CowPageRecord>; MAX_COW_PAGES]>,
    pub(crate) next_memory_object_id: u64,
    pub(crate) frame_allocator: KernelStorage<PhysicalFrameAllocator>,
}

#[derive(Debug)]
pub(crate) struct DriverSubsystem {
    pub(crate) driver_records: [Option<DriverRecord>; MAX_DRIVERS],
    pub(crate) next_iova_space_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CNodeSpace {
    pub(crate) id: CNodeId,
    pub(crate) slot_capacity: usize,
    pub(crate) cspace: KernelStorage<CapabilitySpace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessCNodeRecord {
    pub(crate) pid: u64,
    pub(crate) cnode: CNodeId,
}

#[derive(Debug)]
pub(crate) struct FaultSubsystem {
    pub(crate) last_fault: Option<FaultInfo>,
    pub(crate) last_fault_frame: Option<TrapFrame>,
    pub(crate) fault_handler_endpoint: Option<usize>,
    pub(crate) supervisor_endpoint: Option<usize>,
    pub(crate) fault_policy: FaultPolicy,
}

#[derive(Debug)]
pub(crate) struct RestartSubsystem {
    pub(crate) next_restart_token: u64,
}

#[derive(Debug)]
pub(crate) struct CapabilitySubsystem {
    pub(crate) cnode_spaces: KernelStorage<[Option<CNodeSpace>; MAX_TASKS]>,
    pub(crate) process_cnodes: KernelStorage<[Option<ProcessCNodeRecord>; MAX_TASKS]>,
    pub(crate) delegated_capability_links:
        KernelStorage<[Option<DelegatedCapabilityLink>; MAX_DELEGATED_CAPABILITY_LINKS]>,
}

#[derive(Debug)]
pub(crate) struct TelemetrySubsystem {
    pub(crate) tlb_shootdown_count: u64,
    pub(crate) tlb_shootdown_timeout_count: u64,
    pub(crate) tid_allocation: TidAllocationTelemetry,
}

#[derive(Debug)]
pub(crate) struct BootConfigSubsystem {
    pub(crate) capacity_profile: KernelCapacityProfile,
}

#[derive(Debug)]
pub(crate) struct SchedulerState {
    pub(crate) scheduler: KernelStorage<SmpScheduler>,
    pub(crate) timer: Timer,
    pub(crate) current_cpu: CpuId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegatedCapabilityLink {
    pub(crate) source_tid: u64,
    pub(crate) source_cap: CapId,
    pub(crate) dest_tid: u64,
    pub(crate) dest_cap: CapId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegatedCapRef {
    pub(crate) pid: u64,
    pub(crate) cap: CapId,
}

pub(crate) fn map_scheduler_error(err: SchedulerError) -> KernelError {
    match err {
        SchedulerError::QueueFull => KernelError::SchedulerFull,
        SchedulerError::InvalidCpu | SchedulerError::CpuOffline => KernelError::WrongObject,
        SchedulerError::AlreadyQueued => KernelError::WouldBlock,
    }
}

pub(crate) fn map_ipc_error(err: IpcError) -> KernelError {
    match err {
        IpcError::EndpointFull => KernelError::EndpointQueueFull,
        IpcError::PayloadTooLarge
        | IpcError::MissingCapTransferFlag
        | IpcError::InconsistentCapTransferFlag
        | IpcError::InvalidEndpointDepth => KernelError::WrongObject,
    }
}
