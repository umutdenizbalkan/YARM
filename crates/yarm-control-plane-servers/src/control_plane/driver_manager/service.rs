// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(test)]
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
// KernelDriverControl (test-only runtime adapter)
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[cfg(test)]
pub struct KernelDriverControl<'a> {
    kernel: &'a mut KernelState,
}

#[cfg(test)]
impl<'a> KernelDriverControl<'a> {
    pub const fn new(kernel: &'a mut KernelState) -> Self {
        Self { kernel }
    }
}

#[cfg(test)]
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

#[cfg(test)]
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
    handled: usize,
}

impl DriverService {
    pub const fn new() -> Self {
        Self {
            registry: DriverRegistry::new(),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn registry(&self) -> &DriverRegistry {
        &self.registry
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
    use yarm::kernel::boot::Bootstrap;
    use yarm::std::thread;
    use yarm_ipc_abi::driver_abi::{DRIVER_OP_GRANT_IRQ, pack_driver_pair};

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
}
