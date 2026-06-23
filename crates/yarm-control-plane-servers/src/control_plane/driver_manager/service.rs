// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(test, feature = "legacy-tests"))]
use yarm::kernel::boot::{KernelError, KernelState};
use yarm_ipc_abi::driver_abi::{
    DRIVER_OP_GRANT_DMA, DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, DRIVER_OP_RESTARTED,
};
use yarm_user_rt::capability::CapId;
use yarm_user_rt::ipc::Message;
use yarm_user_rt::runtime::{DriverControlOps, KernelIpcError};

// TODO: MMIO/IOPORT bind opcodes (future)
// TODO: Device enumeration opcode (future)
// TODO: Heartbeat / watchdog opcode (future)
// Userspace-only, driver_manager-local query opcodes. These are not syscall or
// global IPC ABI additions; they are inert data replies for hosted scaffolding
// and future driver-manager protocol work.
pub const DRIVER_OP_QUERY_MY_DEVICE: u16 = 0x4450;
pub const DRIVER_OP_QUERY_MY_MMIO: u16 = 0x4451;
pub const DRIVER_OP_QUERY_MY_IRQS: u16 = 0x4452;
pub const DRIVER_OP_QUERY_MY_CANDIDATE: u16 = 0x4453;
pub const DRIVER_OP_QUERY_MY_DMA: u16 = 0x4454;

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, KernelIpcError> {
    let end = offset.checked_add(8).ok_or(KernelIpcError::WrongObject)?;
    let bytes = payload
        .get(offset..end)
        .ok_or(KernelIpcError::WrongObject)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(arr))
}

fn read_u16_checked(payload: &[u8], offset: usize) -> Result<u16, KernelIpcError> {
    u16::try_from(read_u64(payload, offset)?).map_err(|_| KernelIpcError::WrongObject)
}

fn read_usize_checked(payload: &[u8], offset: usize) -> Result<usize, KernelIpcError> {
    usize::try_from(read_u64(payload, offset)?).map_err(|_| KernelIpcError::WrongObject)
}

fn ok_reply(
    opcode: u16,
    value: u64,
    transferred_cap: Option<CapId>,
) -> Result<Message, KernelIpcError> {
    let payload = value.to_le_bytes();
    let (flags, cap) = if let Some(cap_id) = transferred_cap {
        if cap_id.0 == 0 {
            return Err(KernelIpcError::InvalidCapability);
        }
        (Message::FLAG_CAP_TRANSFER, Some(cap_id.0))
    } else {
        (0, None)
    };
    Message::with_header(0, opcode, flags, cap, &payload).map_err(|_| KernelIpcError::WrongObject)
}

fn inert_reply(opcode: u16, payload: &[u8]) -> Result<Message, KernelIpcError> {
    Message::with_header(0, opcode, 0, None, payload).map_err(|_| KernelIpcError::WrongObject)
}

// ---------------------------------------------------------------------------
// Driver registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverClass {
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverLiveness {
    Alive,
}

#[derive(Debug, Clone, Copy)]
pub struct DriverRecord {
    pub tid: u64,
    /// Fixed-capacity name stored as UTF-8 bytes; unused bytes are zero.
    pub name: [u8; 32],
    pub name_len: usize,
    pub class: DriverClass,
    pub abi_version: u32,
    pub liveness: DriverLiveness,
}

impl DriverRecord {
    const fn new(tid: u64) -> Self {
        Self {
            tid,
            name: [0u8; 32],
            name_len: 0,
            class: DriverClass::Unknown,
            abi_version: 0,
            liveness: DriverLiveness::Alive,
        }
    }
}

const MAX_DRIVERS: usize = 64;

#[derive(Debug)]
pub struct DriverRegistry {
    records: [Option<DriverRecord>; MAX_DRIVERS],
    len: usize,
}

impl DriverRegistry {
    pub const fn new() -> Self {
        Self {
            records: [None; MAX_DRIVERS],
            len: 0,
        }
    }

