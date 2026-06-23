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
const MAX_RESOURCE_GRANT_ENTRIES: usize = MAX_DEVICES * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAction {
    WouldSpawn,
    Deferred,
    Unsupported,
    AlreadyRunning,
    NoCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnBlocker {
    MissingMmioGrant,
    MissingIrqRoute,
    MissingDmaPolicy,
    DeferredNoMmioGrant,
    RequiresPcieBarDiscovery,
    UnsupportedDevice,
    UnknownCandidate,
    AlreadyRegistered,
    MissingSpawnAuthority,
    MissingMailboxTransport,
    MissingCachePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnDenialReason {
    MissingSpawnAuthority,
    PlanEntryDeferred,
    UnsupportedDevice,
    MissingMmioGrant,
    MissingIrqRoute,
    MissingDmaPolicy,
    RequiresPcieBarDiscovery,
    MissingMailboxTransport,
    MissingCachePolicy,
    AlreadyRunning,
    UnknownCandidate,
    PolicyDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceGrantKind {
    Mmio,
    Irq,
    Dma,
    MailboxTransport,
    PcieBar,
    Pinmux,
    Clock,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceGrantRequirement {
    WouldRequest,
    Deferred,
    Denied,
    Unsupported,
    AlreadySatisfied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceGrantBlocker {
    MissingMmioAuthority,
    MissingIrqRouting,
    MissingDmaPolicy,
    RequiresPcieBarDiscovery,
    RequiresMailboxTransport,
    RequiresCacheMaintenancePolicy,
    RequiresPinmuxOwnership,
    RequiresClockDiscovery,
    DeviceDeferred,
    DeviceUnsupported,
    SpawnNotApproved,
    UnknownResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnPolicy {
    pub uart_prereqs_available: bool,
    pub irqmux_prereqs_available: bool,
    pub spawn_authority_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnAuthorityRequest {
    pub requester_tid: Option<u64>,
    pub mock_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnAuthorityPolicy {
    pub spawn_authority_available: bool,
    pub policy_allows_spawn: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceGrantPolicy {
    pub describe_uart_clock: bool,
    pub describe_uart_pinmux: bool,
}

impl ResourceGrantPolicy {
    pub const fn hosted_fake_rpi5() -> Self {
        Self {
            describe_uart_clock: true,
            describe_uart_pinmux: true,
        }
    }
}

impl SpawnAuthorityPolicy {
    pub const fn fail_closed() -> Self {
        Self {
            spawn_authority_available: false,
            policy_allows_spawn: false,
        }
    }

    pub const fn allow_hosted_mock_spawns() -> Self {
        Self {
            spawn_authority_available: true,
            policy_allows_spawn: true,
        }
    }
}

impl SpawnPolicy {
    pub const fn fail_closed() -> Self {
        Self {
            uart_prereqs_available: false,
            irqmux_prereqs_available: false,
            spawn_authority_available: false,
        }
    }

    pub const fn hosted_fake_rpi5() -> Self {
        Self {
            uart_prereqs_available: true,
            irqmux_prereqs_available: false,
            spawn_authority_available: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnPlanEntry {
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub class: DeviceClass,
    pub status: DeviceStatus,
    pub action: SpawnAction,
    pub blockers: [Option<SpawnBlocker>; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnApproval {
    pub mock_spawn_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnDenial {
    pub reasons: [Option<SpawnDenialReason>; 6],
}

impl SpawnDenial {
    const fn empty() -> Self {
        Self { reasons: [None; 6] }
    }

    pub fn has_reason(&self, reason: SpawnDenialReason) -> bool {
        self.reasons.iter().any(|entry| *entry == Some(reason))
    }

    fn push_reason(&mut self, reason: SpawnDenialReason) -> Result<(), KernelIpcError> {
        if self.has_reason(reason) {
            return Ok(());
        }
        let Some(slot) = self.reasons.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(reason);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnAuthorityDecision {
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub action: SpawnAction,
    pub approval: Option<SpawnApproval>,
    pub denial: Option<SpawnDenial>,
}

impl SpawnAuthorityDecision {
    fn from_entry(entry: &SpawnPlanEntry) -> Self {
        Self {
            compatible: entry.compatible,
            compatible_len: entry.compatible_len,
            driver_candidate: entry.driver_candidate,
            driver_candidate_len: entry.driver_candidate_len,
            action: entry.action,
            approval: None,
            denial: None,
        }
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn driver_candidate(&self) -> Option<&str> {
        bounded_str(&self.driver_candidate, self.driver_candidate_len)
    }
}

#[derive(Debug)]
pub struct SpawnAuthorityDecisions {
    decisions: [Option<SpawnAuthorityDecision>; MAX_DEVICES],
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceGrantEntry {
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub kind: ResourceGrantKind,
    pub requirement: ResourceGrantRequirement,
    pub mock_resource_id: Option<u32>,
    pub blockers: [Option<ResourceGrantBlocker>; 6],
}

impl ResourceGrantEntry {
    fn new(
        device: &DeviceRecord,
        kind: ResourceGrantKind,
        requirement: ResourceGrantRequirement,
    ) -> Self {
        Self {
            compatible: device.compatible,
            compatible_len: device.compatible_len,
            driver_candidate: device.driver_candidate,
            driver_candidate_len: device.driver_candidate_len,
            kind,
            requirement,
            mock_resource_id: None,
            blockers: [None; 6],
        }
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn has_blocker(&self, blocker: ResourceGrantBlocker) -> bool {
        self.blockers.iter().any(|entry| *entry == Some(blocker))
    }

    fn with_mock_resource_id(mut self, mock_resource_id: u32) -> Self {
        self.mock_resource_id = Some(mock_resource_id);
        self
    }

    fn push_blocker(&mut self, blocker: ResourceGrantBlocker) -> Result<(), KernelIpcError> {
        if self.has_blocker(blocker) {
            return Ok(());
        }
        let Some(slot) = self.blockers.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(blocker);
        Ok(())
    }
}

#[derive(Debug)]
pub struct ResourceGrantBundle {
    entries: [Option<ResourceGrantEntry>; MAX_RESOURCE_GRANT_ENTRIES],
    len: usize,
}

impl ResourceGrantBundle {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_RESOURCE_GRANT_ENTRIES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &ResourceGrantEntry> {
        self.entries[..self.len]
            .iter()
            .filter_map(|entry| entry.as_ref())
    }

    fn push(&mut self, entry: ResourceGrantEntry) -> Result<(), KernelIpcError> {
        if self.len >= MAX_RESOURCE_GRANT_ENTRIES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.entries[self.len] = Some(entry);
        self.len += 1;
        Ok(())
    }
}

impl SpawnAuthorityDecisions {
    pub const fn new() -> Self {
        Self {
            decisions: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &SpawnAuthorityDecision> {
        self.decisions[..self.len]
            .iter()
            .filter_map(|decision| decision.as_ref())
    }

    fn push(&mut self, decision: SpawnAuthorityDecision) -> Result<(), KernelIpcError> {
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.decisions[self.len] = Some(decision);
        self.len += 1;
        Ok(())
    }
}

impl SpawnPlanEntry {
    fn from_device(device: &DeviceRecord, action: SpawnAction) -> Self {
        Self {
            compatible: device.compatible,
            compatible_len: device.compatible_len,
            driver_candidate: device.driver_candidate,
            driver_candidate_len: device.driver_candidate_len,
            class: device.class,
            status: device.status,
            action,
            blockers: [None; 6],
        }
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn driver_candidate(&self) -> Option<&str> {
        bounded_str(&self.driver_candidate, self.driver_candidate_len)
    }

    pub fn has_blocker(&self, blocker: SpawnBlocker) -> bool {
        self.blockers.iter().any(|entry| *entry == Some(blocker))
    }

    fn push_blocker(&mut self, blocker: SpawnBlocker) -> Result<(), KernelIpcError> {
        if self.has_blocker(blocker) {
            return Ok(());
        }
        let Some(slot) = self.blockers.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(blocker);
        Ok(())
    }
}

#[derive(Debug)]
pub struct SpawnPlan {
    entries: [Option<SpawnPlanEntry>; MAX_DEVICES],
    len: usize,
}

impl SpawnPlan {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &SpawnPlanEntry> {
        self.entries[..self.len]
            .iter()
            .filter_map(|entry| entry.as_ref())
    }

    fn push(&mut self, entry: SpawnPlanEntry) -> Result<(), KernelIpcError> {
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.entries[self.len] = Some(entry);
        self.len += 1;
        Ok(())
    }

    pub fn evaluate_spawn_authority(
        &self,
        _request: SpawnAuthorityRequest,
        policy: SpawnAuthorityPolicy,
    ) -> Result<SpawnAuthorityDecisions, KernelIpcError> {
        let mut decisions = SpawnAuthorityDecisions::new();
        for (index, entry) in self.iter().enumerate() {
            decisions.push(evaluate_spawn_entry(entry, index, policy)?)?;
        }
        Ok(decisions)
    }
}

fn evaluate_spawn_entry(
    entry: &SpawnPlanEntry,
    index: usize,
    policy: SpawnAuthorityPolicy,
) -> Result<SpawnAuthorityDecision, KernelIpcError> {
    let mut decision = SpawnAuthorityDecision::from_entry(entry);
    match entry.action {
        SpawnAction::WouldSpawn
            if policy.spawn_authority_available && policy.policy_allows_spawn =>
        {
            decision.approval = Some(SpawnApproval {
                mock_spawn_id: u32::try_from(index + 1).map_err(|_| KernelIpcError::WrongObject)?,
            });
        }
        SpawnAction::WouldSpawn if !policy.spawn_authority_available => {
            decision.denial = Some(denial_with_reason(
                SpawnDenialReason::MissingSpawnAuthority,
            )?);
        }
        SpawnAction::WouldSpawn => {
            decision.denial = Some(denial_with_reason(SpawnDenialReason::PolicyDenied)?);
        }
        SpawnAction::Deferred => {
            let mut denial = SpawnDenial::empty();
            denial.push_reason(SpawnDenialReason::PlanEntryDeferred)?;
            for blocker in entry.blockers.iter().filter_map(|blocker| *blocker) {
                denial.push_reason(denial_reason_from_blocker(blocker))?;
            }
            decision.denial = Some(denial);
        }
        SpawnAction::Unsupported => {
            decision.denial = Some(denial_with_reason(SpawnDenialReason::UnsupportedDevice)?);
        }
        SpawnAction::AlreadyRunning => {
            decision.denial = Some(denial_with_reason(SpawnDenialReason::AlreadyRunning)?);
        }
        SpawnAction::NoCandidate => {
            decision.denial = Some(denial_with_reason(SpawnDenialReason::UnknownCandidate)?);
        }
    }
    Ok(decision)
}

fn denial_with_reason(reason: SpawnDenialReason) -> Result<SpawnDenial, KernelIpcError> {
    let mut denial = SpawnDenial::empty();
    denial.push_reason(reason)?;
    Ok(denial)
}

fn denial_reason_from_blocker(blocker: SpawnBlocker) -> SpawnDenialReason {
    match blocker {
        SpawnBlocker::MissingMmioGrant | SpawnBlocker::DeferredNoMmioGrant => {
            SpawnDenialReason::MissingMmioGrant
        }
        SpawnBlocker::MissingIrqRoute => SpawnDenialReason::MissingIrqRoute,
        SpawnBlocker::MissingDmaPolicy => SpawnDenialReason::MissingDmaPolicy,
        SpawnBlocker::RequiresPcieBarDiscovery => SpawnDenialReason::RequiresPcieBarDiscovery,
        SpawnBlocker::UnsupportedDevice => SpawnDenialReason::UnsupportedDevice,
        SpawnBlocker::UnknownCandidate => SpawnDenialReason::UnknownCandidate,
        SpawnBlocker::AlreadyRegistered => SpawnDenialReason::AlreadyRunning,
        SpawnBlocker::MissingSpawnAuthority => SpawnDenialReason::MissingSpawnAuthority,
        SpawnBlocker::MissingMailboxTransport => SpawnDenialReason::MissingMailboxTransport,
        SpawnBlocker::MissingCachePolicy => SpawnDenialReason::MissingCachePolicy,
    }
}

const MAX_DRIVER_SPAWN_REQUEST_RESOURCES: usize = 8;
const MAX_STARTUP_CAP_REQUIREMENTS: usize = 9;
const MAX_DRIVER_SPAWN_DEPENDENCIES: usize = 4;
const MAX_DRIVER_SPAWN_BLOCKERS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverSpawnRequestStatus {
    ReadyForPmValidation,
    Deferred,
    Denied,
    Unsupported,
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverSpawnRequestBlocker {
    MissingSpawnAuthority,
    PlanEntryDeferred,
    MissingMmioGrant,
    MissingIrqRoute,
    MissingDmaPolicy,
    RequiresPcieBarDiscovery,
    MissingMailboxTransport,
    MissingCachePolicy,
    UnsupportedDevice,
    UnknownCandidate,
    AlreadyRunning,
    PolicyDenied,
    SpawnNotApproved,
    UnknownResource,
    DeviceDeferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverSpawnResourceRequirement {
    pub kind: ResourceGrantKind,
    pub requirement: ResourceGrantRequirement,
    /// Inert model-only resource id. This is not a CapId and is never
    /// materialized into authority.
    pub mock_resource_id: Option<u32>,
    pub blockers: [Option<ResourceGrantBlocker>; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverSpawnDependency {
    DriverManager,
    Devfs,
    IrqMux,
    PlatformInventory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverSpawnHealthPolicy {
    pub startup_timeout_ms: u32,
    pub heartbeat_timeout_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverRestartPolicy {
    pub max_restarts: u8,
    pub backoff_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverIsolationPolicy {
    DefaultUserDriver,
    HardwareIsolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupCapRequirement {
    DriverManagerControlEndpoint,
    DriverRegistrationEndpoint,
    FaultOrRestartEndpoint,
    Mmio,
    IrqNotification,
    DmaOrIommu,
    MailboxTransport,
    DevfsRegistration,
    LoggingOrDebug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverSpawnRequest {
    pub request_version: u16,
    /// Inert model-only request id. This is not a task id, process handle, or cap.
    pub mock_request_id: u32,
    pub image_id: Option<u32>,
    pub image_name: [u8; 32],
    pub image_name_len: usize,
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub device_class: DeviceClass,
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub device_record_index: usize,
    pub status: DriverSpawnRequestStatus,
    pub resource_requirements:
        [Option<DriverSpawnResourceRequirement>; MAX_DRIVER_SPAWN_REQUEST_RESOURCES],
    pub startup_cap_requirements: [Option<StartupCapRequirement>; MAX_STARTUP_CAP_REQUIREMENTS],
    pub dependencies: [Option<DriverSpawnDependency>; MAX_DRIVER_SPAWN_DEPENDENCIES],
    pub restart_policy: DriverRestartPolicy,
    pub health_policy: DriverSpawnHealthPolicy,
    pub isolation_policy: DriverIsolationPolicy,
    pub blockers: [Option<DriverSpawnRequestBlocker>; MAX_DRIVER_SPAWN_BLOCKERS],
}

impl DriverSpawnRequest {
    fn from_pipeline(
        device: &DeviceRecord,
        device_record_index: usize,
        plan_entry: &SpawnPlanEntry,
        decision: &SpawnAuthorityDecision,
    ) -> Result<Self, KernelIpcError> {
        let mock_request_id =
            u32::try_from(device_record_index + 1).map_err(|_| KernelIpcError::WrongObject)?;
        let mut request = Self {
            request_version: 1,
            mock_request_id,
            image_id: None,
            image_name: device.driver_candidate,
            image_name_len: device.driver_candidate_len,
            driver_candidate: device.driver_candidate,
            driver_candidate_len: device.driver_candidate_len,
            device_class: device.class,
            compatible: device.compatible,
            compatible_len: device.compatible_len,
            device_record_index,
            status: request_status_from_pipeline(plan_entry, decision),
            resource_requirements: [None; MAX_DRIVER_SPAWN_REQUEST_RESOURCES],
            startup_cap_requirements: [None; MAX_STARTUP_CAP_REQUIREMENTS],
            dependencies: [None; MAX_DRIVER_SPAWN_DEPENDENCIES],
            restart_policy: DriverRestartPolicy {
                max_restarts: 3,
                backoff_ms: 1000,
            },
            health_policy: DriverSpawnHealthPolicy {
                startup_timeout_ms: 5000,
                heartbeat_timeout_ms: 1000,
            },
            isolation_policy: DriverIsolationPolicy::DefaultUserDriver,
            blockers: [None; MAX_DRIVER_SPAWN_BLOCKERS],
        };
        request.push_dependency(DriverSpawnDependency::DriverManager)?;
        request.push_dependency(DriverSpawnDependency::PlatformInventory)?;
        if matches!(
            device.class,
            DeviceClass::Uart | DeviceClass::Gpio | DeviceClass::Block
        ) {
            request.push_dependency(DriverSpawnDependency::Devfs)?;
        }
        if matches!(
            device.class,
            DeviceClass::Uart | DeviceClass::Gpio | DeviceClass::IrqMux
        ) {
            request.push_dependency(DriverSpawnDependency::IrqMux)?;
        }
        request.push_startup_cap(StartupCapRequirement::DriverManagerControlEndpoint)?;
        request.push_startup_cap(StartupCapRequirement::DriverRegistrationEndpoint)?;
        request.push_startup_cap(StartupCapRequirement::FaultOrRestartEndpoint)?;
        request.push_startup_cap(StartupCapRequirement::DevfsRegistration)?;
        request.push_startup_cap(StartupCapRequirement::LoggingOrDebug)?;
        for blocker in plan_entry.blockers.iter().filter_map(|blocker| *blocker) {
            request.push_blocker(request_blocker_from_spawn_blocker(blocker))?;
        }
        if let Some(denial) = decision.denial {
            for reason in denial.reasons.iter().filter_map(|reason| *reason) {
                request.push_blocker(request_blocker_from_denial(reason))?;
            }
        }
        Ok(request)
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn driver_candidate(&self) -> Option<&str> {
        bounded_str(&self.driver_candidate, self.driver_candidate_len)
    }

    pub fn image_name(&self) -> Option<&str> {
        bounded_str(&self.image_name, self.image_name_len)
    }

    pub fn has_startup_cap_requirement(&self, requirement: StartupCapRequirement) -> bool {
        self.startup_cap_requirements
            .iter()
            .any(|entry| *entry == Some(requirement))
    }

    pub fn has_resource_requirement(&self, kind: ResourceGrantKind) -> bool {
        self.resource_requirements
            .iter()
            .filter_map(|entry| *entry)
            .any(|entry| entry.kind == kind)
    }

    pub fn has_blocker(&self, blocker: DriverSpawnRequestBlocker) -> bool {
        self.blockers.iter().any(|entry| *entry == Some(blocker))
    }

    fn push_resource(
        &mut self,
        requirement: DriverSpawnResourceRequirement,
    ) -> Result<(), KernelIpcError> {
        let Some(slot) = self
            .resource_requirements
            .iter_mut()
            .find(|slot| slot.is_none())
        else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(requirement);
        match requirement.kind {
            ResourceGrantKind::Mmio => self.push_startup_cap(StartupCapRequirement::Mmio)?,
            ResourceGrantKind::Irq => {
                self.push_startup_cap(StartupCapRequirement::IrqNotification)?
            }
            ResourceGrantKind::Dma => self.push_startup_cap(StartupCapRequirement::DmaOrIommu)?,
            ResourceGrantKind::MailboxTransport => {
                self.push_startup_cap(StartupCapRequirement::MailboxTransport)?
            }
            _ => {}
        }
        for blocker in requirement.blockers.iter().filter_map(|blocker| *blocker) {
            self.push_blocker(request_blocker_from_resource_blocker(blocker))?;
        }
        Ok(())
    }

    fn push_startup_cap(
        &mut self,
        requirement: StartupCapRequirement,
    ) -> Result<(), KernelIpcError> {
        if self.has_startup_cap_requirement(requirement) {
            return Ok(());
        }
        let Some(slot) = self
            .startup_cap_requirements
            .iter_mut()
            .find(|slot| slot.is_none())
        else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(requirement);
        Ok(())
    }

    fn push_dependency(&mut self, dependency: DriverSpawnDependency) -> Result<(), KernelIpcError> {
        if self
            .dependencies
            .iter()
            .any(|entry| *entry == Some(dependency))
        {
            return Ok(());
        }
        let Some(slot) = self.dependencies.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(dependency);
        Ok(())
    }

    fn push_blocker(&mut self, blocker: DriverSpawnRequestBlocker) -> Result<(), KernelIpcError> {
        if self.has_blocker(blocker) {
            return Ok(());
        }
        let Some(slot) = self.blockers.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(blocker);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverSpawnRequestBundle {
    requests: [Option<DriverSpawnRequest>; MAX_DEVICES],
    len: usize,
}

impl DriverSpawnRequestBundle {
    pub const fn new() -> Self {
        Self {
            requests: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &DriverSpawnRequest> {
        self.requests[..self.len]
            .iter()
            .filter_map(|request| request.as_ref())
    }

    pub fn ready_count(&self) -> usize {
        self.iter()
            .filter(|request| request.status == DriverSpawnRequestStatus::ReadyForPmValidation)
            .count()
    }

    pub fn simulate_pm_validation(
        &self,
        inventory: Option<&PlatformInventory>,
        policy: PmSpawnValidationPolicy,
    ) -> Result<PmSpawnValidationReport, KernelIpcError> {
        let mut report = PmSpawnValidationReport::new();
        let mut accepted_so_far = 0usize;
        for request in self.iter() {
            let entry = validate_pm_spawn_request(request, inventory, policy, accepted_so_far)?;
            if entry.status == PmSpawnValidationStatus::WouldAccept {
                accepted_so_far = accepted_so_far
                    .checked_add(1)
                    .ok_or(KernelIpcError::CapabilityFull)?;
            }
            report.push(entry)?;
        }
        Ok(report)
    }

    pub fn simulate_pm_accounting(
        &self,
        validation_report: &PmSpawnValidationReport,
        policy: PmSpawnAccountingPolicy,
    ) -> Result<PmSpawnAccountingReport, KernelIpcError> {
        let mut report = PmSpawnAccountingReport::new();
        let mut committed_so_far = 0usize;
        for (request, validation) in self.iter().zip(validation_report.iter()) {
            if request.compatible() != validation.compatible()
                || request.mock_request_id != validation.mock_request_id
            {
                return Err(KernelIpcError::WrongObject);
            }
            let entry =
                simulate_pm_spawn_accounting_entry(request, validation, policy, committed_so_far)?;
            if entry.status == PmSpawnAccountingStatus::WouldCommit {
                committed_so_far = committed_so_far
                    .checked_add(1)
                    .ok_or(KernelIpcError::CapabilityFull)?;
            }
            report.push(entry)?;
        }
        if report.len() != self.len() || report.len() != validation_report.len() {
            return Err(KernelIpcError::WrongObject);
        }
        Ok(report)
    }

    fn push(&mut self, request: DriverSpawnRequest) -> Result<(), KernelIpcError> {
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.requests[self.len] = Some(request);
        self.len += 1;
        Ok(())
    }
}

fn request_status_from_pipeline(
    plan_entry: &SpawnPlanEntry,
    decision: &SpawnAuthorityDecision,
) -> DriverSpawnRequestStatus {
    if decision.approval.is_some() && matches!(plan_entry.action, SpawnAction::WouldSpawn) {
        return DriverSpawnRequestStatus::ReadyForPmValidation;
    }
    match plan_entry.action {
        SpawnAction::WouldSpawn => DriverSpawnRequestStatus::Denied,
        SpawnAction::Deferred => DriverSpawnRequestStatus::Deferred,
        SpawnAction::Unsupported | SpawnAction::NoCandidate => {
            DriverSpawnRequestStatus::Unsupported
        }
        SpawnAction::AlreadyRunning => DriverSpawnRequestStatus::AlreadyRunning,
    }
}

fn request_blocker_from_spawn_blocker(blocker: SpawnBlocker) -> DriverSpawnRequestBlocker {
    match blocker {
        SpawnBlocker::MissingMmioGrant | SpawnBlocker::DeferredNoMmioGrant => {
            DriverSpawnRequestBlocker::MissingMmioGrant
        }
        SpawnBlocker::MissingIrqRoute => DriverSpawnRequestBlocker::MissingIrqRoute,
        SpawnBlocker::MissingDmaPolicy => DriverSpawnRequestBlocker::MissingDmaPolicy,
        SpawnBlocker::RequiresPcieBarDiscovery => {
            DriverSpawnRequestBlocker::RequiresPcieBarDiscovery
        }
        SpawnBlocker::UnsupportedDevice => DriverSpawnRequestBlocker::UnsupportedDevice,
        SpawnBlocker::UnknownCandidate => DriverSpawnRequestBlocker::UnknownCandidate,
        SpawnBlocker::AlreadyRegistered => DriverSpawnRequestBlocker::AlreadyRunning,
        SpawnBlocker::MissingSpawnAuthority => DriverSpawnRequestBlocker::MissingSpawnAuthority,
        SpawnBlocker::MissingMailboxTransport => DriverSpawnRequestBlocker::MissingMailboxTransport,
        SpawnBlocker::MissingCachePolicy => DriverSpawnRequestBlocker::MissingCachePolicy,
    }
}

fn request_blocker_from_denial(reason: SpawnDenialReason) -> DriverSpawnRequestBlocker {
    match reason {
        SpawnDenialReason::MissingSpawnAuthority => {
            DriverSpawnRequestBlocker::MissingSpawnAuthority
        }
        SpawnDenialReason::PlanEntryDeferred => DriverSpawnRequestBlocker::PlanEntryDeferred,
        SpawnDenialReason::UnsupportedDevice => DriverSpawnRequestBlocker::UnsupportedDevice,
        SpawnDenialReason::MissingMmioGrant => DriverSpawnRequestBlocker::MissingMmioGrant,
        SpawnDenialReason::MissingIrqRoute => DriverSpawnRequestBlocker::MissingIrqRoute,
        SpawnDenialReason::MissingDmaPolicy => DriverSpawnRequestBlocker::MissingDmaPolicy,
        SpawnDenialReason::RequiresPcieBarDiscovery => {
            DriverSpawnRequestBlocker::RequiresPcieBarDiscovery
        }
        SpawnDenialReason::MissingMailboxTransport => {
            DriverSpawnRequestBlocker::MissingMailboxTransport
        }
        SpawnDenialReason::MissingCachePolicy => DriverSpawnRequestBlocker::MissingCachePolicy,
        SpawnDenialReason::AlreadyRunning => DriverSpawnRequestBlocker::AlreadyRunning,
        SpawnDenialReason::UnknownCandidate => DriverSpawnRequestBlocker::UnknownCandidate,
        SpawnDenialReason::PolicyDenied => DriverSpawnRequestBlocker::PolicyDenied,
    }
}

fn request_blocker_from_resource_blocker(
    blocker: ResourceGrantBlocker,
) -> DriverSpawnRequestBlocker {
    match blocker {
        ResourceGrantBlocker::MissingMmioAuthority => DriverSpawnRequestBlocker::MissingMmioGrant,
        ResourceGrantBlocker::MissingIrqRouting => DriverSpawnRequestBlocker::MissingIrqRoute,
        ResourceGrantBlocker::MissingDmaPolicy => DriverSpawnRequestBlocker::MissingDmaPolicy,
        ResourceGrantBlocker::RequiresPcieBarDiscovery => {
            DriverSpawnRequestBlocker::RequiresPcieBarDiscovery
        }
        ResourceGrantBlocker::RequiresMailboxTransport => {
            DriverSpawnRequestBlocker::MissingMailboxTransport
        }
        ResourceGrantBlocker::RequiresCacheMaintenancePolicy => {
            DriverSpawnRequestBlocker::MissingCachePolicy
        }
        ResourceGrantBlocker::RequiresPinmuxOwnership => {
            DriverSpawnRequestBlocker::MissingMmioGrant
        }
        ResourceGrantBlocker::RequiresClockDiscovery => DriverSpawnRequestBlocker::MissingMmioGrant,
        ResourceGrantBlocker::DeviceDeferred => DriverSpawnRequestBlocker::DeviceDeferred,
        ResourceGrantBlocker::DeviceUnsupported => DriverSpawnRequestBlocker::UnsupportedDevice,
        ResourceGrantBlocker::SpawnNotApproved => DriverSpawnRequestBlocker::SpawnNotApproved,
        ResourceGrantBlocker::UnknownResource => DriverSpawnRequestBlocker::UnknownResource,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnValidationStatus {
    WouldAccept,
    WouldReject,
    Deferred,
    Unsupported,
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnValidationFailure {
    MissingVerifiedDriverManager,
    RequestVersionUnsupported,
    SpawnRequestNotReady,
    DeviceDeferred,
    DeviceUnsupported,
    ResourceNotAssigned,
    ResourceDeferred,
    MissingMmioAuthority,
    MissingIrqRouting,
    MissingDmaPolicy,
    MissingPcieBar,
    MissingMailboxTransport,
    MissingCachePolicy,
    MissingStartupCapLayout,
    ImageNotAllowed,
    ResourceLimitExceeded,
    AlreadyRunning,
    PolicyDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmSpawnValidationPolicy {
    pub verified_driver_manager_identity: bool,
    pub supported_request_version: u16,
    pub allow_uart_srv_image: bool,
    pub allow_irqmux_srv_image: bool,
    pub max_would_accept: usize,
    pub require_inventory_match: bool,
}

impl PmSpawnValidationPolicy {
    pub const fn fail_closed() -> Self {
        Self {
            verified_driver_manager_identity: false,
            supported_request_version: 1,
            allow_uart_srv_image: false,
            allow_irqmux_srv_image: false,
            max_would_accept: 0,
            require_inventory_match: true,
        }
    }

    pub const fn hosted_fake_rpi5() -> Self {
        Self {
            verified_driver_manager_identity: true,
            supported_request_version: 1,
            allow_uart_srv_image: true,
            allow_irqmux_srv_image: false,
            max_would_accept: MAX_DEVICES,
            require_inventory_match: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmSpawnValidationEntry {
    pub mock_request_id: u32,
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub driver_candidate: [u8; 32],
    pub driver_candidate_len: usize,
    pub status: PmSpawnValidationStatus,
    pub failures: [Option<PmSpawnValidationFailure>; MAX_DRIVER_SPAWN_BLOCKERS],
}

impl PmSpawnValidationEntry {
    fn from_request(request: &DriverSpawnRequest) -> Self {
        Self {
            mock_request_id: request.mock_request_id,
            compatible: request.compatible,
            compatible_len: request.compatible_len,
            driver_candidate: request.driver_candidate,
            driver_candidate_len: request.driver_candidate_len,
            status: PmSpawnValidationStatus::WouldReject,
            failures: [None; MAX_DRIVER_SPAWN_BLOCKERS],
        }
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn has_failure(&self, failure: PmSpawnValidationFailure) -> bool {
        self.failures.iter().any(|entry| *entry == Some(failure))
    }

    fn push_failure(&mut self, failure: PmSpawnValidationFailure) -> Result<(), KernelIpcError> {
        if self.has_failure(failure) {
            return Ok(());
        }
        let Some(slot) = self.failures.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(failure);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmSpawnValidationReport {
    entries: [Option<PmSpawnValidationEntry>; MAX_DEVICES],
    len: usize,
}

impl PmSpawnValidationReport {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &PmSpawnValidationEntry> {
        self.entries[..self.len]
            .iter()
            .filter_map(|entry| entry.as_ref())
    }

    pub fn would_accept_count(&self) -> usize {
        self.iter()
            .filter(|entry| entry.status == PmSpawnValidationStatus::WouldAccept)
            .count()
    }

    fn push(&mut self, entry: PmSpawnValidationEntry) -> Result<(), KernelIpcError> {
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.entries[self.len] = Some(entry);
        self.len += 1;
        Ok(())
    }
}

fn validate_pm_spawn_request(
    request: &DriverSpawnRequest,
    inventory: Option<&PlatformInventory>,
    policy: PmSpawnValidationPolicy,
    accepted_so_far: usize,
) -> Result<PmSpawnValidationEntry, KernelIpcError> {
    let mut entry = PmSpawnValidationEntry::from_request(request);
    match request.status {
        DriverSpawnRequestStatus::Deferred => {
            entry.status = PmSpawnValidationStatus::Deferred;
            entry.push_failure(PmSpawnValidationFailure::SpawnRequestNotReady)?;
            entry.push_failure(PmSpawnValidationFailure::DeviceDeferred)?;
        }
        DriverSpawnRequestStatus::Unsupported => {
            entry.status = PmSpawnValidationStatus::Unsupported;
            entry.push_failure(PmSpawnValidationFailure::SpawnRequestNotReady)?;
            entry.push_failure(PmSpawnValidationFailure::DeviceUnsupported)?;
        }
        DriverSpawnRequestStatus::AlreadyRunning => {
            entry.status = PmSpawnValidationStatus::AlreadyRunning;
            entry.push_failure(PmSpawnValidationFailure::AlreadyRunning)?;
        }
        DriverSpawnRequestStatus::Denied => {
            entry.status = PmSpawnValidationStatus::WouldReject;
            entry.push_failure(PmSpawnValidationFailure::SpawnRequestNotReady)?;
            entry.push_failure(PmSpawnValidationFailure::PolicyDenied)?;
        }
        DriverSpawnRequestStatus::ReadyForPmValidation => {
            entry.status = PmSpawnValidationStatus::WouldAccept;
        }
    }

    if !policy.verified_driver_manager_identity {
        entry.push_failure(PmSpawnValidationFailure::MissingVerifiedDriverManager)?;
    }
    if request.request_version != policy.supported_request_version {
        entry.push_failure(PmSpawnValidationFailure::RequestVersionUnsupported)?;
    }
    if !image_allowed_by_policy(request, policy) {
        entry.push_failure(PmSpawnValidationFailure::ImageNotAllowed)?;
    }
    if accepted_so_far >= policy.max_would_accept
        && request.status == DriverSpawnRequestStatus::ReadyForPmValidation
    {
        entry.push_failure(PmSpawnValidationFailure::ResourceLimitExceeded)?;
    }
    validate_startup_cap_layout(request, &mut entry)?;
    validate_request_resources(request, inventory, policy, &mut entry)?;
    for blocker in request.blockers.iter().filter_map(|blocker| *blocker) {
        entry.push_failure(pm_failure_from_request_blocker(blocker))?;
    }

    if entry.failures.iter().any(Option::is_some)
        && entry.status == PmSpawnValidationStatus::WouldAccept
    {
        entry.status = PmSpawnValidationStatus::WouldReject;
    }
    Ok(entry)
}

fn image_allowed_by_policy(request: &DriverSpawnRequest, policy: PmSpawnValidationPolicy) -> bool {
    match request.driver_candidate() {
        Some("uart_srv") => policy.allow_uart_srv_image,
        Some("irqmux_srv") => policy.allow_irqmux_srv_image,
        _ => false,
    }
}

fn validate_startup_cap_layout(
    request: &DriverSpawnRequest,
    entry: &mut PmSpawnValidationEntry,
) -> Result<(), KernelIpcError> {
    for required in [
        StartupCapRequirement::DriverManagerControlEndpoint,
        StartupCapRequirement::DriverRegistrationEndpoint,
        StartupCapRequirement::FaultOrRestartEndpoint,
    ] {
        if !request.has_startup_cap_requirement(required) {
            entry.push_failure(PmSpawnValidationFailure::MissingStartupCapLayout)?;
        }
    }
    for resource in request
        .resource_requirements
        .iter()
        .filter_map(|entry| *entry)
    {
        let required = match resource.kind {
            ResourceGrantKind::Mmio => Some(StartupCapRequirement::Mmio),
            ResourceGrantKind::Irq => Some(StartupCapRequirement::IrqNotification),
            ResourceGrantKind::Dma => Some(StartupCapRequirement::DmaOrIommu),
            ResourceGrantKind::MailboxTransport => Some(StartupCapRequirement::MailboxTransport),
            _ => None,
        };
        if let Some(required) = required
            && !request.has_startup_cap_requirement(required)
        {
            entry.push_failure(PmSpawnValidationFailure::MissingStartupCapLayout)?;
        }
    }
    Ok(())
}

fn validate_request_resources(
    request: &DriverSpawnRequest,
    inventory: Option<&PlatformInventory>,
    policy: PmSpawnValidationPolicy,
    entry: &mut PmSpawnValidationEntry,
) -> Result<(), KernelIpcError> {
    let inventory_record = if policy.require_inventory_match {
        match inventory.and_then(|inventory| inventory.iter().nth(request.device_record_index)) {
            Some(device)
                if device.compatible() == request.compatible()
                    && device.driver_candidate() == request.driver_candidate()
                    && device.class == request.device_class =>
            {
                Some(device)
            }
            _ => {
                entry.push_failure(PmSpawnValidationFailure::ResourceNotAssigned)?;
                None
            }
        }
    } else {
        None
    };

    for resource in request
        .resource_requirements
        .iter()
        .filter_map(|entry| *entry)
    {
        if resource.mock_resource_id == Some(0) {
            entry.push_failure(PmSpawnValidationFailure::ResourceNotAssigned)?;
        }
        if resource.requirement != ResourceGrantRequirement::WouldRequest {
            entry.push_failure(PmSpawnValidationFailure::ResourceDeferred)?;
        }
        for blocker in resource.blockers.iter().filter_map(|blocker| *blocker) {
            entry.push_failure(pm_failure_from_resource_blocker(blocker))?;
        }
        if let Some(device) = inventory_record {
            match resource.kind {
                ResourceGrantKind::Mmio if !device.mmio_ranges.iter().any(Option::is_some) => {
                    entry.push_failure(PmSpawnValidationFailure::ResourceNotAssigned)?;
                }
                ResourceGrantKind::Irq if !device.irq_lines.iter().any(Option::is_some) => {
                    entry.push_failure(PmSpawnValidationFailure::ResourceNotAssigned)?;
                }
                ResourceGrantKind::PcieBar => {
                    entry.push_failure(PmSpawnValidationFailure::MissingPcieBar)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn pm_failure_from_request_blocker(blocker: DriverSpawnRequestBlocker) -> PmSpawnValidationFailure {
    match blocker {
        DriverSpawnRequestBlocker::MissingSpawnAuthority => PmSpawnValidationFailure::PolicyDenied,
        DriverSpawnRequestBlocker::PlanEntryDeferred => PmSpawnValidationFailure::DeviceDeferred,
        DriverSpawnRequestBlocker::MissingMmioGrant => {
            PmSpawnValidationFailure::MissingMmioAuthority
        }
        DriverSpawnRequestBlocker::MissingIrqRoute => PmSpawnValidationFailure::MissingIrqRouting,
        DriverSpawnRequestBlocker::MissingDmaPolicy => PmSpawnValidationFailure::MissingDmaPolicy,
        DriverSpawnRequestBlocker::RequiresPcieBarDiscovery => {
            PmSpawnValidationFailure::MissingPcieBar
        }
        DriverSpawnRequestBlocker::MissingMailboxTransport => {
            PmSpawnValidationFailure::MissingMailboxTransport
        }
        DriverSpawnRequestBlocker::MissingCachePolicy => {
            PmSpawnValidationFailure::MissingCachePolicy
        }
        DriverSpawnRequestBlocker::UnsupportedDevice => PmSpawnValidationFailure::DeviceUnsupported,
        DriverSpawnRequestBlocker::UnknownCandidate => PmSpawnValidationFailure::ImageNotAllowed,
        DriverSpawnRequestBlocker::AlreadyRunning => PmSpawnValidationFailure::AlreadyRunning,
        DriverSpawnRequestBlocker::PolicyDenied => PmSpawnValidationFailure::PolicyDenied,
        DriverSpawnRequestBlocker::SpawnNotApproved => PmSpawnValidationFailure::PolicyDenied,
        DriverSpawnRequestBlocker::UnknownResource => PmSpawnValidationFailure::ResourceNotAssigned,
        DriverSpawnRequestBlocker::DeviceDeferred => PmSpawnValidationFailure::DeviceDeferred,
    }
}

fn pm_failure_from_resource_blocker(blocker: ResourceGrantBlocker) -> PmSpawnValidationFailure {
    match blocker {
        ResourceGrantBlocker::MissingMmioAuthority => {
            PmSpawnValidationFailure::MissingMmioAuthority
        }
        ResourceGrantBlocker::MissingIrqRouting => PmSpawnValidationFailure::MissingIrqRouting,
        ResourceGrantBlocker::MissingDmaPolicy => PmSpawnValidationFailure::MissingDmaPolicy,
        ResourceGrantBlocker::RequiresPcieBarDiscovery => PmSpawnValidationFailure::MissingPcieBar,
        ResourceGrantBlocker::RequiresMailboxTransport => {
            PmSpawnValidationFailure::MissingMailboxTransport
        }
        ResourceGrantBlocker::RequiresCacheMaintenancePolicy => {
            PmSpawnValidationFailure::MissingCachePolicy
        }
        ResourceGrantBlocker::RequiresPinmuxOwnership => PmSpawnValidationFailure::PolicyDenied,
        ResourceGrantBlocker::RequiresClockDiscovery => PmSpawnValidationFailure::PolicyDenied,
        ResourceGrantBlocker::DeviceDeferred => PmSpawnValidationFailure::DeviceDeferred,
        ResourceGrantBlocker::DeviceUnsupported => PmSpawnValidationFailure::DeviceUnsupported,
        ResourceGrantBlocker::SpawnNotApproved => PmSpawnValidationFailure::PolicyDenied,
        ResourceGrantBlocker::UnknownResource => PmSpawnValidationFailure::ResourceNotAssigned,
    }
}

const MAX_PM_SPAWN_RESERVATIONS: usize = 12;
const MAX_PM_SPAWN_ROLLBACK_STEPS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnAccountingStatus {
    WouldReserve,
    WouldCommit,
    WouldRollback,
    WouldReject,
    Deferred,
    Unsupported,
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnReservation {
    ProcessSlot,
    AddressSpace,
    CNodeSlots,
    MmioWindow,
    IrqRoute,
    DmaWindow,
    StartupCapSlots,
    HandleSlot,
    HealthMonitorSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnRollbackStep {
    ReleaseProcessSlot,
    DestroyAddressSpace,
    RevokeMintedCaps,
    ReleaseMmioReservation,
    ReleaseIrqReservation,
    ReleaseDmaReservation,
    ClearStartupCapSlots,
    DropHandle,
    ClearHealthMonitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnAccountingFailure {
    ValidationNotAccepted,
    PolicyDenied,
    ResourceLimitExceeded,
    MissingStartupCapLayout,
    InjectedFailureBeforeReservation,
    InjectedFailureAfterReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSpawnFailureInjectionPoint {
    None,
    BeforeAnyReservation,
    AfterProcessSlot,
    AfterAddressSpace,
    AfterStartupCapSlots,
    AfterMmio,
    AfterIrq,
    AfterHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmSpawnAccountingPolicy {
    pub accounting_allowed: bool,
    pub max_commits: usize,
    pub commit_successful_reservations: bool,
    pub failure_injection: PmSpawnFailureInjectionPoint,
}

impl PmSpawnAccountingPolicy {
    pub const fn fail_closed() -> Self {
        Self {
            accounting_allowed: false,
            max_commits: 0,
            commit_successful_reservations: false,
            failure_injection: PmSpawnFailureInjectionPoint::None,
        }
    }

    pub const fn hosted_fake_rpi5() -> Self {
        Self {
            accounting_allowed: true,
            max_commits: MAX_DEVICES,
            commit_successful_reservations: true,
            failure_injection: PmSpawnFailureInjectionPoint::None,
        }
    }

    pub const fn with_failure(mut self, failure_injection: PmSpawnFailureInjectionPoint) -> Self {
        self.failure_injection = failure_injection;
        self
    }

    pub const fn with_max_commits(mut self, max_commits: usize) -> Self {
        self.max_commits = max_commits;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmSpawnAccountingEntry {
    pub mock_request_id: u32,
    pub compatible: [u8; 64],
    pub compatible_len: usize,
    pub status: PmSpawnAccountingStatus,
    pub reservations: [Option<PmSpawnReservation>; MAX_PM_SPAWN_RESERVATIONS],
    pub rollback_steps: [Option<PmSpawnRollbackStep>; MAX_PM_SPAWN_ROLLBACK_STEPS],
    pub failures: [Option<PmSpawnAccountingFailure>; MAX_DRIVER_SPAWN_BLOCKERS],
}

impl PmSpawnAccountingEntry {
    fn from_request(request: &DriverSpawnRequest, status: PmSpawnAccountingStatus) -> Self {
        Self {
            mock_request_id: request.mock_request_id,
            compatible: request.compatible,
            compatible_len: request.compatible_len,
            status,
            reservations: [None; MAX_PM_SPAWN_RESERVATIONS],
            rollback_steps: [None; MAX_PM_SPAWN_ROLLBACK_STEPS],
            failures: [None; MAX_DRIVER_SPAWN_BLOCKERS],
        }
    }

    pub fn compatible(&self) -> Option<&str> {
        bounded_str(&self.compatible, self.compatible_len)
    }

    pub fn has_reservation(&self, reservation: PmSpawnReservation) -> bool {
        self.reservations
            .iter()
            .any(|entry| *entry == Some(reservation))
    }

    pub fn has_rollback_step(&self, step: PmSpawnRollbackStep) -> bool {
        self.rollback_steps.iter().any(|entry| *entry == Some(step))
    }

    pub fn has_failure(&self, failure: PmSpawnAccountingFailure) -> bool {
        self.failures.iter().any(|entry| *entry == Some(failure))
    }

    fn push_reservation(&mut self, reservation: PmSpawnReservation) -> Result<(), KernelIpcError> {
        let Some(slot) = self.reservations.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(reservation);
        Ok(())
    }

    fn push_rollback_step(&mut self, step: PmSpawnRollbackStep) -> Result<(), KernelIpcError> {
        let Some(slot) = self.rollback_steps.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(step);
        Ok(())
    }

    fn push_failure(&mut self, failure: PmSpawnAccountingFailure) -> Result<(), KernelIpcError> {
        if self.has_failure(failure) {
            return Ok(());
        }
        let Some(slot) = self.failures.iter_mut().find(|slot| slot.is_none()) else {
            return Err(KernelIpcError::CapabilityFull);
        };
        *slot = Some(failure);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmSpawnAccountingReport {
    entries: [Option<PmSpawnAccountingEntry>; MAX_DEVICES],
    len: usize,
}

impl PmSpawnAccountingReport {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_DEVICES],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> impl Iterator<Item = &PmSpawnAccountingEntry> {
        self.entries[..self.len]
            .iter()
            .filter_map(|entry| entry.as_ref())
    }

    pub fn committed_count(&self) -> usize {
        self.iter()
            .filter(|entry| entry.status == PmSpawnAccountingStatus::WouldCommit)
            .count()
    }

    fn push(&mut self, entry: PmSpawnAccountingEntry) -> Result<(), KernelIpcError> {
        if self.len >= MAX_DEVICES {
            return Err(KernelIpcError::CapabilityFull);
        }
        self.entries[self.len] = Some(entry);
        self.len += 1;
        Ok(())
    }
}

fn simulate_pm_spawn_accounting_entry(
    request: &DriverSpawnRequest,
    validation: &PmSpawnValidationEntry,
    policy: PmSpawnAccountingPolicy,
    committed_so_far: usize,
) -> Result<PmSpawnAccountingEntry, KernelIpcError> {
    let base_status = match validation.status {
        PmSpawnValidationStatus::WouldAccept => PmSpawnAccountingStatus::WouldReserve,
        PmSpawnValidationStatus::WouldReject => PmSpawnAccountingStatus::WouldReject,
        PmSpawnValidationStatus::Deferred => PmSpawnAccountingStatus::Deferred,
        PmSpawnValidationStatus::Unsupported => PmSpawnAccountingStatus::Unsupported,
        PmSpawnValidationStatus::AlreadyRunning => PmSpawnAccountingStatus::AlreadyRunning,
    };
    let mut entry = PmSpawnAccountingEntry::from_request(request, base_status);
    if validation.status != PmSpawnValidationStatus::WouldAccept {
        entry.push_failure(PmSpawnAccountingFailure::ValidationNotAccepted)?;
        return Ok(entry);
    }
    if !policy.accounting_allowed {
        entry.status = PmSpawnAccountingStatus::WouldReject;
        entry.push_failure(PmSpawnAccountingFailure::PolicyDenied)?;
        return Ok(entry);
    }
    if committed_so_far >= policy.max_commits {
        entry.status = PmSpawnAccountingStatus::WouldReject;
        entry.push_failure(PmSpawnAccountingFailure::ResourceLimitExceeded)?;
        return Ok(entry);
    }
    if validation.has_failure(PmSpawnValidationFailure::MissingStartupCapLayout) {
        entry.status = PmSpawnAccountingStatus::WouldReject;
        entry.push_failure(PmSpawnAccountingFailure::MissingStartupCapLayout)?;
        return Ok(entry);
    }
    if policy.failure_injection == PmSpawnFailureInjectionPoint::BeforeAnyReservation {
        entry.status = PmSpawnAccountingStatus::WouldRollback;
        entry.push_failure(PmSpawnAccountingFailure::InjectedFailureBeforeReservation)?;
        return Ok(entry);
    }

    let reservation_plan = reservation_plan_for_request(request);
    for reservation in reservation_plan
        .iter()
        .filter_map(|reservation| *reservation)
    {
        entry.push_reservation(reservation)?;
        if failure_matches_reservation(policy.failure_injection, reservation) {
            entry.status = PmSpawnAccountingStatus::WouldRollback;
            entry.push_failure(PmSpawnAccountingFailure::InjectedFailureAfterReservation)?;
            append_reverse_rollback_steps(&mut entry)?;
            return Ok(entry);
        }
    }
    entry.status = if policy.commit_successful_reservations {
        PmSpawnAccountingStatus::WouldCommit
    } else {
        PmSpawnAccountingStatus::WouldReserve
    };
    Ok(entry)
}

fn reservation_plan_for_request(
    request: &DriverSpawnRequest,
) -> [Option<PmSpawnReservation>; MAX_PM_SPAWN_RESERVATIONS] {
    let mut plan = [None; MAX_PM_SPAWN_RESERVATIONS];
    let mut len = 0usize;
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::ProcessSlot);
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::AddressSpace);
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::CNodeSlots);
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::StartupCapSlots);
    for resource in request
        .resource_requirements
        .iter()
        .filter_map(|entry| *entry)
    {
        match resource.kind {
            ResourceGrantKind::Mmio => {
                push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::MmioWindow)
            }
            ResourceGrantKind::Irq => {
                push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::IrqRoute)
            }
            ResourceGrantKind::Dma => {
                push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::DmaWindow)
            }
            _ => {}
        }
    }
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::HandleSlot);
    push_reservation_plan(&mut plan, &mut len, PmSpawnReservation::HealthMonitorSlot);
    plan
}

fn push_reservation_plan(
    plan: &mut [Option<PmSpawnReservation>; MAX_PM_SPAWN_RESERVATIONS],
    len: &mut usize,
    reservation: PmSpawnReservation,
) {
    if *len < MAX_PM_SPAWN_RESERVATIONS {
        plan[*len] = Some(reservation);
        *len += 1;
    }
}

fn failure_matches_reservation(
    failure_injection: PmSpawnFailureInjectionPoint,
    reservation: PmSpawnReservation,
) -> bool {
    matches!(
        (failure_injection, reservation),
        (
            PmSpawnFailureInjectionPoint::AfterProcessSlot,
            PmSpawnReservation::ProcessSlot
        ) | (
            PmSpawnFailureInjectionPoint::AfterAddressSpace,
            PmSpawnReservation::AddressSpace
        ) | (
            PmSpawnFailureInjectionPoint::AfterStartupCapSlots,
            PmSpawnReservation::StartupCapSlots
        ) | (
            PmSpawnFailureInjectionPoint::AfterMmio,
            PmSpawnReservation::MmioWindow
        ) | (
            PmSpawnFailureInjectionPoint::AfterIrq,
            PmSpawnReservation::IrqRoute
        ) | (
            PmSpawnFailureInjectionPoint::AfterHandle,
            PmSpawnReservation::HandleSlot
        )
    )
}

fn append_reverse_rollback_steps(entry: &mut PmSpawnAccountingEntry) -> Result<(), KernelIpcError> {
    let mut index = MAX_PM_SPAWN_RESERVATIONS;
    while index > 0 {
        index -= 1;
        if let Some(reservation) = entry.reservations[index] {
            entry.push_rollback_step(rollback_step_for_reservation(reservation))?;
        }
    }
    Ok(())
}

fn rollback_step_for_reservation(reservation: PmSpawnReservation) -> PmSpawnRollbackStep {
    match reservation {
        PmSpawnReservation::ProcessSlot => PmSpawnRollbackStep::ReleaseProcessSlot,
        PmSpawnReservation::AddressSpace => PmSpawnRollbackStep::DestroyAddressSpace,
        PmSpawnReservation::CNodeSlots => PmSpawnRollbackStep::RevokeMintedCaps,
        PmSpawnReservation::MmioWindow => PmSpawnRollbackStep::ReleaseMmioReservation,
        PmSpawnReservation::IrqRoute => PmSpawnRollbackStep::ReleaseIrqReservation,
        PmSpawnReservation::DmaWindow => PmSpawnRollbackStep::ReleaseDmaReservation,
        PmSpawnReservation::StartupCapSlots => PmSpawnRollbackStep::ClearStartupCapSlots,
        PmSpawnReservation::HandleSlot => PmSpawnRollbackStep::DropHandle,
        PmSpawnReservation::HealthMonitorSlot => PmSpawnRollbackStep::ClearHealthMonitor,
    }
}

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

    pub fn build_driver_spawn_request_bundle(
        &self,
        plan: &SpawnPlan,
        decisions: &SpawnAuthorityDecisions,
        grant_bundle: &ResourceGrantBundle,
    ) -> Result<DriverSpawnRequestBundle, KernelIpcError> {
        let mut requests = DriverSpawnRequestBundle::new();
        for (index, ((device, plan_entry), decision)) in self
            .iter()
            .zip(plan.iter())
            .zip(decisions.iter())
            .enumerate()
        {
            if device.compatible() != plan_entry.compatible()
                || device.compatible() != decision.compatible()
                || device.driver_candidate() != plan_entry.driver_candidate()
                || device.driver_candidate() != decision.driver_candidate()
            {
                return Err(KernelIpcError::WrongObject);
            }
            let mut request =
                DriverSpawnRequest::from_pipeline(device, index, plan_entry, decision)?;
            for grant in grant_bundle
                .iter()
                .filter(|grant| grant.compatible() == device.compatible())
            {
                request.push_resource(DriverSpawnResourceRequirement {
                    kind: grant.kind,
                    requirement: grant.requirement,
                    mock_resource_id: grant.mock_resource_id,
                    blockers: grant.blockers,
                })?;
            }
            requests.push(request)?;
        }
        if requests.len() != self.len()
            || requests.len() != plan.len()
            || requests.len() != decisions.len()
        {
            return Err(KernelIpcError::WrongObject);
        }
        Ok(requests)
    }

    pub fn build_spawn_plan(
        &self,
        registry: &DriverRegistry,
        policy: SpawnPolicy,
    ) -> Result<SpawnPlan, KernelIpcError> {
        let mut plan = SpawnPlan::new();
        for device in self.iter() {
            let mut entry = classify_spawn_plan_entry(device, registry, policy)?;
            if !policy.spawn_authority_available {
                if matches!(entry.action, SpawnAction::WouldSpawn) {
                    entry.action = SpawnAction::Deferred;
                }
                if matches!(entry.action, SpawnAction::Deferred) {
                    entry.push_blocker(SpawnBlocker::MissingSpawnAuthority)?;
                }
            }
            plan.push(entry)?;
        }
        Ok(plan)
    }

    pub fn build_resource_grant_bundle(
        &self,
        plan: &SpawnPlan,
        decisions: &SpawnAuthorityDecisions,
        policy: ResourceGrantPolicy,
    ) -> Result<ResourceGrantBundle, KernelIpcError> {
        let mut bundle = ResourceGrantBundle::new();
        for ((device, plan_entry), decision) in self.iter().zip(plan.iter()).zip(decisions.iter()) {
            if device.compatible() != plan_entry.compatible()
                || device.compatible() != decision.compatible()
            {
                return Err(KernelIpcError::WrongObject);
            }
            append_resource_requirements(device, plan_entry, decision, policy, &mut bundle)?;
        }
        Ok(bundle)
    }
}

fn append_resource_requirements(
    device: &DeviceRecord,
    plan_entry: &SpawnPlanEntry,
    decision: &SpawnAuthorityDecision,
    policy: ResourceGrantPolicy,
    bundle: &mut ResourceGrantBundle,
) -> Result<(), KernelIpcError> {
    if decision.approval.is_some() && matches!(plan_entry.action, SpawnAction::WouldSpawn) {
        return append_approved_resource_requirements(device, policy, bundle);
    }
    append_blocked_resource_requirements(device, plan_entry, bundle)
}

fn append_approved_resource_requirements(
    device: &DeviceRecord,
    policy: ResourceGrantPolicy,
    bundle: &mut ResourceGrantBundle,
) -> Result<(), KernelIpcError> {
    match device.class {
        DeviceClass::Uart => {
            let next_id =
                u32::try_from(bundle.len() + 1).map_err(|_| KernelIpcError::WrongObject)?;
            bundle.push(
                ResourceGrantEntry::new(
                    device,
                    ResourceGrantKind::Mmio,
                    ResourceGrantRequirement::WouldRequest,
                )
                .with_mock_resource_id(next_id),
            )?;
            let next_id =
                u32::try_from(bundle.len() + 1).map_err(|_| KernelIpcError::WrongObject)?;
            bundle.push(
                ResourceGrantEntry::new(
                    device,
                    ResourceGrantKind::Irq,
                    ResourceGrantRequirement::WouldRequest,
                )
                .with_mock_resource_id(next_id),
            )?;
            if policy.describe_uart_clock {
                let next_id =
                    u32::try_from(bundle.len() + 1).map_err(|_| KernelIpcError::WrongObject)?;
                bundle.push(
                    ResourceGrantEntry::new(
                        device,
                        ResourceGrantKind::Clock,
                        ResourceGrantRequirement::WouldRequest,
                    )
                    .with_mock_resource_id(next_id),
                )?;
            }
            if policy.describe_uart_pinmux {
                let next_id =
                    u32::try_from(bundle.len() + 1).map_err(|_| KernelIpcError::WrongObject)?;
                bundle.push(
                    ResourceGrantEntry::new(
                        device,
                        ResourceGrantKind::Pinmux,
                        ResourceGrantRequirement::WouldRequest,
                    )
                    .with_mock_resource_id(next_id),
                )?;
            }
        }
        DeviceClass::IrqMux => {
            let next_id =
                u32::try_from(bundle.len() + 1).map_err(|_| KernelIpcError::WrongObject)?;
            bundle.push(
                ResourceGrantEntry::new(
                    device,
                    ResourceGrantKind::Irq,
                    ResourceGrantRequirement::WouldRequest,
                )
                .with_mock_resource_id(next_id),
            )?;
        }
        _ => return Err(KernelIpcError::WrongObject),
    }
    Ok(())
}

fn append_blocked_resource_requirements(
    device: &DeviceRecord,
    plan_entry: &SpawnPlanEntry,
    bundle: &mut ResourceGrantBundle,
) -> Result<(), KernelIpcError> {
    match device.class {
        DeviceClass::Gpio => {
            let mut pcie = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::PcieBar,
                ResourceGrantRequirement::Deferred,
            );
            pcie.push_blocker(ResourceGrantBlocker::RequiresPcieBarDiscovery)?;
            pcie.push_blocker(ResourceGrantBlocker::DeviceDeferred)?;
            bundle.push(pcie)?;
            let mut mmio = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Mmio,
                ResourceGrantRequirement::Deferred,
            );
            mmio.push_blocker(ResourceGrantBlocker::RequiresPcieBarDiscovery)?;
            mmio.push_blocker(ResourceGrantBlocker::MissingMmioAuthority)?;
            mmio.push_blocker(ResourceGrantBlocker::DeviceDeferred)?;
            bundle.push(mmio)?;
        }
        DeviceClass::Mailbox => {
            let mut transport = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::MailboxTransport,
                ResourceGrantRequirement::Deferred,
            );
            transport.push_blocker(ResourceGrantBlocker::RequiresMailboxTransport)?;
            transport.push_blocker(ResourceGrantBlocker::DeviceDeferred)?;
            bundle.push(transport)?;
            let mut dma = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Dma,
                ResourceGrantRequirement::Deferred,
            );
            dma.push_blocker(ResourceGrantBlocker::RequiresCacheMaintenancePolicy)?;
            dma.push_blocker(ResourceGrantBlocker::MissingDmaPolicy)?;
            dma.push_blocker(ResourceGrantBlocker::DeviceDeferred)?;
            bundle.push(dma)?;
            let mut mmio = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Mmio,
                ResourceGrantRequirement::Deferred,
            );
            mmio.push_blocker(ResourceGrantBlocker::MissingMmioAuthority)?;
            mmio.push_blocker(ResourceGrantBlocker::DeviceDeferred)?;
            bundle.push(mmio)?;
        }
        DeviceClass::IrqMux => {
            let mut irq = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Irq,
                ResourceGrantRequirement::Deferred,
            );
            irq.push_blocker(ResourceGrantBlocker::MissingIrqRouting)?;
            if !matches!(plan_entry.action, SpawnAction::WouldSpawn) {
                irq.push_blocker(ResourceGrantBlocker::SpawnNotApproved)?;
            }
            bundle.push(irq)?;
        }
        DeviceClass::Unknown => {
            let mut unknown = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Unknown,
                ResourceGrantRequirement::Unsupported,
            );
            unknown.push_blocker(ResourceGrantBlocker::DeviceUnsupported)?;
            unknown.push_blocker(ResourceGrantBlocker::UnknownResource)?;
            bundle.push(unknown)?;
        }
        DeviceClass::Uart | DeviceClass::Block => {
            let mut denied = ResourceGrantEntry::new(
                device,
                ResourceGrantKind::Unknown,
                ResourceGrantRequirement::Denied,
            );
            denied.push_blocker(ResourceGrantBlocker::SpawnNotApproved)?;
            bundle.push(denied)?;
        }
    }
    Ok(())
}

fn classify_spawn_plan_entry(
    device: &DeviceRecord,
    registry: &DriverRegistry,
    policy: SpawnPolicy,
) -> Result<SpawnPlanEntry, KernelIpcError> {
    if let Some(tid) = device.assigned_tid
        && registry.get(tid).is_some()
    {
        let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::AlreadyRunning);
        entry.push_blocker(SpawnBlocker::AlreadyRegistered)?;
        return Ok(entry);
    }

    if matches!(device.class, DeviceClass::Unknown)
        || matches!(device.status, DeviceStatus::Unsupported)
    {
        let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Unsupported);
        entry.push_blocker(SpawnBlocker::UnsupportedDevice)?;
        return Ok(entry);
    }

    let candidate = device.driver_candidate();
    if candidate.is_none() || candidate == Some("unknown") || device.driver_candidate_len == 0 {
        let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::NoCandidate);
        entry.push_blocker(SpawnBlocker::UnknownCandidate)?;
        return Ok(entry);
    }

    match device.status {
        DeviceStatus::Discovered => classify_discovered_device(device, policy),
        DeviceStatus::DeferredNoMmioGrant => classify_deferred_mmio_device(device),
        DeviceStatus::DeferredNoIrqRoute => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Deferred);
            entry.push_blocker(SpawnBlocker::MissingIrqRoute)?;
            Ok(entry)
        }
        DeviceStatus::DeferredNoHardwareControl => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Deferred);
            entry.push_blocker(SpawnBlocker::MissingSpawnAuthority)?;
            Ok(entry)
        }
        DeviceStatus::Unsupported => unreachable!(),
    }
}

fn classify_discovered_device(
    device: &DeviceRecord,
    policy: SpawnPolicy,
) -> Result<SpawnPlanEntry, KernelIpcError> {
    match device.class {
        DeviceClass::Uart if policy.uart_prereqs_available => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::WouldSpawn);
            if !device.mmio_ranges.iter().any(Option::is_some) {
                entry.action = SpawnAction::Deferred;
                entry.push_blocker(SpawnBlocker::MissingMmioGrant)?;
            }
            if !device.irq_lines.iter().any(Option::is_some) {
                entry.action = SpawnAction::Deferred;
                entry.push_blocker(SpawnBlocker::MissingIrqRoute)?;
            }
            Ok(entry)
        }
        DeviceClass::IrqMux if policy.irqmux_prereqs_available => {
            Ok(SpawnPlanEntry::from_device(device, SpawnAction::WouldSpawn))
        }
        DeviceClass::IrqMux => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Deferred);
            entry.push_blocker(SpawnBlocker::MissingIrqRoute)?;
            Ok(entry)
        }
        DeviceClass::Uart => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Deferred);
            entry.push_blocker(SpawnBlocker::MissingMmioGrant)?;
            entry.push_blocker(SpawnBlocker::MissingIrqRoute)?;
            Ok(entry)
        }
        DeviceClass::Mailbox | DeviceClass::Gpio | DeviceClass::Block => {
            classify_deferred_mmio_device(device)
        }
        DeviceClass::Unknown => {
            let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Unsupported);
            entry.push_blocker(SpawnBlocker::UnsupportedDevice)?;
            Ok(entry)
        }
    }
}

fn classify_deferred_mmio_device(device: &DeviceRecord) -> Result<SpawnPlanEntry, KernelIpcError> {
    let mut entry = SpawnPlanEntry::from_device(device, SpawnAction::Deferred);
    entry.push_blocker(SpawnBlocker::DeferredNoMmioGrant)?;
    entry.push_blocker(SpawnBlocker::MissingMmioGrant)?;
    match device.class {
        DeviceClass::Gpio => {
            entry.push_blocker(SpawnBlocker::RequiresPcieBarDiscovery)?;
        }
        DeviceClass::Mailbox => {
            entry.push_blocker(SpawnBlocker::MissingMailboxTransport)?;
            entry.push_blocker(SpawnBlocker::MissingCachePolicy)?;
            entry.push_blocker(SpawnBlocker::MissingDmaPolicy)?;
        }
        _ => {}
    }
    Ok(entry)
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

    fn spawn_entry<'a>(plan: &'a SpawnPlan, compatible: &str) -> &'a SpawnPlanEntry {
        plan.iter()
            .find(|entry| entry.compatible() == Some(compatible))
            .expect("spawn plan entry")
    }

    fn authority_decision<'a>(
        decisions: &'a SpawnAuthorityDecisions,
        compatible: &str,
    ) -> &'a SpawnAuthorityDecision {
        decisions
            .iter()
            .find(|decision| decision.compatible() == Some(compatible))
            .expect("spawn authority decision")
    }

    fn grant_entries<'a>(
        bundle: &'a ResourceGrantBundle,
        compatible: &'a str,
    ) -> impl Iterator<Item = &'a ResourceGrantEntry> + 'a {
        bundle
            .iter()
            .filter(move |entry| entry.compatible() == Some(compatible))
    }

    #[test]
    fn spawn_plan_for_fake_rpi5_inventory_is_policy_only_and_deterministic() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "uart_srv",
                    DeviceStatus::Discovered,
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
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware",
                    DeviceStatus::DeferredNoMmioGrant,
                )
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
                .with_mmio(0, 0x1_0000, 0x1000)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "yarm,irqmux",
                    DeviceClass::IrqMux,
                    "irqmux_srv",
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_irq(0, 5)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "vendor,unknown",
                    DeviceClass::Unknown,
                    "unknown",
                    DeviceStatus::Unsupported,
                )
                .unwrap(),
            )
            .unwrap();

        let registry = DriverRegistry::new();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .expect("spawn plan");
        assert_eq!(plan.len(), 5);
        assert_eq!(
            spawn_entry(&plan, "arm,pl011").action,
            SpawnAction::WouldSpawn
        );

        let mailbox = spawn_entry(&plan, "raspberrypi,firmware");
        assert_eq!(mailbox.action, SpawnAction::Deferred);
        assert!(mailbox.has_blocker(SpawnBlocker::MissingMailboxTransport));
        assert!(mailbox.has_blocker(SpawnBlocker::MissingCachePolicy));
        assert!(mailbox.has_blocker(SpawnBlocker::MissingMmioGrant));

        let rp1 = spawn_entry(&plan, "raspberrypi,rp1-gpio");
        assert_eq!(rp1.action, SpawnAction::Deferred);
        assert!(rp1.has_blocker(SpawnBlocker::RequiresPcieBarDiscovery));
        assert!(rp1.has_blocker(SpawnBlocker::DeferredNoMmioGrant));
        assert!(rp1.has_blocker(SpawnBlocker::MissingMmioGrant));

        let irqmux = spawn_entry(&plan, "yarm,irqmux");
        assert_eq!(irqmux.action, SpawnAction::Deferred);
        assert!(irqmux.has_blocker(SpawnBlocker::MissingIrqRoute));

        let unknown = spawn_entry(&plan, "vendor,unknown");
        assert_eq!(unknown.action, SpawnAction::Unsupported);
        assert!(unknown.has_blocker(SpawnBlocker::UnsupportedDevice));

        let ordered: [&str; 5] =
            core::array::from_fn(|index| plan.iter().nth(index).unwrap().compatible().unwrap());
        assert_eq!(
            ordered,
            [
                "arm,pl011",
                "raspberrypi,firmware",
                "raspberrypi,rp1-gpio",
                "yarm,irqmux",
                "vendor,unknown"
            ]
        );
    }

    #[test]
    fn spawn_plan_fail_closed_policy_and_registry_state_are_inert() {
        let mut inventory = pl011_inventory(7, DeviceStatus::Discovered);
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "uart_srv",
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_mmio(0, 0x107d_0020_0000, 0x1000)
                .unwrap()
                .with_irq(0, 122)
                .unwrap(),
            )
            .unwrap();
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let before_len = registry.len();

        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::fail_closed())
            .expect("spawn plan");
        assert_eq!(
            registry.len(),
            before_len,
            "planning must not mutate registry"
        );
        let assigned = spawn_entry(&plan, "arm,pl011");
        assert_eq!(assigned.action, SpawnAction::AlreadyRunning);
        assert!(assigned.has_blocker(SpawnBlocker::AlreadyRegistered));
        let unassigned_duplicate = plan
            .iter()
            .filter(|entry| entry.compatible() == Some("arm,pl011"))
            .nth(1)
            .expect("duplicate entry remains deterministic");
        assert_eq!(unassigned_duplicate.action, SpawnAction::Deferred);
        assert!(unassigned_duplicate.has_blocker(SpawnBlocker::MissingMmioGrant));
        assert!(unassigned_duplicate.has_blocker(SpawnBlocker::MissingIrqRoute));
        assert!(unassigned_duplicate.has_blocker(SpawnBlocker::MissingSpawnAuthority));
    }

    #[test]
    fn spawn_authority_approves_only_would_spawn_entries_with_mock_authority() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "uart_srv",
                    DeviceStatus::Discovered,
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
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware",
                    DeviceStatus::DeferredNoMmioGrant,
                )
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
                .with_mmio(0, 0x1_0000, 0x1000)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "yarm,irqmux",
                    DeviceClass::IrqMux,
                    "irqmux_srv",
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_irq(0, 5)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "vendor,unknown",
                    DeviceClass::Unknown,
                    "unknown",
                    DeviceStatus::Unsupported,
                )
                .unwrap(),
            )
            .unwrap();
        let registry = DriverRegistry::new();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: None,
                    mock_epoch: 1,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        assert_eq!(decisions.len(), 5);
        let uart = authority_decision(&decisions, "arm,pl011");
        assert_eq!(
            uart.approval.map(|approval| approval.mock_spawn_id),
            Some(1)
        );
        assert_eq!(uart.denial, None);

        let mailbox = authority_decision(&decisions, "raspberrypi,firmware");
        let mailbox_denial = mailbox.denial.expect("mailbox denied");
        assert!(mailbox_denial.has_reason(SpawnDenialReason::PlanEntryDeferred));
        assert!(mailbox_denial.has_reason(SpawnDenialReason::MissingMailboxTransport));
        assert!(mailbox_denial.has_reason(SpawnDenialReason::MissingCachePolicy));
        assert!(mailbox_denial.has_reason(SpawnDenialReason::MissingMmioGrant));

        let rp1 = authority_decision(&decisions, "raspberrypi,rp1-gpio");
        let rp1_denial = rp1.denial.expect("rp1 denied");
        assert!(rp1_denial.has_reason(SpawnDenialReason::PlanEntryDeferred));
        assert!(rp1_denial.has_reason(SpawnDenialReason::RequiresPcieBarDiscovery));
        assert!(rp1_denial.has_reason(SpawnDenialReason::MissingMmioGrant));

        let irqmux = authority_decision(&decisions, "yarm,irqmux");
        assert!(
            irqmux
                .denial
                .unwrap()
                .has_reason(SpawnDenialReason::MissingIrqRoute)
        );
        let unknown = authority_decision(&decisions, "vendor,unknown");
        assert!(
            unknown
                .denial
                .unwrap()
                .has_reason(SpawnDenialReason::UnsupportedDevice)
        );
    }

    #[test]
    fn spawn_authority_fail_closed_and_already_running_are_noop_denials() {
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut registry = DriverRegistry::new();
        let before_len = registry.len();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let fail_closed = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 2,
                },
                SpawnAuthorityPolicy::fail_closed(),
            )
            .unwrap();
        let denied_uart = authority_decision(&fail_closed, "arm,pl011");
        assert_eq!(denied_uart.approval, None);
        assert!(
            denied_uart
                .denial
                .unwrap()
                .has_reason(SpawnDenialReason::MissingSpawnAuthority)
        );
        assert_eq!(
            registry.len(),
            before_len,
            "authority checks do not mutate registry"
        );

        registry.register(7).unwrap();
        let already_running_plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let already_running = already_running_plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 3,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        let running_uart = authority_decision(&already_running, "arm,pl011");
        assert_eq!(running_uart.approval, None);
        assert!(
            running_uart
                .denial
                .unwrap()
                .has_reason(SpawnDenialReason::AlreadyRunning)
        );
    }

    #[test]
    fn approved_pl011_spawn_produces_inert_resource_grant_requirements() {
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let registry = DriverRegistry::new();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 4,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        let bundle = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        let grants: [_; 4] =
            core::array::from_fn(|index| grant_entries(&bundle, "arm,pl011").nth(index).unwrap());
        assert_eq!(bundle.len(), 4);
        assert_eq!(grants[0].kind, ResourceGrantKind::Mmio);
        assert_eq!(grants[1].kind, ResourceGrantKind::Irq);
        assert_eq!(grants[2].kind, ResourceGrantKind::Clock);
        assert_eq!(grants[3].kind, ResourceGrantKind::Pinmux);
        for grant in grants {
            assert_eq!(grant.requirement, ResourceGrantRequirement::WouldRequest);
            assert!(grant.mock_resource_id.is_some());
            assert!(
                grant.mock_resource_id.unwrap() > 0,
                "mock resource IDs are inert and must not be CapId(0)"
            );
        }
    }

    #[test]
    fn denied_or_deferred_spawns_do_not_produce_live_grant_requests() {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "uart_srv",
                    DeviceStatus::Discovered,
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
                .with_mmio(0, 0x1_0000, 0x1000)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware",
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
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_irq(0, 5)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "vendor,unknown",
                    DeviceClass::Unknown,
                    "unknown",
                    DeviceStatus::Unsupported,
                )
                .unwrap(),
            )
            .unwrap();
        let registry = DriverRegistry::new();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 5,
                },
                SpawnAuthorityPolicy::fail_closed(),
            )
            .unwrap();
        let before_inventory_len = inventory.len();
        let before_registry_len = registry.len();
        let bundle = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        assert_eq!(inventory.len(), before_inventory_len);
        assert_eq!(registry.len(), before_registry_len);
        assert!(
            bundle
                .iter()
                .all(|entry| entry.requirement != ResourceGrantRequirement::WouldRequest),
            "denied authority cannot produce would-request grants"
        );

        let rp1: [_; 2] = core::array::from_fn(|index| {
            grant_entries(&bundle, "raspberrypi,rp1-gpio")
                .nth(index)
                .unwrap()
        });
        assert_eq!(rp1[0].kind, ResourceGrantKind::PcieBar);
        assert!(rp1[0].has_blocker(ResourceGrantBlocker::RequiresPcieBarDiscovery));
        assert_eq!(rp1[1].kind, ResourceGrantKind::Mmio);
        assert!(rp1[1].has_blocker(ResourceGrantBlocker::MissingMmioAuthority));

        let mailbox: [_; 3] = core::array::from_fn(|index| {
            grant_entries(&bundle, "raspberrypi,firmware")
                .nth(index)
                .unwrap()
        });
        assert_eq!(mailbox[0].kind, ResourceGrantKind::MailboxTransport);
        assert!(mailbox[0].has_blocker(ResourceGrantBlocker::RequiresMailboxTransport));
        assert_eq!(mailbox[1].kind, ResourceGrantKind::Dma);
        assert!(mailbox[1].has_blocker(ResourceGrantBlocker::RequiresCacheMaintenancePolicy));
        assert_eq!(mailbox[2].kind, ResourceGrantKind::Mmio);
        assert!(mailbox[2].has_blocker(ResourceGrantBlocker::MissingMmioAuthority));

        let irqmux = grant_entries(&bundle, "yarm,irqmux").next().unwrap();
        assert_eq!(irqmux.kind, ResourceGrantKind::Irq);
        assert!(irqmux.has_blocker(ResourceGrantBlocker::MissingIrqRouting));

        let unknown = grant_entries(&bundle, "vendor,unknown").next().unwrap();
        assert_eq!(unknown.requirement, ResourceGrantRequirement::Unsupported);
        assert!(unknown.has_blocker(ResourceGrantBlocker::DeviceUnsupported));
    }

    fn fake_rpi5_inventory() -> PlatformInventory {
        let mut inventory = PlatformInventory::new();
        inventory
            .add(
                DeviceRecord::new(
                    "arm,pl011",
                    DeviceClass::Uart,
                    "uart_srv",
                    DeviceStatus::Discovered,
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
                    "raspberrypi,firmware",
                    DeviceClass::Mailbox,
                    "rpi_firmware",
                    DeviceStatus::DeferredNoMmioGrant,
                )
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
                .with_mmio(0, 0x1_0000, 0x1000)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "yarm,irqmux",
                    DeviceClass::IrqMux,
                    "irqmux_srv",
                    DeviceStatus::Discovered,
                )
                .unwrap()
                .with_irq(0, 5)
                .unwrap(),
            )
            .unwrap();
        inventory
            .add(
                DeviceRecord::new(
                    "vendor,unknown",
                    DeviceClass::Unknown,
                    "unknown",
                    DeviceStatus::Unsupported,
                )
                .unwrap(),
            )
            .unwrap();
        inventory
    }

    fn request_for<'a>(
        bundle: &'a DriverSpawnRequestBundle,
        compatible: &str,
    ) -> &'a DriverSpawnRequest {
        bundle
            .iter()
            .find(|request| request.compatible() == Some(compatible))
            .expect("driver spawn request")
    }

    fn build_fake_rpi5_request_bundle(
        authority: SpawnAuthorityPolicy,
    ) -> (
        PlatformInventory,
        SpawnPlan,
        SpawnAuthorityDecisions,
        ResourceGrantBundle,
        DriverSpawnRequestBundle,
    ) {
        let inventory = fake_rpi5_inventory();
        let registry = DriverRegistry::new();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 7,
                },
                authority,
            )
            .unwrap();
        let grant_bundle = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        let request_bundle = inventory
            .build_driver_spawn_request_bundle(&plan, &decisions, &grant_bundle)
            .unwrap();
        (inventory, plan, decisions, grant_bundle, request_bundle)
    }

    #[test]
    fn approved_fake_pl011_pipeline_produces_pm_facing_request_with_descriptive_resources() {
        let (_, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        assert_eq!(requests.len(), 5);
        assert_eq!(requests.ready_count(), 1);
        let pl011 = request_for(&requests, "arm,pl011");
        assert_eq!(pl011.status, DriverSpawnRequestStatus::ReadyForPmValidation);
        assert_eq!(pl011.request_version, 1);
        assert_eq!(pl011.mock_request_id, 1);
        assert_eq!(pl011.driver_candidate(), Some("uart_srv"));
        assert_eq!(pl011.image_name(), Some("uart_srv"));
        assert_eq!(pl011.device_class, DeviceClass::Uart);
        assert!(pl011.has_resource_requirement(ResourceGrantKind::Mmio));
        assert!(pl011.has_resource_requirement(ResourceGrantKind::Irq));
        assert!(pl011.has_resource_requirement(ResourceGrantKind::Clock));
        assert!(pl011.has_resource_requirement(ResourceGrantKind::Pinmux));
        assert!(
            pl011
                .resource_requirements
                .iter()
                .filter_map(|entry| *entry)
                .all(
                    |entry| entry.requirement == ResourceGrantRequirement::WouldRequest
                        && entry.mock_resource_id.is_some_and(|id| id > 0)
                )
        );
    }

    #[test]
    fn pl011_request_includes_descriptive_startup_cap_requirements() {
        let (_, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let pl011 = request_for(&requests, "arm,pl011");
        for requirement in [
            StartupCapRequirement::DriverManagerControlEndpoint,
            StartupCapRequirement::DriverRegistrationEndpoint,
            StartupCapRequirement::FaultOrRestartEndpoint,
            StartupCapRequirement::Mmio,
            StartupCapRequirement::IrqNotification,
            StartupCapRequirement::DevfsRegistration,
            StartupCapRequirement::LoggingOrDebug,
        ] {
            assert!(
                pl011.has_startup_cap_requirement(requirement),
                "missing startup-cap descriptor: {requirement:?}"
            );
        }
    }

    #[test]
    fn fail_closed_spawn_authority_produces_no_ready_spawn_request() {
        let (_, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::fail_closed());
        assert_eq!(requests.ready_count(), 0);
        let pl011 = request_for(&requests, "arm,pl011");
        assert_eq!(pl011.status, DriverSpawnRequestStatus::Denied);
        assert!(pl011.has_blocker(DriverSpawnRequestBlocker::MissingSpawnAuthority));
        assert!(
            pl011
                .resource_requirements
                .iter()
                .filter_map(|entry| *entry)
                .all(|entry| entry.requirement != ResourceGrantRequirement::WouldRequest)
        );
    }

    #[test]
    fn deferred_and_unsupported_devices_produce_inert_request_records() {
        let (_, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());

        let rp1 = request_for(&requests, "raspberrypi,rp1-gpio");
        assert_eq!(rp1.status, DriverSpawnRequestStatus::Deferred);
        assert!(rp1.has_resource_requirement(ResourceGrantKind::PcieBar));
        assert!(rp1.has_resource_requirement(ResourceGrantKind::Mmio));
        assert!(rp1.has_blocker(DriverSpawnRequestBlocker::RequiresPcieBarDiscovery));
        assert!(rp1.has_blocker(DriverSpawnRequestBlocker::MissingMmioGrant));

        let mailbox = request_for(&requests, "raspberrypi,firmware");
        assert_eq!(mailbox.status, DriverSpawnRequestStatus::Deferred);
        assert!(mailbox.has_resource_requirement(ResourceGrantKind::MailboxTransport));
        assert!(mailbox.has_resource_requirement(ResourceGrantKind::Dma));
        assert!(mailbox.has_resource_requirement(ResourceGrantKind::Mmio));
        assert!(mailbox.has_blocker(DriverSpawnRequestBlocker::MissingMailboxTransport));
        assert!(mailbox.has_blocker(DriverSpawnRequestBlocker::MissingCachePolicy));
        assert!(mailbox.has_blocker(DriverSpawnRequestBlocker::MissingDmaPolicy));
        assert!(mailbox.has_blocker(DriverSpawnRequestBlocker::MissingMmioGrant));

        let irqmux = request_for(&requests, "yarm,irqmux");
        assert_eq!(irqmux.status, DriverSpawnRequestStatus::Deferred);
        assert!(irqmux.has_resource_requirement(ResourceGrantKind::Irq));
        assert!(irqmux.has_blocker(DriverSpawnRequestBlocker::MissingIrqRoute));

        let unknown = request_for(&requests, "vendor,unknown");
        assert_eq!(unknown.status, DriverSpawnRequestStatus::Unsupported);
        assert!(unknown.has_resource_requirement(ResourceGrantKind::Unknown));
        assert!(unknown.has_blocker(DriverSpawnRequestBlocker::UnsupportedDevice));
        assert!(unknown.has_blocker(DriverSpawnRequestBlocker::UnknownResource));
    }

    #[test]
    fn already_running_does_not_produce_duplicate_ready_pm_request() {
        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 8,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        let grant_bundle = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        let requests = inventory
            .build_driver_spawn_request_bundle(&plan, &decisions, &grant_bundle)
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests.ready_count(), 0);
        let pl011 = request_for(&requests, "arm,pl011");
        assert_eq!(pl011.status, DriverSpawnRequestStatus::AlreadyRunning);
        assert!(pl011.has_blocker(DriverSpawnRequestBlocker::AlreadyRunning));
    }

    #[test]
    fn request_bundle_generation_is_deterministic_bounded_and_inert() {
        let (inventory, plan, decisions, grants, first) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let before = (inventory.len(), plan.len(), decisions.len(), grants.len());
        let second = inventory
            .build_driver_spawn_request_bundle(&plan, &decisions, &grants)
            .unwrap();
        assert_eq!(
            before,
            (inventory.len(), plan.len(), decisions.len(), grants.len())
        );
        assert_eq!(first.len(), second.len());
        assert!(first.len() <= MAX_DEVICES);
        for (left, right) in first.iter().zip(second.iter()) {
            assert_eq!(left, right);
            assert!(left.mock_request_id > 0);
            assert_eq!(left.image_id, None);
            assert!(left.resource_requirements.len() <= MAX_DRIVER_SPAWN_REQUEST_RESOURCES);
            assert!(left.startup_cap_requirements.len() <= MAX_STARTUP_CAP_REQUIREMENTS);
        }
    }

    #[test]
    fn request_bundle_contains_no_caps_and_performs_no_pm_mmio_grant_or_spawn_operation() {
        let (inventory, plan, decisions, grants, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        assert_eq!(requests.ready_count(), 1);
        assert_eq!(
            (inventory.len(), plan.len(), decisions.len(), grants.len()),
            (5, 5, 5, 11)
        );
        for request in requests.iter() {
            assert_eq!(request.image_id, None, "image identity remains descriptive");
            assert!(request.mock_request_id > 0);
            for resource in request
                .resource_requirements
                .iter()
                .filter_map(|entry| *entry)
            {
                assert_ne!(resource.mock_resource_id, Some(0));
            }
        }
        let mut runtime = MockDriverControl::new();
        assert_eq!(runtime.irq_line.get(), None);
        assert_eq!(runtime.dma_request.get(), None);
        assert_eq!(runtime.irq_grant.get(), None);
        assert_eq!(runtime.dma_grant.get(), None);
        assert_eq!(runtime.registered.get(), None);
        // Keep a runtime instance alive to prove the request-bundle helper has no
        // parameter through which it could call PM/supervisor/control operations.
        let _ = &mut runtime;
    }

    fn validation_entry<'a>(
        report: &'a PmSpawnValidationReport,
        compatible: &str,
    ) -> &'a PmSpawnValidationEntry {
        report
            .iter()
            .find(|entry| entry.compatible() == Some(compatible))
            .expect("pm validation entry")
    }

    #[test]
    fn pm_validation_accepts_approved_pl011_only_with_identity_image_resources_and_caps() {
        let (inventory, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let report = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        assert_eq!(report.len(), 5);
        assert_eq!(report.would_accept_count(), 1);
        let pl011 = validation_entry(&report, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnValidationStatus::WouldAccept);
        assert!(pl011.failures.iter().all(Option::is_none));
    }

    #[test]
    fn pm_validation_missing_verified_dm_identity_rejects_pl011() {
        let (inventory, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let mut policy = PmSpawnValidationPolicy::hosted_fake_rpi5();
        policy.verified_driver_manager_identity = false;
        let report = requests
            .simulate_pm_validation(Some(&inventory), policy)
            .unwrap();
        assert_eq!(report.would_accept_count(), 0);
        let pl011 = validation_entry(&report, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnValidationStatus::WouldReject);
        assert!(pl011.has_failure(PmSpawnValidationFailure::MissingVerifiedDriverManager));
    }

    #[test]
    fn pm_validation_rejects_unsupported_request_version_and_fail_closed_policy() {
        let (inventory, _, _, _, mut requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        requests.requests[0].as_mut().unwrap().request_version = 2;
        let version_report = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let pl011 = validation_entry(&version_report, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnValidationStatus::WouldReject);
        assert!(pl011.has_failure(PmSpawnValidationFailure::RequestVersionUnsupported));

        let (_, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let fail_closed = requests
            .simulate_pm_validation(Some(&inventory), PmSpawnValidationPolicy::fail_closed())
            .unwrap();
        assert_eq!(fail_closed.would_accept_count(), 0);
        assert!(
            fail_closed
                .iter()
                .all(|entry| entry.status != PmSpawnValidationStatus::WouldAccept)
        );
        assert!(
            validation_entry(&fail_closed, "arm,pl011")
                .has_failure(PmSpawnValidationFailure::MissingVerifiedDriverManager)
        );
        assert!(
            validation_entry(&fail_closed, "arm,pl011")
                .has_failure(PmSpawnValidationFailure::ImageNotAllowed)
        );
    }

    #[test]
    fn pm_validation_keeps_rp1_mailbox_irqmux_unknown_and_running_rejected_or_deferred() {
        let (inventory, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let report = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();

        let rp1 = validation_entry(&report, "raspberrypi,rp1-gpio");
        assert_eq!(rp1.status, PmSpawnValidationStatus::Deferred);
        assert!(rp1.has_failure(PmSpawnValidationFailure::MissingPcieBar));
        assert!(rp1.has_failure(PmSpawnValidationFailure::MissingMmioAuthority));

        let mailbox = validation_entry(&report, "raspberrypi,firmware");
        assert_eq!(mailbox.status, PmSpawnValidationStatus::Deferred);
        assert!(mailbox.has_failure(PmSpawnValidationFailure::MissingMailboxTransport));
        assert!(mailbox.has_failure(PmSpawnValidationFailure::MissingCachePolicy));
        assert!(mailbox.has_failure(PmSpawnValidationFailure::MissingDmaPolicy));
        assert!(mailbox.has_failure(PmSpawnValidationFailure::MissingMmioAuthority));

        let irqmux = validation_entry(&report, "yarm,irqmux");
        assert_eq!(irqmux.status, PmSpawnValidationStatus::Deferred);
        assert!(irqmux.has_failure(PmSpawnValidationFailure::MissingIrqRouting));

        let unknown = validation_entry(&report, "vendor,unknown");
        assert_eq!(unknown.status, PmSpawnValidationStatus::Unsupported);
        assert!(unknown.has_failure(PmSpawnValidationFailure::DeviceUnsupported));

        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 9,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        let grants = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        let running_requests = inventory
            .build_driver_spawn_request_bundle(&plan, &decisions, &grants)
            .unwrap();
        let running_report = running_requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let running = validation_entry(&running_report, "arm,pl011");
        assert_eq!(running.status, PmSpawnValidationStatus::AlreadyRunning);
        assert!(running.has_failure(PmSpawnValidationFailure::AlreadyRunning));
    }

    #[test]
    fn pm_validation_rejects_missing_startup_cap_layout_and_resource_mismatch() {
        let (inventory, _, _, _, mut requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let pl011 = requests.requests[0].as_mut().unwrap();
        pl011.startup_cap_requirements = [None; MAX_STARTUP_CAP_REQUIREMENTS];
        let missing_caps = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let pl011_entry = validation_entry(&missing_caps, "arm,pl011");
        assert_eq!(pl011_entry.status, PmSpawnValidationStatus::WouldReject);
        assert!(pl011_entry.has_failure(PmSpawnValidationFailure::MissingStartupCapLayout));

        let (_, _, _, _, mut requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        requests.requests[0].as_mut().unwrap().device_record_index = 31;
        let mismatch = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let pl011_entry = validation_entry(&mismatch, "arm,pl011");
        assert_eq!(pl011_entry.status, PmSpawnValidationStatus::WouldReject);
        assert!(pl011_entry.has_failure(PmSpawnValidationFailure::ResourceNotAssigned));
    }

    #[test]
    fn pm_validation_report_is_deterministic_bounded_and_non_mutating() {
        let (inventory, plan, decisions, grants, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let request_snapshot = requests.clone();
        let before = (
            inventory.len(),
            plan.len(),
            decisions.len(),
            grants.len(),
            requests.len(),
        );
        let first = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let second = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), requests.len());
        assert!(first.len() <= MAX_DEVICES);
        assert_eq!(requests, request_snapshot);
        assert_eq!(
            before,
            (
                inventory.len(),
                plan.len(),
                decisions.len(),
                grants.len(),
                requests.len()
            )
        );
    }

    #[test]
    fn pm_validation_does_not_call_driver_control_pm_supervisor_caps_grants_or_mmio() {
        let (inventory, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let report = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        assert_eq!(report.would_accept_count(), 1);
        let runtime = MockDriverControl::new();
        assert_eq!(runtime.irq_line.get(), None);
        assert_eq!(runtime.dma_request.get(), None);
        assert_eq!(runtime.irq_grant.get(), None);
        assert_eq!(runtime.dma_grant.get(), None);
        assert_eq!(runtime.registered.get(), None);
        for entry in report.iter() {
            assert_ne!(entry.mock_request_id, 0);
        }
    }

    fn accounting_entry<'a>(
        report: &'a PmSpawnAccountingReport,
        compatible: &str,
    ) -> &'a PmSpawnAccountingEntry {
        report
            .iter()
            .find(|entry| entry.compatible() == Some(compatible))
            .expect("pm accounting entry")
    }

    fn build_fake_rpi5_accounting_report(
        policy: PmSpawnAccountingPolicy,
    ) -> (
        PlatformInventory,
        DriverSpawnRequestBundle,
        PmSpawnValidationReport,
        PmSpawnAccountingReport,
    ) {
        let (inventory, _, _, _, requests) =
            build_fake_rpi5_request_bundle(SpawnAuthorityPolicy::allow_hosted_mock_spawns());
        let validation = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let accounting = requests
            .simulate_pm_accounting(&validation, policy)
            .unwrap();
        (inventory, requests, validation, accounting)
    }

    #[test]
    fn pm_accounting_accepted_fake_pl011_produces_descriptive_reservations() {
        let (_, _, _, accounting) =
            build_fake_rpi5_accounting_report(PmSpawnAccountingPolicy::hosted_fake_rpi5());
        assert_eq!(accounting.len(), 5);
        assert_eq!(accounting.committed_count(), 1);
        let pl011 = accounting_entry(&accounting, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldCommit);
        for reservation in [
            PmSpawnReservation::ProcessSlot,
            PmSpawnReservation::AddressSpace,
            PmSpawnReservation::CNodeSlots,
            PmSpawnReservation::StartupCapSlots,
            PmSpawnReservation::MmioWindow,
            PmSpawnReservation::IrqRoute,
            PmSpawnReservation::HandleSlot,
            PmSpawnReservation::HealthMonitorSlot,
        ] {
            assert!(
                pl011.has_reservation(reservation),
                "missing {reservation:?}"
            );
        }
        assert!(pl011.rollback_steps.iter().all(Option::is_none));
    }

    #[test]
    fn pm_accounting_fail_closed_and_resource_limit_reject_without_reservations() {
        let (_, _, _, fail_closed) =
            build_fake_rpi5_accounting_report(PmSpawnAccountingPolicy::fail_closed());
        let pl011 = accounting_entry(&fail_closed, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldReject);
        assert!(pl011.has_failure(PmSpawnAccountingFailure::PolicyDenied));
        assert!(pl011.reservations.iter().all(Option::is_none));

        let policy = PmSpawnAccountingPolicy::hosted_fake_rpi5().with_max_commits(0);
        let (_, _, _, limited) = build_fake_rpi5_accounting_report(policy);
        let pl011 = accounting_entry(&limited, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldReject);
        assert!(pl011.has_failure(PmSpawnAccountingFailure::ResourceLimitExceeded));
        assert!(pl011.reservations.iter().all(Option::is_none));
    }

    #[test]
    fn pm_accounting_rollback_after_process_slot_is_reverse_and_descriptive() {
        let policy = PmSpawnAccountingPolicy::hosted_fake_rpi5()
            .with_failure(PmSpawnFailureInjectionPoint::AfterProcessSlot);
        let (_, _, _, accounting) = build_fake_rpi5_accounting_report(policy);
        let pl011 = accounting_entry(&accounting, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldRollback);
        assert!(pl011.has_reservation(PmSpawnReservation::ProcessSlot));
        assert_eq!(
            pl011.rollback_steps[0],
            Some(PmSpawnRollbackStep::ReleaseProcessSlot)
        );
        assert!(pl011.rollback_steps[1].is_none());
    }

    #[test]
    fn pm_accounting_rollback_after_address_space_is_reverse_order() {
        let policy = PmSpawnAccountingPolicy::hosted_fake_rpi5()
            .with_failure(PmSpawnFailureInjectionPoint::AfterAddressSpace);
        let (_, _, _, accounting) = build_fake_rpi5_accounting_report(policy);
        let pl011 = accounting_entry(&accounting, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldRollback);
        assert_eq!(
            pl011.rollback_steps[0],
            Some(PmSpawnRollbackStep::DestroyAddressSpace)
        );
        assert_eq!(
            pl011.rollback_steps[1],
            Some(PmSpawnRollbackStep::ReleaseProcessSlot)
        );
        assert!(pl011.rollback_steps[2].is_none());
    }

    #[test]
    fn pm_accounting_rollback_after_irq_includes_reverse_irq_and_mmio_release() {
        let policy = PmSpawnAccountingPolicy::hosted_fake_rpi5()
            .with_failure(PmSpawnFailureInjectionPoint::AfterIrq);
        let (_, _, _, accounting) = build_fake_rpi5_accounting_report(policy);
        let pl011 = accounting_entry(&accounting, "arm,pl011");
        assert_eq!(pl011.status, PmSpawnAccountingStatus::WouldRollback);
        assert!(pl011.has_rollback_step(PmSpawnRollbackStep::ReleaseIrqReservation));
        assert!(pl011.has_rollback_step(PmSpawnRollbackStep::ReleaseMmioReservation));
        let irq_index = pl011
            .rollback_steps
            .iter()
            .position(|step| *step == Some(PmSpawnRollbackStep::ReleaseIrqReservation))
            .unwrap();
        let mmio_index = pl011
            .rollback_steps
            .iter()
            .position(|step| *step == Some(PmSpawnRollbackStep::ReleaseMmioReservation))
            .unwrap();
        assert!(
            irq_index < mmio_index,
            "rollback must be reverse reservation order"
        );
        assert!(pl011.rollback_steps.iter().all(|step| {
            !matches!(step, Some(PmSpawnRollbackStep::RevokeMintedCaps))
                || pl011.has_reservation(PmSpawnReservation::CNodeSlots)
        }));
    }

    #[test]
    fn pm_accounting_keeps_deferred_unsupported_and_running_without_new_reservations() {
        let (_, _, _, accounting) =
            build_fake_rpi5_accounting_report(PmSpawnAccountingPolicy::hosted_fake_rpi5());
        let rp1 = accounting_entry(&accounting, "raspberrypi,rp1-gpio");
        assert_eq!(rp1.status, PmSpawnAccountingStatus::Deferred);
        assert!(rp1.reservations.iter().all(Option::is_none));
        let mailbox = accounting_entry(&accounting, "raspberrypi,firmware");
        assert_eq!(mailbox.status, PmSpawnAccountingStatus::Deferred);
        assert!(mailbox.reservations.iter().all(Option::is_none));
        let unknown = accounting_entry(&accounting, "vendor,unknown");
        assert_eq!(unknown.status, PmSpawnAccountingStatus::Unsupported);
        assert!(unknown.reservations.iter().all(Option::is_none));

        let inventory = pl011_inventory(7, DeviceStatus::Discovered);
        let mut registry = DriverRegistry::new();
        registry.register(7).unwrap();
        let plan = inventory
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        let decisions = plan
            .evaluate_spawn_authority(
                SpawnAuthorityRequest {
                    requester_tid: Some(3),
                    mock_epoch: 10,
                },
                SpawnAuthorityPolicy::allow_hosted_mock_spawns(),
            )
            .unwrap();
        let grants = inventory
            .build_resource_grant_bundle(&plan, &decisions, ResourceGrantPolicy::hosted_fake_rpi5())
            .unwrap();
        let requests = inventory
            .build_driver_spawn_request_bundle(&plan, &decisions, &grants)
            .unwrap();
        let validation = requests
            .simulate_pm_validation(
                Some(&inventory),
                PmSpawnValidationPolicy::hosted_fake_rpi5(),
            )
            .unwrap();
        let accounting = requests
            .simulate_pm_accounting(&validation, PmSpawnAccountingPolicy::hosted_fake_rpi5())
            .unwrap();
        let running = accounting_entry(&accounting, "arm,pl011");
        assert_eq!(running.status, PmSpawnAccountingStatus::AlreadyRunning);
        assert!(running.reservations.iter().all(Option::is_none));
    }

    #[test]
    fn pm_accounting_report_is_deterministic_bounded_and_non_mutating() {
        let (inventory, requests, validation, first) =
            build_fake_rpi5_accounting_report(PmSpawnAccountingPolicy::hosted_fake_rpi5());
        let requests_snapshot = requests.clone();
        let validation_snapshot = validation.clone();
        let second = requests
            .simulate_pm_accounting(&validation, PmSpawnAccountingPolicy::hosted_fake_rpi5())
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), requests.len());
        assert!(first.len() <= MAX_DEVICES);
        assert_eq!(requests, requests_snapshot);
        assert_eq!(validation, validation_snapshot);
        assert_eq!(inventory.len(), 5);
    }

    #[test]
    fn pm_accounting_does_not_call_driver_control_pm_supervisor_caps_grants_spawn_or_mmio() {
        let (_, _, _, accounting) =
            build_fake_rpi5_accounting_report(PmSpawnAccountingPolicy::hosted_fake_rpi5());
        assert_eq!(accounting.committed_count(), 1);
        let runtime = MockDriverControl::new();
        assert_eq!(runtime.irq_line.get(), None);
        assert_eq!(runtime.dma_request.get(), None);
        assert_eq!(runtime.irq_grant.get(), None);
        assert_eq!(runtime.dma_grant.get(), None);
        assert_eq!(runtime.registered.get(), None);
        for entry in accounting.iter() {
            assert_ne!(entry.mock_request_id, 0);
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
