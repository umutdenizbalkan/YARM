#![no_std]
extern crate std;

use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::linux_compat::{LINUX_NR_OPENAT, dispatch};
use yarm::kernel::trapframe::TrapFrame;
use yarm::kernel::vfs_lite::{InMemoryBackend, VfsLiteService};

use std::println;

fn main() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut vfs = VfsLiteService::with_backend(InMemoryBackend::new());

    let (_req_ep, req_send, _req_recv) = kernel.create_endpoint(8).expect("req ep");
    let (_rep_ep, rep_send, rep_recv) = kernel.create_endpoint(8).expect("rep ep");
    kernel
        .register_linux_vfs_manager(req_send, rep_recv)
        .expect("register vfs manager");

    let mut payload = [0u8; 32];
    payload[8..16].copy_from_slice(&(0x1000u64).to_le_bytes());
    let synthetic_open = Message::with_header(
        0,
        yarm::kernel::linux_compat::VFS_OP_OPENAT,
        0,
        None,
        &payload,
    )
    .expect("request");
    let reply = vfs.handle_request(synthetic_open).expect("vfs reply");
    kernel.ipc_send(rep_send, reply).expect("seed reply");

    let mut frame = TrapFrame::new(LINUX_NR_OPENAT, [0, 0x1000, 0, 0, 0, 0]);
    dispatch(&mut kernel, &mut frame);
    println!("vfs-lite demo: fd={}", frame.ret0);
}