    /// Register a driver by verified sender tid.
    ///
    /// The table is append-only: no remove path exists, so `records[..len]` is
    /// always densely occupied. Restart handling must update only an existing
    /// verified-sender record and must not append a replacement record.
    pub fn register(&mut self, tid: u64) -> Result<(), KernelIpcError> {
        if tid == 0 {
            return Err(KernelIpcError::MissingRight);
        }
        // Duplicate: return Ok without creating a second entry.
        if self.records[..self.len]
            .iter()
            .any(|r| r.map(|rec| rec.tid == tid).unwrap_or(false))
        {
            return Ok(());
        }
        if self.len >= MAX_DRIVERS {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.records[self.len] = Some(DriverRecord::new(tid));
        self.len += 1;
        Ok(())
    }

    pub fn note_restarted(&mut self, tid: u64) -> Result<(), KernelIpcError> {
        let Some(record) = self.records[..self.len]
            .iter_mut()
            .filter_map(|record| record.as_mut())
            .find(|record| record.tid == tid)
        else {
            return Err(KernelIpcError::TaskMissing);
        };
        record.liveness = DriverLiveness::Alive;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn get(&self, tid: u64) -> Option<&DriverRecord> {
        self.records[..self.len]
            .iter()
            .filter_map(|r| r.as_ref())
            .find(|r| r.tid == tid)
    }
}

// ---------------------------------------------------------------------------
// Platform inventory model (userspace-only, no DTB parsing or spawning)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    Uart,
    Mailbox,
    Gpio,
    IrqMux,
    Block,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    Discovered,
    DeferredNoMmioGrant,
    DeferredNoIrqRoute,
    DeferredNoHardwareControl,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRange {
    pub base: u64,
    pub len: u64,
}

impl MmioRange {
    pub fn new(base: u64, len: u64) -> Result<Self, KernelIpcError> {
        if len == 0 || base.checked_add(len - 1).is_none() {
            return Err(KernelIpcError::WrongObject);
        }
        Ok(Self { base, len })
    }

    pub fn contains_exact(&self, base: u64, len: u64) -> bool {
        self.base == base && self.len == len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRecord {
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub class: DeviceClass,
    pub mmio_ranges: [Option<MmioRange>; 4],
    pub irq_lines: [Option<u32>; 8],
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub status: DeviceStatus,
    pub assigned_tid: Option<u64>,
}

impl DeviceRecord {
    pub fn new(
        compatible: &str,
        class: DeviceClass,
        driver_candidate: &str,
        status: DeviceStatus,
    ) -> Result<Self, KernelIpcError> {
        if compatible.is_empty() || compatible.len() > 64 || driver_candidate.len() > 32 {
            return Err(KernelIpcError::WrongObject);
        }
        let mut record = Self {
            compatible: [0; 64],
            compatible_len: compatible.len(),
            class,
            mmio_ranges: [None; 4],
            irq_lines: [None; 8],
            driver_candidate: [0; 32],
            driver_candidate_len: driver_candidate.len(),
            status,
            assigned_tid: None,
        };
        record.compatible[..compatible.len()].copy_from_slice(compatible.as_bytes());
        record.driver_candidate[..driver_candidate.len()]
            .copy_from_slice(driver_candidate.as_bytes());
        Ok(record)
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn driver_candidate(&self) -> Option<&str> {
        bounded_str(&self.driver_candidate, self.driver_candidate_len)
    }

    pub fn with_mmio(mut self, index: usize, base: u64, len: u64) -> Result<Self, KernelIpcError> {
        if index >= self.mmio_ranges.len() {
            return Err(KernelIpcError::WrongObject);
        }
        self.mmio_ranges[index] = Some(MmioRange::new(base, len)?);
        Ok(self)
    }

    pub fn with_irq(mut self, index: usize, line: u32) -> Result<Self, KernelIpcError> {
        if index >= self.irq_lines.len() {
            return Err(KernelIpcError::WrongObject);
        }
        self.irq_lines[index] = Some(line);
        Ok(self)
    }

    pub fn assigned_to(mut self, tid: u64) -> Result<Self, KernelIpcError> {
        if tid == 0 {
            return Err(KernelIpcError::MissingRight);
        }
        self.assigned_tid = Some(tid);
        Ok(self)
    }

    fn is_live_grantable(&self) -> bool {
        matches!(self.status, DeviceStatus::Discovered)
    }
}

fn device_class_code(class: DeviceClass) -> u32 {
    match class {
        DeviceClass::Uart => 1,
        DeviceClass::Mailbox => 2,
        DeviceClass::Gpio => 3,
        DeviceClass::IrqMux => 4,
        DeviceClass::Block => 5,
        DeviceClass::Unknown => 0,
    }
}

fn device_status_code(status: DeviceStatus) -> u32 {
    match status {
        DeviceStatus::Discovered => 1,
        DeviceStatus::DeferredNoMmioGrant => 2,
        DeviceStatus::DeferredNoIrqRoute => 3,
        DeviceStatus::DeferredNoHardwareControl => 4,
        DeviceStatus::Unsupported => 0,
    }
}

fn bounded_str(bytes: &[u8], len: usize) -> Option<&str> {
    bytes
        .get(..len)
        .and_then(|slice| core::str::from_utf8(slice).ok())
}

const MAX_DEVICES: usize = 32;

#[derive(Debug)]
pub struct PlatformInventory {
    devices: [Option<DeviceRecord>; MAX_DEVICES],
    len: usize,
}

impl PlatformInventory {
    pub const fn new() -> Self {
        Self {
            devices: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub fn add(&mut self, record: DeviceRecord) -> Result<(), KernelIpcError> {
        if record.compatible_len == 0 || record.driver_candidate_len == 0 {
            return Err(KernelIpcError::WrongObject);
        }
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.devices[self.len] = Some(record);
        self.len += 1;
        Ok(())
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &DeviceRecord> {
        self.devices[..self.len]
            .iter()
            .filter_map(|record| record.as_ref())
    }

    pub fn candidates_for(&self, class: DeviceClass) -> impl Iterator<Item = &DeviceRecord> {
        self.iter().filter(move |record| record.class == class)
    }

    pub fn assigned_device_for(&self, tid: u64) -> Option<&DeviceRecord> {
        self.iter().find(|record| record.assigned_tid == Some(tid))
    }

    pub fn authorize_irq(&self, tid: u64, line: u16) -> Result<(), KernelIpcError> {
        let device = self
            .assigned_device_for(tid)
            .ok_or(KernelIpcError::MissingRight)?;
        if !device.is_live_grantable() {
            return Err(KernelIpcError::MissingRight);
        }
        if device
            .irq_lines
            .iter()
            .any(|irq| irq == &Some(u32::from(line)))
        {
            Ok(())
        } else {
            Err(KernelIpcError::MissingRight)
        }
    }

    pub fn authorize_mmio(&self, tid: u64, base: u64, len: u64) -> Result<(), KernelIpcError> {
        let device = self
            .assigned_device_for(tid)
            .ok_or(KernelIpcError::MissingRight)?;
        if !device.is_live_grantable() {
            return Err(KernelIpcError::MissingRight);
        }
        if device.mmio_ranges.iter().any(|range| {
            range
                .map(|range| range.contains_exact(base, len))
                .unwrap_or(false)
        }) {
            Ok(())
        } else {
            Err(KernelIpcError::MissingRight)
        }
    }

    pub fn authorize_dma(&self, tid: u64) -> Result<(), KernelIpcError> {
        let device = self
            .assigned_device_for(tid)
            .ok_or(KernelIpcError::MissingRight)?;
        if device.is_live_grantable() && !matches!(device.class, DeviceClass::Unknown) {
            Ok(())
        } else {
            Err(KernelIpcError::MissingRight)
        }
    }

    pub fn query_assigned_device(&self, tid: u64) -> Result<&DeviceRecord, KernelIpcError> {
        self.assigned_device_for(tid)
            .ok_or(KernelIpcError::TaskMissing)
    }
}

// ---------------------------------------------------------------------------
// KernelDriverControl (test-only runtime adapter)
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[cfg(all(test, feature = "legacy-tests"))]
pub struct KernelDriverControl<'a> {
    kernel: &'a mut KernelState,
}

#[cfg(all(test, feature = "legacy-tests"))]
impl<'a> KernelDriverControl<'a> {
    pub const fn new(kernel: &'a mut KernelState) -> Self {
        Self { kernel }
    }
}

#[cfg(all(test, feature = "legacy-tests"))]
impl DriverControlOps for KernelDriverControl<'_> {
    fn register_driver(&mut self, tid: u64) -> Result<(), KernelIpcError> {
        self.kernel
            .register_driver(tid)
            .map_err(map_kernel_ipc_error)
    }

    fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelIpcError> {
        self.kernel.mint_irq_cap(line).map_err(map_kernel_ipc_error)
    }

    fn grant_driver_irq(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError> {
        self.kernel
            .grant_driver_irq(tid, cap)
            .map(|_| ())
            .map_err(map_kernel_ipc_error)
    }

    fn mint_dma_region_cap(
        &mut self,
        mem_cap: CapId,
        offset: usize,
        len: usize,
    ) -> Result<CapId, KernelIpcError> {
        self.kernel
            .mint_dma_region_cap(mem_cap, offset, len)
            .map_err(map_kernel_ipc_error)
    }

    fn grant_driver_dma(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError> {
        self.kernel
            .grant_driver_dma(tid, cap)
            .map(|_| ())
            .map_err(map_kernel_ipc_error)
    }

    fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelIpcError> {
        self.kernel
            .restart_task(tid, token)
            .map_err(map_kernel_ipc_error)
    }
}

#[cfg(all(test, feature = "legacy-tests"))]
fn map_kernel_ipc_error(err: KernelError) -> KernelIpcError {
    match err {
        KernelError::MissingRight => KernelIpcError::MissingRight,
        KernelError::WouldBlock => KernelIpcError::WouldBlock,
        KernelError::CapabilityFull => KernelIpcError::CapabilityFull,
        KernelError::EndpointFull => KernelIpcError::EndpointFull,
        KernelError::EndpointQueueFull => KernelIpcError::EndpointQueueFull,
        KernelError::TaskTableFull => KernelIpcError::TaskTableFull,
        KernelError::MemoryObjectFull => KernelIpcError::MemoryObjectFull,
        KernelError::SchedulerFull => KernelIpcError::SchedulerFull,
        KernelError::VmFull => KernelIpcError::VmFull,
        KernelError::InvalidCapability => KernelIpcError::InvalidCapability,
        KernelError::WrongObject => KernelIpcError::WrongObject,
        KernelError::StaleCapability => KernelIpcError::StaleCapability,
        KernelError::UserMemoryFault => KernelIpcError::UserMemoryFault,
        KernelError::TaskMissing => KernelIpcError::TaskMissing,
        KernelError::MemoryObjectMissing => KernelIpcError::MemoryObjectMissing,
        KernelError::Vm(_) => KernelIpcError::VmFault,
    }
}

// ---------------------------------------------------------------------------
// DriverService — message dispatcher + registry owner
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DriverService {
    registry: DriverRegistry,
    inventory: PlatformInventory,
    handled: usize,
}

impl DriverService {
    pub const fn new() -> Self {
        Self {
            registry: DriverRegistry::new(),
            inventory: PlatformInventory::new(),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn registry(&self) -> &DriverRegistry {
        &self.registry
    }

    pub fn inventory(&self) -> &PlatformInventory {
        &self.inventory
    }

    pub fn inventory_mut(&mut self) -> &mut PlatformInventory {
        &mut self.inventory
    }

    pub fn handle(
        &mut self,
        runtime: &mut impl DriverControlOps,
        request: Message,
    ) -> Result<Message, KernelIpcError> {
        let reply = handle_request_with_sender(
            &mut self.registry,
            &self.inventory,
            runtime,
            request,
            None,
        )?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle_from_sender(
        &mut self,
        runtime: &mut impl DriverControlOps,
        request: Message,
        verified_sender_tid: u64,
    ) -> Result<Message, KernelIpcError> {
        let reply = handle_request_with_sender(
            &mut self.registry,
            &self.inventory,
            runtime,
            request,
            Some(verified_sender_tid),
        )?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle_batch(
        &mut self,
        runtime: &mut impl DriverControlOps,
        requests: impl IntoIterator<Item = Message>,
    ) -> Result<usize, KernelIpcError> {
        for request in requests {
            self.handle(runtime, request)?;
        }
        Ok(self.handled)
    }
}

pub fn handle_request(
    registry: &mut DriverRegistry,
    runtime: &mut impl DriverControlOps,
    request: Message,
) -> Result<Message, KernelIpcError> {
    handle_request_with_sender(registry, &PlatformInventory::new(), runtime, request, None)
}

fn mmio_count(record: &DeviceRecord) -> usize {
    record
        .mmio_ranges
        .iter()
        .filter(|range| range.is_some())
        .count()
}

fn irq_count(record: &DeviceRecord) -> usize {
    record.irq_lines.iter().filter(|irq| irq.is_some()).count()
}

fn count_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(0)
}

fn encode_device_summary(record: &DeviceRecord) -> [u8; 16] {
    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&device_class_code(record.class).to_le_bytes());
    payload[4..8].copy_from_slice(&device_status_code(record.status).to_le_bytes());
    payload[8..12].copy_from_slice(&count_u32(mmio_count(record)).to_le_bytes());
    payload[12..16].copy_from_slice(&count_u32(irq_count(record)).to_le_bytes());
    payload
}

fn encode_mmio_ranges(record: &DeviceRecord) -> [u8; 72] {
    let mut payload = [0u8; 72];
    payload[0..4].copy_from_slice(&count_u32(mmio_count(record)).to_le_bytes());
    let mut cursor = 8;
    for range in record.mmio_ranges.iter().flatten() {
        payload[cursor..cursor + 8].copy_from_slice(&range.base.to_le_bytes());
        payload[cursor + 8..cursor + 16].copy_from_slice(&range.len.to_le_bytes());
        cursor += 16;
    }
    payload
}

fn encode_irq_lines(record: &DeviceRecord) -> [u8; 40] {
    let mut payload = [0u8; 40];
    payload[0..4].copy_from_slice(&count_u32(irq_count(record)).to_le_bytes());
    let mut cursor = 8;
    for irq in record.irq_lines.iter().flatten() {
        payload[cursor..cursor + 4].copy_from_slice(&irq.to_le_bytes());
        cursor += 4;
    }
    payload
}

fn encode_candidate(record: &DeviceRecord) -> [u8; 112] {
    let mut payload = [0u8; 112];
    payload[0..4].copy_from_slice(&device_class_code(record.class).to_le_bytes());
    payload[4..8].copy_from_slice(&device_status_code(record.status).to_le_bytes());
    let compatible_len = record.compatible().map(|value| value.len()).unwrap_or(0);
    let candidate_len = record
        .driver_candidate()
        .map(|value| value.len())
        .unwrap_or(0);
    payload[8..12].copy_from_slice(&count_u32(compatible_len).to_le_bytes());
    payload[12..16].copy_from_slice(&count_u32(candidate_len).to_le_bytes());
    if let Some(compatible) = record.compatible() {
        payload[16..16 + compatible.len()].copy_from_slice(compatible.as_bytes());
    }
    if let Some(candidate) = record.driver_candidate() {
        payload[80..80 + candidate.len()].copy_from_slice(candidate.as_bytes());
    }
    payload
}

fn encode_dma_constraints(record: &DeviceRecord) -> [u8; 16] {
    let mut payload = [0u8; 16];
    payload[0..4].copy_from_slice(&device_class_code(record.class).to_le_bytes());
    payload[4..8].copy_from_slice(&device_status_code(record.status).to_le_bytes());
    // No DMA constraints are modeled yet. The zero count is inert data, not a grant.
    payload[8..12].copy_from_slice(&0u32.to_le_bytes());
    payload
}

fn handle_query_request(
    inventory: &PlatformInventory,
    request: &Message,
    sender_tid: Result<u64, KernelIpcError>,
) -> Result<Message, KernelIpcError> {
    let claimed_tid = read_u64(request.as_slice(), 0)?;
    let tid = verified_tid_or_reject_spoof(sender_tid?, claimed_tid)?;
    let record = inventory.query_assigned_device(tid)?;
    match request.opcode {
        DRIVER_OP_QUERY_MY_DEVICE => inert_reply(request.opcode, &encode_device_summary(record)),
        DRIVER_OP_QUERY_MY_MMIO => inert_reply(request.opcode, &encode_mmio_ranges(record)),
        DRIVER_OP_QUERY_MY_IRQS => inert_reply(request.opcode, &encode_irq_lines(record)),
        DRIVER_OP_QUERY_MY_CANDIDATE => inert_reply(request.opcode, &encode_candidate(record)),
        DRIVER_OP_QUERY_MY_DMA => inert_reply(request.opcode, &encode_dma_constraints(record)),
        _ => Err(KernelIpcError::WrongObject),
    }
}

pub fn handle_request_with_sender(
    registry: &mut DriverRegistry,
    inventory: &PlatformInventory,
    runtime: &mut impl DriverControlOps,
    request: Message,
    verified_sender_tid: Option<u64>,
) -> Result<Message, KernelIpcError> {
    let payload = request.as_slice();
    let sender_tid = verified_sender_tid
        .filter(|tid| *tid != 0)
        .ok_or(KernelIpcError::MissingRight);
    match request.opcode {
        DRIVER_OP_QUERY_MY_DEVICE
        | DRIVER_OP_QUERY_MY_MMIO
        | DRIVER_OP_QUERY_MY_IRQS
        | DRIVER_OP_QUERY_MY_CANDIDATE
        | DRIVER_OP_QUERY_MY_DMA => handle_query_request(inventory, &request, sender_tid),
        DRIVER_OP_REGISTER => {
            let claimed_tid = read_u64(payload, 0)?;
            let tid = verified_tid_or_reject_spoof(sender_tid?, claimed_tid)?;
            // Record in local registry first; then inform kernel runtime.
            registry.register(tid)?;
            runtime.register_driver(tid)?;
            ok_reply(DRIVER_OP_REGISTER, tid, None)
        }
        DRIVER_OP_GRANT_IRQ => {
            let claimed_tid = read_u64(payload, 0)?;
            let tid = verified_tid_or_reject_spoof(sender_tid?, claimed_tid)?;
            let line = read_u16_checked(payload, 8)?;
            inventory.authorize_irq(tid, line)?;
            let cap = runtime.mint_irq_cap(line)?;
            runtime.grant_driver_irq(tid, cap)?;
            ok_reply(DRIVER_OP_GRANT_IRQ, u64::from(line), Some(cap))
        }
        DRIVER_OP_GRANT_DMA => {
            let claimed_tid = read_u64(payload, 0)?;
            let tid = verified_tid_or_reject_spoof(sender_tid?, claimed_tid)?;
            let mem_cap = CapId(read_u64(payload, 8)?);
            if mem_cap.0 == 0 {
                return Err(KernelIpcError::InvalidCapability);
            }
            let offset = read_usize_checked(payload, 16)?;
            let len = read_usize_checked(payload, 24)?;
            if len == 0 || offset.checked_add(len).is_none() {
                return Err(KernelIpcError::WrongObject);
            }
            inventory.authorize_dma(tid)?;
            let cap = runtime.mint_dma_region_cap(mem_cap, offset, len)?;
            runtime.grant_driver_dma(tid, cap)?;
            ok_reply(
                DRIVER_OP_GRANT_DMA,
                u64::try_from(len).map_err(|_| KernelIpcError::WrongObject)?,
                Some(cap),
            )
        }
        DRIVER_OP_RESTARTED => {
            let claimed_tid = read_u64(payload, 0)?;
            let tid = verified_tid_or_reject_spoof(sender_tid?, claimed_tid)?;
            let token = read_u64(payload, 8)?;
            registry.note_restarted(tid)?;
            runtime.restart_task(tid, token)?;
            ok_reply(DRIVER_OP_RESTARTED, tid, None)
        }
        _ => Err(KernelIpcError::WrongObject),
    }
}

fn verified_tid_or_reject_spoof(
    verified_sender_tid: u64,
    claimed_tid: u64,
) -> Result<u64, KernelIpcError> {
    if verified_sender_tid == 0 {
        return Err(KernelIpcError::MissingRight);
    }
    if claimed_tid != 0 && claimed_tid != verified_sender_tid {
        return Err(KernelIpcError::MissingRight);
    }
    Ok(verified_sender_tid)
}

// ---------------------------------------------------------------------------
// Production IPC loop (non-test only)
// ---------------------------------------------------------------------------

/// No-op DriverControlOps used in the bare-metal production loop where the
/// kernel runtime is not available as a Rust trait object.
#[cfg(not(test))]
struct NoopDriverControl;

#[cfg(not(test))]
impl DriverControlOps for NoopDriverControl {
    fn register_driver(&mut self, _tid: u64) -> Result<(), KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
    fn mint_irq_cap(&mut self, _line: u16) -> Result<CapId, KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
    fn grant_driver_irq(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
    fn mint_dma_region_cap(
        &mut self,
        _mem_cap: CapId,
        _offset: usize,
        _len: usize,
    ) -> Result<CapId, KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
    fn grant_driver_dma(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
    fn restart_task(&mut self, _tid: u64, _token: u64) -> Result<(), KernelIpcError> {
        Err(KernelIpcError::MissingRight)
    }
}

pub fn run() {
    yarm_user_rt::user_log!("DRIVER_MANAGER_ENTRY");

    #[cfg(not(test))]
    {
        let ctx = yarm_user_rt::runtime::startup_context();

        let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
            yarm_user_rt::user_log!("DRIVER_MANAGER_NO_RECV_CAP");
            loop {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        };
        yarm_user_rt::user_log!("DRIVER_MANAGER_RECV_CAP cap={}", recv_cap);
        let mut service = DriverService::new();
        let mut runtime = NoopDriverControl;
        yarm_user_rt::user_log!("DRIVER_MANAGER_READY");
        yarm_user_rt::user_log!("DRIVER_MANAGER_HW_CONTROL_UNAVAILABLE");
        yarm_user_rt::user_log!("DRIVER_MANAGER_BLOCKING_RECV_LOOP");

        loop {
            // SAFETY: driver_manager owns its startup-provided service recv endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let reply_cap = received.reply_cap;
                    yarm_user_rt::user_log!(
                        "DRIVER_MANAGER_GOT_MSG opcode={} reply_cap={}",
                        msg.opcode,
                        reply_cap.unwrap_or(u32::MAX)
                    );
                    match service.handle_from_sender(&mut runtime, msg, received.sender_tid) {
                        Ok(reply) => {
                            if let Some(cap) = reply_cap {
                                // SAFETY: kernel validates reply capability rights/object.
                                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(cap, &reply) };
                            }
                        }
                        Err(e) => {
                            yarm_user_rt::user_log!("DRIVER_MANAGER_HANDLE_ERR err={:?}", e);
                            if matches!(e, KernelIpcError::MissingRight) {
                                yarm_user_rt::user_log!("DRIVER_MANAGER_GRANT_DEFERRED_NO_CONTROL");
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    yarm_user_rt::user_log!("DRIVER_MANAGER_RECV_ERR err={:?}", e);
                }
            }
        }
    }

    // In hosted-dev tests the loop above is cfg'd out; nothing to do.
    #[cfg(test)]
    {
        yarm_user_rt::user_log!("DRIVER_MANAGER_HOSTED_DEV_RETURN");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    #[cfg(feature = "legacy-tests")]
    use yarm::kernel::boot::Bootstrap;
    #[cfg(feature = "legacy-tests")]
    use yarm::std::thread;
    #[cfg(feature = "legacy-tests")]
    use yarm_ipc_abi::driver_abi::{DRIVER_OP_GRANT_IRQ, pack_driver_pair};

    #[derive(Debug)]
    struct MockDriverControl {
        next_irq_cap: CapId,
        next_dma_cap: CapId,
        registered: Cell<Option<u64>>,
        irq_line: Cell<Option<u16>>,
        irq_grant: Cell<Option<(u64, CapId)>>,
        dma_request: Cell<Option<(CapId, usize, usize)>>,
        dma_grant: Cell<Option<(u64, CapId)>>,
        restarted: Cell<Option<(u64, u64)>>,
    }

    impl MockDriverControl {
        const fn new() -> Self {
            Self {
                next_irq_cap: CapId(41),
                next_dma_cap: CapId(42),
                registered: Cell::new(None),
                irq_line: Cell::new(None),
                irq_grant: Cell::new(None),
                dma_request: Cell::new(None),
                dma_grant: Cell::new(None),
                restarted: Cell::new(None),
            }
        }
    }

    impl DriverControlOps for MockDriverControl {
        fn register_driver(&mut self, tid: u64) -> Result<(), KernelIpcError> {
            self.registered.set(Some(tid));
            Ok(())
        }

        fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelIpcError> {
            self.irq_line.set(Some(line));
            Ok(self.next_irq_cap)
        }

        fn grant_driver_irq(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError> {
            self.irq_grant.set(Some((tid, cap)));
            Ok(())
        }

        fn mint_dma_region_cap(
            &mut self,
            mem_cap: CapId,
            offset: usize,
            len: usize,
        ) -> Result<CapId, KernelIpcError> {
            self.dma_request.set(Some((mem_cap, offset, len)));
            Ok(self.next_dma_cap)
        }

        fn grant_driver_dma(&mut self, tid: u64, cap: CapId) -> Result<(), KernelIpcError> {
            self.dma_grant.set(Some((tid, cap)));
            Ok(())
        }

        fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelIpcError> {
            self.restarted.set(Some((tid, token)));
            Ok(())
        }
    }

    #[cfg(feature = "legacy-tests")]
    fn run_with_large_stack<F>(f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let handle = thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(f)
            .expect("spawn large-stack test thread");
        handle.join().expect("join large-stack test thread");
    }

    #[cfg(feature = "legacy-tests")]
    #[test]
    fn driver_manager_register_and_grant_irq_roundtrip() {
        run_with_large_stack(|| {
            let mut state = Bootstrap::init().expect("init");
            state.register_task(7).expect("task");
            let mut registry = DriverRegistry::new();
            let mut runtime = KernelDriverControl { kernel: &mut state };

            let register_msg =
                Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &7u64.to_le_bytes())
                    .expect("msg");
            let register_reply =
                handle_request(&mut registry, &mut runtime, register_msg).expect("handle");
            assert_eq!(register_reply.opcode, DRIVER_OP_REGISTER);

            let grant_msg =
                Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(7, 9))
                    .expect("msg");
            let reply = handle_request(&mut registry, &mut runtime, grant_msg).expect("handle");
            assert_eq!(reply.opcode, DRIVER_OP_GRANT_IRQ);
            assert!(reply.transferred_cap().is_some());
        });
    }

    #[cfg(feature = "legacy-tests")]
    #[test]
    fn driver_service_tracks_handled_requests() {
        run_with_large_stack(|| {
            let mut state = Bootstrap::init().expect("init");
            state.register_task(5).expect("task");
            let mut runtime = KernelDriverControl { kernel: &mut state };

            let register =
                Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &5u64.to_le_bytes())
                    .expect("register");
            let irq =
                Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(5, 2))
                    .expect("irq");

            let mut service = DriverService::new();
            let handled = service
                .handle_batch(&mut runtime, [register, irq])
                .expect("batch");
            assert_eq!(handled, 2);
            assert_eq!(service.handled_count(), 2);
        });
    }

    #[test]
    fn register_creates_one_driver_record() {
        let mut registry = DriverRegistry::new();
        assert_eq!(registry.len(), 0);
        registry.register(42).expect("first register");
        assert_eq!(registry.len(), 1);
        let rec = registry.get(42).expect("get");
        assert_eq!(rec.tid, 42);
        assert!(matches!(rec.class, DriverClass::Unknown));
        assert!(matches!(rec.liveness, DriverLiveness::Alive));
    }

    #[test]
    fn duplicate_register_is_idempotent() {
        // Duplicate registration returns Ok without creating a second entry.
        let mut registry = DriverRegistry::new();
        registry.register(10).expect("first");
        registry.register(10).expect("duplicate is ok");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_capacity_is_enforced() {
        let mut registry = DriverRegistry::new();
        let max_drivers_u64 = u64::try_from(MAX_DRIVERS).unwrap();
        for i in 1..=max_drivers_u64 {
            registry.register(i).expect("fill");
        }
        assert_eq!(registry.len(), MAX_DRIVERS);
        let result = registry.register(max_drivers_u64 + 1);
        assert!(result.is_err(), "should fail when table is full");
    }

    fn msg(opcode: u16, words: &[u64]) -> Message {
        let mut payload = [0u8; 32];
        for (index, word) in words.iter().enumerate() {
            let start = index * 8;
            payload[start..start + 8].copy_from_slice(&word.to_le_bytes());
        }
        Message::with_header(0, opcode, 0, None, &payload[..words.len() * 8]).unwrap()
    }

    fn pl011_inventory(tid: u64, status: DeviceStatus) -> PlatformInventory {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new("arm,pl011", DeviceClass::Uart, "pl011_uart", status)
                    .unwrap()
                    .with_mmio(0, 0x107d_0010_0000, 0x1000)
                    .unwrap()
                    .with_irq(0, 121)
                    .unwrap()
                    .assigned_to(tid)
                    .unwrap(),
            )
            .unwrap();
        inventory
    }

    fn read_reply_u32(reply: &Message, offset: usize) -> u32 {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&reply.as_slice()[offset..offset + 4]);
        u32::from_le_bytes(bytes)
    }

    fn read_reply_u64(reply: &Message, offset: usize) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&reply.as_slice()[offset..offset + 8]);
        u64::from_le_bytes(bytes)
    }

    #[test]
    fn privileged_requests_require_verified_sender_and_reject_spoofed_tid() {
        let mut registry = DriverRegistry::new();
        let inventory = PlatformInventory::new();
        let mut runtime = MockDriverControl::new();
        let register = msg(DRIVER_OP_REGISTER, &[7]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, register, None),
            Err(KernelIpcError::MissingRight)
        );

        let forged = msg(DRIVER_OP_REGISTER, &[7]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, forged, Some(8)),
            Err(KernelIpcError::MissingRight)
        );

        let diagnostic_zero = msg(DRIVER_OP_REGISTER, &[0]);
        let reply = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            diagnostic_zero,
            Some(8),
        )
        .expect("register verified sender");
        assert_eq!(reply.opcode, DRIVER_OP_REGISTER);
        assert_eq!(registry.get(8).map(|record| record.tid), Some(8));
        assert_eq!(runtime.registered.get(), Some(8));
    }

    #[test]
    fn pl011_driver_can_query_inert_assigned_device_mmio_irq_and_candidate() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();

        let before_len = registry.len();
        let summary = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            msg(DRIVER_OP_QUERY_MY_DEVICE, &[7]),
            Some(7),
        )
        .expect("query summary");
        assert_eq!(summary.transferred_cap(), None);
        assert_eq!(
            read_reply_u32(&summary, 0),
            device_class_code(DeviceClass::Uart)
        );
        assert_eq!(
            read_reply_u32(&summary, 4),
            device_status_code(DeviceStatus::Discovered)
        );
        assert_eq!(read_reply_u32(&summary, 8), 1);
        assert_eq!(read_reply_u32(&summary, 12), 1);

        let mmio = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            msg(DRIVER_OP_QUERY_MY_MMIO, &[0]),
            Some(7),
        )
        .expect("query mmio");
        assert_eq!(mmio.transferred_cap(), None);
        assert_eq!(read_reply_u32(&mmio, 0), 1);
        assert_eq!(read_reply_u64(&mmio, 8), 0x107d_0010_0000);
        assert_eq!(read_reply_u64(&mmio, 16), 0x1000);

        let irqs = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            msg(DRIVER_OP_QUERY_MY_IRQS, &[7]),
            Some(7),
        )
        .expect("query irqs");
        assert_eq!(irqs.transferred_cap(), None);
        assert_eq!(read_reply_u32(&irqs, 0), 1);
        assert_eq!(read_reply_u32(&irqs, 8), 121);

        let candidate = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            msg(DRIVER_OP_QUERY_MY_CANDIDATE, &[7]),
            Some(7),
        )
        .expect("query candidate");
        assert_eq!(candidate.transferred_cap(), None);
        assert_eq!(candidate.as_slice().len(), 112);
        assert_eq!(
            read_reply_u32(&candidate, 0),
            device_class_code(DeviceClass::Uart)
        );
        assert_eq!(
            read_reply_u32(&candidate, 8),
            u32::try_from("arm,pl011".len()).unwrap()
        );
        assert_eq!(
            read_reply_u32(&candidate, 12),
            u32::try_from("pl011_uart".len()).unwrap()
        );
        assert_eq!(&candidate.as_slice()[16..25], b"arm,pl011");
        assert_eq!(&candidate.as_slice()[80..90], b"pl011_uart");

