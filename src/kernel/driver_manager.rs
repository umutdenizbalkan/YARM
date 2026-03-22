use super::bootstrap::{KernelError, KernelState};
use super::capabilities::CapId;
use super::driver_abi::{
    DRIVER_OP_GRANT_DMA, DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, DRIVER_OP_RESTARTED,
};
use super::ipc::Message;

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, KernelError> {
    let end = offset.checked_add(8).ok_or(KernelError::WrongObject)?;
    let bytes = payload.get(offset..end).ok_or(KernelError::WrongObject)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(arr))
}

fn ok_reply(
    opcode: u16,
    value: u64,
    transferred_cap: Option<CapId>,
) -> Result<Message, KernelError> {
    let payload = value.to_le_bytes();
    let (flags, cap) = if let Some(cap_id) = transferred_cap {
        (Message::FLAG_CAP_TRANSFER, Some(cap_id.0))
    } else {
        (0, None)
    };
    Message::with_header(0, opcode, flags, cap, &payload).map_err(|_| KernelError::WrongObject)
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
        kernel: &mut KernelState,
        request: Message,
    ) -> Result<Message, KernelError> {
        let reply = handle_request(kernel, request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle_batch(
        &mut self,
        kernel: &mut KernelState,
        requests: impl IntoIterator<Item = Message>,
    ) -> Result<usize, KernelError> {
        for request in requests {
            self.handle(kernel, request)?;
        }
        Ok(self.handled)
    }
}

pub fn handle_request(kernel: &mut KernelState, request: Message) -> Result<Message, KernelError> {
    let payload = request.as_slice();
    match request.opcode {
        DRIVER_OP_REGISTER => {
            let tid = read_u64(payload, 0)?;
            kernel.register_driver(tid)?;
            ok_reply(DRIVER_OP_REGISTER, tid, None)
        }
        DRIVER_OP_GRANT_IRQ => {
            let tid = read_u64(payload, 0)?;
            let line = read_u64(payload, 8)? as u16;
            let cap = kernel.mint_irq_cap(line)?;
            kernel.grant_driver_irq(tid, cap)?;
            ok_reply(DRIVER_OP_GRANT_IRQ, line as u64, Some(cap))
        }
        DRIVER_OP_GRANT_DMA => {
            let tid = read_u64(payload, 0)?;
            let mem_cap = CapId(read_u64(payload, 8)?);
            let offset = read_u64(payload, 16)? as usize;
            let len = read_u64(payload, 24)? as usize;
            let cap = kernel.mint_dma_region_cap(mem_cap, offset, len)?;
            kernel.grant_driver_dma(tid, cap)?;
            ok_reply(DRIVER_OP_GRANT_DMA, len as u64, Some(cap))
        }
        DRIVER_OP_RESTARTED => {
            let tid = read_u64(payload, 0)?;
            let token = read_u64(payload, 8)?;
            kernel.restart_task(tid, token)?;
            ok_reply(DRIVER_OP_RESTARTED, tid, None)
        }
        _ => Err(KernelError::WrongObject),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::driver_abi::{DRIVER_OP_GRANT_IRQ, pack_driver_pair};

    #[test]
    fn driver_manager_register_and_grant_irq_roundtrip() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(7).expect("task");

        let register_msg =
            Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &7u64.to_le_bytes()).expect("msg");
        let register_reply = handle_request(&mut state, register_msg).expect("handle");
        assert_eq!(register_reply.opcode, DRIVER_OP_REGISTER);

        let grant_msg =
            Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(7, 9))
                .expect("msg");
        let reply = handle_request(&mut state, grant_msg).expect("handle");
        assert_eq!(reply.opcode, DRIVER_OP_GRANT_IRQ);
        assert!(reply.transferred_cap().is_some());
    }

    #[test]
    fn driver_service_tracks_handled_requests() {
        let mut state = Bootstrap::init().expect("init");
        state.register_task(5).expect("task");

        let register = Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &5u64.to_le_bytes())
            .expect("register");
        let irq = Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(5, 2))
            .expect("irq");

        let mut service = DriverService::new();
        let handled = service
            .handle_batch(&mut state, [register, irq])
            .expect("batch");
        assert_eq!(handled, 2);
        assert_eq!(service.handled_count(), 2);
    }
}
