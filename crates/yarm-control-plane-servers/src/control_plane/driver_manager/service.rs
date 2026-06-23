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

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, KernelIpcError> {
    let end = offset.checked_add(8).ok_or(KernelIpcError::WrongObject)?;
    let bytes = payload
        .get(offset..end)
        .ok_or(KernelIpcError::WrongObject)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(arr))
}

fn ok_reply(
    opcode: u16,
    value: u64,
    transferred_cap: Option<CapId>,
) -> Result<Message, KernelIpcError> {
    let payload = value.to_le_bytes();
    let (flags, cap) = if let Some(cap_id) = transferred_cap {
        (Message::FLAG_CAP_TRANSFER, Some(cap_id.0))
    } else {
        (0, None)
    };
    Message::with_header(0, opcode, flags, cap, &payload).map_err(|_| KernelIpcError::WrongObject)
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

    /// Register a driver by tid.  If the tid is already registered, the
    /// existing record is returned without modification.  Returns `Err` when
    /// the table is full.
    pub fn register(&mut self, tid: u64) -> Result<(), KernelIpcError> {
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
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRange {
    pub base: u64,
    pub len: u64,
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
        };
        record.compatible[..compatible.len()].copy_from_slice(compatible.as_bytes());
        record.driver_candidate[..driver_candidate.len()]
            .copy_from_slice(driver_candidate.as_bytes());
        Ok(record)
    }

    pub fn compatible(&self) -> Option<&str> {
        core::str::from_utf8(&self.compatible[..self.compatible_len]).ok()
    }

    pub fn driver_candidate(&self) -> Option<&str> {
        core::str::from_utf8(&self.driver_candidate[..self.driver_candidate_len]).ok()
    }

    pub fn with_mmio(mut self, index: usize, base: u64, len: u64) -> Result<Self, KernelIpcError> {
        if index >= self.mmio_ranges.len() || len == 0 {
            return Err(KernelIpcError::WrongObject);
        }
        self.mmio_ranges[index] = Some(MmioRange { base, len });
        Ok(self)
    }

    pub fn with_irq(mut self, index: usize, line: u32) -> Result<Self, KernelIpcError> {
        if index >= self.irq_lines.len() {
            return Err(KernelIpcError::WrongObject);
        }
        self.irq_lines[index] = Some(line);
        Ok(self)
    }
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

    pub fn handle(
        &mut self,
        runtime: &mut impl DriverControlOps,
        request: Message,
    ) -> Result<Message, KernelIpcError> {
        let reply = handle_request(&mut self.registry, runtime, request)?;
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
    let payload = request.as_slice();
    match request.opcode {
        DRIVER_OP_REGISTER => {
            let tid = read_u64(payload, 0)?;
            // Record in local registry first; then inform kernel runtime.
            registry.register(tid)?;
            runtime.register_driver(tid)?;
            ok_reply(DRIVER_OP_REGISTER, tid, None)
        }
        DRIVER_OP_GRANT_IRQ => {
            let tid = read_u64(payload, 0)?;
            let line = read_u64(payload, 8)? as u16;
            let cap = runtime.mint_irq_cap(line)?;
            runtime.grant_driver_irq(tid, cap)?;
            ok_reply(DRIVER_OP_GRANT_IRQ, line as u64, Some(cap))
        }
        DRIVER_OP_GRANT_DMA => {
            let tid = read_u64(payload, 0)?;
            let mem_cap = CapId(read_u64(payload, 8)?);
            let offset = read_u64(payload, 16)? as usize;
            let len = read_u64(payload, 24)? as usize;
            let cap = runtime.mint_dma_region_cap(mem_cap, offset, len)?;
            runtime.grant_driver_dma(tid, cap)?;
            ok_reply(DRIVER_OP_GRANT_DMA, len as u64, Some(cap))
        }
        DRIVER_OP_RESTARTED => {
            let tid = read_u64(payload, 0)?;
            let token = read_u64(payload, 8)?;
            runtime.restart_task(tid, token)?;
            ok_reply(DRIVER_OP_RESTARTED, tid, None)
        }
        _ => Err(KernelIpcError::WrongObject),
    }
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
        Ok(())
    }
    fn mint_irq_cap(&mut self, _line: u16) -> Result<CapId, KernelIpcError> {
        Ok(CapId(0))
    }
    fn grant_driver_irq(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
        Ok(())
    }
    fn mint_dma_region_cap(
        &mut self,
        _mem_cap: CapId,
        _offset: usize,
        _len: usize,
    ) -> Result<CapId, KernelIpcError> {
        Ok(CapId(0))
    }
    fn grant_driver_dma(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
        Ok(())
    }
    fn restart_task(&mut self, _tid: u64, _token: u64) -> Result<(), KernelIpcError> {
        Ok(())
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
                    match service.handle(&mut runtime, msg) {
                        Ok(reply) => {
                            if let Some(cap) = reply_cap {
                                // SAFETY: kernel validates reply capability rights/object.
                                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(cap, &reply) };
                            }
                        }
                        Err(e) => {
                            yarm_user_rt::user_log!("DRIVER_MANAGER_HANDLE_ERR err={:?}", e);
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
    #[cfg(feature = "legacy-tests")]
    use yarm::kernel::boot::Bootstrap;
    #[cfg(feature = "legacy-tests")]
    use yarm::std::thread;
    #[cfg(feature = "legacy-tests")]
    use yarm_ipc_abi::driver_abi::{DRIVER_OP_GRANT_IRQ, pack_driver_pair};

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
        for i in 0..MAX_DRIVERS as u64 {
            registry.register(i).expect("fill");
        }
        assert_eq!(registry.len(), MAX_DRIVERS);
        let result = registry.register(MAX_DRIVERS as u64);
        assert!(result.is_err(), "should fail when table is full");
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
}
