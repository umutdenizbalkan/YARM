// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm::kernel::boot::{KernelError, KernelState};
use yarm_ipc_abi::driver_abi::{
    DRIVER_OP_GRANT_DMA, DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, DRIVER_OP_RESTARTED,
};
use yarm_user_rt::capability::CapId;
use yarm_user_rt::ipc::Message;
use yarm_user_rt::runtime::{DriverControlOps, KernelIpcError};

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, KernelIpcError> {
    let end = offset.checked_add(8).ok_or(KernelIpcError::WrongObject)?;
    let bytes = payload.get(offset..end).ok_or(KernelIpcError::WrongObject)?;
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

#[derive(Debug)]
pub struct KernelDriverControl<'a> {
    kernel: &'a mut KernelState,
}

impl<'a> KernelDriverControl<'a> {
    pub const fn new(kernel: &'a mut KernelState) -> Self {
        Self { kernel }
    }
}

impl DriverControlOps for KernelDriverControl<'_> {
    fn register_driver(&mut self, tid: u64) -> Result<(), KernelIpcError> {
        self.kernel.register_driver(tid).map_err(map_kernel_ipc_error)
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

#[derive(Debug, Default)]
pub struct DriverService {
    handled: usize,
}

impl DriverService {
    pub const fn new() -> Self {
        Self { handled: 0 }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(
        &mut self,
        runtime: &mut impl DriverControlOps,
        request: Message,
    ) -> Result<Message, KernelIpcError> {
        let reply = handle_request(runtime, request)?;
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
    runtime: &mut impl DriverControlOps,
    request: Message,
) -> Result<Message, KernelIpcError> {
    let payload = request.as_slice();
    match request.opcode {
        DRIVER_OP_REGISTER => {
            let tid = read_u64(payload, 0)?;
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
            let mut runtime = KernelDriverControl { kernel: &mut state };

            let register_msg =
                Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &7u64.to_le_bytes())
                    .expect("msg");
            let register_reply = handle_request(&mut runtime, register_msg).expect("handle");
            assert_eq!(register_reply.opcode, DRIVER_OP_REGISTER);

            let grant_msg =
                Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(7, 9))
                    .expect("msg");
            let reply = handle_request(&mut runtime, grant_msg).expect("handle");
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

            let register = Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &5u64.to_le_bytes())
                .expect("register");
            let irq = Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(5, 2))
                .expect("irq");

            let mut service = DriverService::new();
            let handled = service.handle_batch(&mut runtime, [register, irq]).expect("batch");
            assert_eq!(handled, 2);
            assert_eq!(service.handled_count(), 2);
        });
    }
}