        let dma = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut runtime,
            msg(DRIVER_OP_QUERY_MY_DMA, &[7]),
            Some(7),
        )
        .expect("query dma");
        assert_eq!(dma.transferred_cap(), None);
        assert_eq!(read_reply_u32(&dma, 8), 0);
        assert_eq!(
            registry.len(),
            before_len,
            "queries must not append registry records"
        );
        assert_eq!(runtime.registered.get(), None);
        assert_eq!(runtime.irq_line.get(), None);
        assert_eq!(runtime.dma_request.get(), None);
        assert_eq!(runtime.restarted.get(), None);
    }

    #[test]
    fn query_requires_verified_sender_and_cannot_spoof_other_assignment() {
        let mut registry = DriverRegistry::new();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();

        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_QUERY_MY_DEVICE, &[7]),
                None,
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_QUERY_MY_DEVICE, &[7]),
                Some(8),
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_QUERY_MY_DEVICE, &[0]),
                Some(8),
            ),
            Err(KernelIpcError::TaskMissing)
        );
    }

    #[test]
    fn deferred_rp1_and_mailbox_can_query_status_but_not_receive_grants() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "raspberrypi,rp1-gpio",
                    DeviceClass::Gpio,
                    "rp1_gpio_srv",
                    DeviceStatus::DeferredNoMmioGrant,
                )
                .unwrap()
                .with_mmio(0, 0x1_0000, 0x1000)
                .unwrap()
                .with_irq(0, 33)
                .unwrap()
                .assigned_to(10)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware_srv",
                    DeviceStatus::DeferredNoMmioGrant,
                )
                .unwrap()
                .with_irq(0, 34)
                .unwrap()
                .assigned_to(11)
                .unwrap(),
            )
            .unwrap();
        let mut registry = DriverRegistry::new();
        registry.register(10).unwrap();
        registry.register(11).unwrap();
        let mut runtime = MockDriverControl::new();

        for (tid, class) in [(10, DeviceClass::Gpio), (11, DeviceClass::Mailbox)] {
            let reply = handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_QUERY_MY_DEVICE, &[tid]),
                Some(tid),
            )
            .expect("deferred query succeeds");
            assert_eq!(reply.transferred_cap(), None);
            assert_eq!(read_reply_u32(&reply, 0), device_class_code(class));
            assert_eq!(
                read_reply_u32(&reply, 4),
                device_status_code(DeviceStatus::DeferredNoMmioGrant)
            );
        }

        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_GRANT_IRQ, &[10, 33]),
                Some(10),
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(DRIVER_OP_GRANT_IRQ, &[11, 34]),
                Some(11),
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(runtime.irq_line.get(), None);
    }

    #[test]
    fn query_output_is_bounded_stable_and_cap_free() {
        let mut registry = DriverRegistry::new();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();
        for (opcode, expected_len) in [
            (DRIVER_OP_QUERY_MY_DEVICE, 16),
            (DRIVER_OP_QUERY_MY_MMIO, 72),
            (DRIVER_OP_QUERY_MY_IRQS, 40),
            (DRIVER_OP_QUERY_MY_CANDIDATE, 112),
            (DRIVER_OP_QUERY_MY_DMA, 16),
        ] {
            let reply = handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                msg(opcode, &[7]),
                Some(7),
            )
            .expect("query");
            assert_eq!(reply.as_slice().len(), expected_len);
            assert_eq!(reply.transferred_cap(), None);
        }
    }

    #[test]
    fn irq_line_overflow_is_rejected_instead_of_truncated() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();
        let overflow_irq = msg(DRIVER_OP_GRANT_IRQ, &[7, 0x1_0005]);
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                overflow_irq,
                Some(7)
            ),
            Err(KernelIpcError::WrongObject)
        );
        assert_eq!(runtime.irq_line.get(), None);
    }

    #[test]
    fn valid_irq_boundary_reaches_mock_control_without_dummy_zero_cap() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "test,max-irq",
                    DeviceClass::IrqMux,
                    "irqmux_srv",
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_irq(0, u32::from(u16::MAX))
                .unwrap()
                .assigned_to(9)
                .unwrap(),
            )
            .unwrap();
        let mut registry = DriverRegistry::new();
        registry.register(9).unwrap();
        let mut runtime = MockDriverControl::new();
        let request = msg(DRIVER_OP_GRANT_IRQ, &[9, u64::from(u16::MAX)]);
        let reply =
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, request, Some(9))
                .expect("max u16 irq is valid");
        assert_eq!(reply.transferred_cap().map(|cap| cap.0), Some(41));
        assert_eq!(runtime.irq_line.get(), Some(u16::MAX));
    }

    #[test]
    fn dma_bounds_and_cap_ids_are_checked() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();
        let zero_cap = msg(DRIVER_OP_GRANT_DMA, &[7, 0, 0, 4096]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, zero_cap, Some(7)),
            Err(KernelIpcError::InvalidCapability)
        );
        let zero_len = msg(DRIVER_OP_GRANT_DMA, &[7, 10, 0, 0]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, zero_len, Some(7)),
            Err(KernelIpcError::WrongObject)
        );

        let valid = msg(DRIVER_OP_GRANT_DMA, &[7, 10, 0, 4096]);
        let reply =
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, valid, Some(7))
                .expect("valid dma request");
        assert_eq!(reply.transferred_cap().map(|cap| cap.0), Some(42));
        assert_eq!(runtime.dma_request.get(), Some((CapId(10), 0, 4096)));
    }

    #[test]
    fn zero_cap_success_from_runtime_is_rejected() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();
        runtime.next_irq_cap = CapId(0);
        let request = msg(DRIVER_OP_GRANT_IRQ, &[7, 121]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, request, Some(7)),
            Err(KernelIpcError::InvalidCapability)
        );
    }

    #[test]
    fn inventory_authorizes_only_assigned_live_resources() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        registry.register(8).unwrap();
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut runtime = MockDriverControl::new();

        let wrong_driver = msg(DRIVER_OP_GRANT_IRQ, &[8, 121]);
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut runtime,
                wrong_driver,
                Some(8)
            ),
            Err(KernelIpcError::MissingRight)
        );

        let wrong_irq = msg(DRIVER_OP_GRANT_IRQ, &[7, 122]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, wrong_irq, Some(7)),
            Err(KernelIpcError::MissingRight)
        );
    }

    #[test]
    fn deferred_rp1_mailbox_and_unknown_devices_do_not_authorize_hardware() {
        let mut inventory = PlatformInventory::new();
        for (tid, compatible, class, candidate, status) in [
            (
                10,
                "raspberrypi,rp1-gpio",
                DeviceClass::Gpio,
                "rp1_gpio_srv",
                DeviceStatus::DeferredNoMmioGrant,
            ),
            (
                11,
                "raspberrypi,firmware",
                DeviceClass::Mailbox,
                "rpi_firmware_srv",
                DeviceStatus::DeferredNoMmioGrant,
            ),
            (
                12,
                "unknown,device",
                DeviceClass::Unknown,
                "unknown",
                DeviceStatus::Unsupported,
            ),
        ] {
            inventory
                .add(
                    DeviceRecord::new(compatible, class, candidate, status)
                        .unwrap()
                        .with_irq(0, 1)
                        .unwrap()
                        .assigned_to(tid)
                        .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            inventory.authorize_irq(10, 1),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            inventory.authorize_irq(11, 1),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            inventory.authorize_dma(12),
            Err(KernelIpcError::MissingRight)
        );
    }

    #[test]
    fn mmio_range_overflow_and_authorization_are_checked() {
        assert!(MmioRange::new(u64::MAX, 2).is_err());
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        assert_eq!(
            inventory.authorize_mmio(7, 0x107d_0010_0000, 0x1000),
            Ok(())
        );
        assert_eq!(
            inventory.authorize_mmio(7, 0x107d_0010_0000, 0x2000),
            Err(KernelIpcError::MissingRight)
        );
    }

    #[test]
    fn restart_updates_existing_verified_record_without_duplicate() {
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let inventory = PlatformInventory::new();
        let mut runtime = MockDriverControl::new();
        let request = msg(DRIVER_OP_RESTARTED, &[7, 0xabc]);
        handle_request_with_sender(&mut registry, &inventory, &mut runtime, request, Some(7))
            .expect("restart existing");
        assert_eq!(registry.len(), 1);
        assert_eq!(runtime.restarted.get(), Some((7, 0xabc)));

        let missing = msg(DRIVER_OP_RESTARTED, &[8, 0xabc]);
        assert_eq!(
            handle_request_with_sender(&mut registry, &inventory, &mut runtime, missing, Some(8)),
            Err(KernelIpcError::TaskMissing)
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn platform_inventory_accepts_fake_rpi5_driver_candidates_without_spawning() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "pl011_uart",
                    DeviceStatus::DeferredNoMmioGrant,
                )
                .unwrap()
                .with_mmio(0, 0x107d_0010_0000, 0x1000)
                .unwrap()
                .with_irq(0, 121)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "raspberrypi,rp1-gpio",
                    DeviceClass::Gpio,
                    "rp1_gpio_srv",
                    DeviceStatus::DeferredNoMmioGrant,
                )
                .unwrap()
                .with_mmio(0, 0, 0x1000)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware_srv",
                    DeviceStatus::DeferredNoMmioGrant,
                )
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "yarm,irqmux",
                    DeviceClass::IrqMux,
                    "irqmux_srv",
                    DeviceStatus::DeferredNoIrqRoute,
                )
                .unwrap(),
            )
            .unwrap();

        assert_eq!(inventory.len(), 4);
        assert_eq!(
            inventory
                .candidates_for(DeviceClass::Uart)
                .next()
                .and_then(DeviceRecord::driver_candidate),
            Some("pl011_uart")
        );
        assert_eq!(
            inventory
                .candidates_for(DeviceClass::Gpio)
                .next()
                .and_then(DeviceRecord::compatible),
            Some("raspberrypi,rp1-gpio")
        );

        let service = DriverService::new();
        assert_eq!(service.handled_count(), 0);
        assert_eq!(service.registry().len(), 0);
        assert_eq!(
            service.inventory().len(),
            0,
            "inventory model is inert and does not spawn/register by default"
        );
    }

    #[test]
    fn platform_inventory_rejects_malformed_records() {
        assert!(
            DeviceRecord::new("", DeviceClass::Unknown, "drv", DeviceStatus::Unsupported).is_err()
        );
        assert!(
            DeviceRecord::new(
                "ok",
                DeviceClass::Unknown,
                "abcdefghijklmnopqrstuvwxyz0123456789",
                DeviceStatus::Unsupported
            )
            .is_err()
        );
        assert!(
            DeviceRecord::new("ok", DeviceClass::Unknown, "drv", DeviceStatus::Unsupported)
                .unwrap()
                .with_mmio(0, 0x1000, 0)
                .is_err()
        );
    }

    #[test]
    fn corrupted_string_lengths_do_not_panic() {
        let mut record = DeviceRecord::new(
            "arm,pl011",
            DeviceClass::Uart,
            "pl011_uart",
            DeviceStatus::Discovered,
        )
        .unwrap();
        assert_eq!(record.compatible(), Some("arm,pl011"));
        assert_eq!(record.driver_candidate(), Some("pl011_uart"));

        record.compatible_len = usize::MAX;
        record.driver_candidate_len = usize::MAX;
        assert_eq!(record.compatible(), None);
        assert_eq!(record.driver_candidate(), None);
    }
}
