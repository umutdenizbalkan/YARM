#![no_std]
extern crate std;

use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::linux_compat::{LINUX_NR_GETPID, LinuxServiceBindings, dispatch};
use yarm::kernel::proc_proto::PROC_OP_GETPID;
use yarm::kernel::trapframe::TrapFrame;

use std::println;

fn main() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut bindings = LinuxServiceBindings::default();

    let (_proc_req_ep, proc_req_send, proc_req_recv) = kernel.create_endpoint(8).expect("proc req");
    let (_proc_rep_ep, proc_rep_send, proc_rep_recv) = kernel.create_endpoint(8).expect("proc rep");
    bindings
        .register_process_manager(&kernel, proc_req_send, proc_rep_recv)
        .expect("bind proc");

    kernel
        .ipc_send(
            proc_rep_send,
            Message::with_header(0, PROC_OP_GETPID, 0, None, &42u64.to_le_bytes()).expect("reply"),
        )
        .expect("seed reply");

    let mut frame = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
    dispatch(&mut kernel, &bindings, &mut frame);

    let routed = kernel.ipc_recv(proc_req_recv).expect("recv").is_some();
    println!(
        "linux-compat server demo: translated getpid -> ret={}, routed_request={}",
        frame.ret0, routed
    );
}
