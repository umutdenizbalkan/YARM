#![no_std]
use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::driver_manager::DriverService;
use yarm::kernel::driver_abi::{DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, pack_driver_pair};
use yarm::kernel::ipc::Message;


fn main() {
    let mut kernel = Bootstrap::init().expect("init");
    kernel.register_task(2).expect("task");

    let register = Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &2u64.to_le_bytes())
        .expect("register msg");
    let grant = Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(2, 9))
        .expect("grant msg");

    let mut service = DriverService::new();
    let handled = service
        .handle_batch(&mut kernel, [register, grant])
        .expect("batch");

    yarm::yarm_log!("driver-manager demo ready: handled={}", handled);
}
