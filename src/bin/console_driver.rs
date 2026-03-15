#![no_std]
extern crate std;

use yarm::kernel::ipc::Message;
use yarm::kernel::vfs_lite::{ConsoleBackend, DEV_CONSOLE_PATH_PTR, VfsLiteService};
use yarm::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_WRITE, VfsV1Args};

use std::println;

fn main() {
    let mut svc = VfsLiteService::with_backend(ConsoleBackend::default());
    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, DEV_CONSOLE_PATH_PTR, 0, 0).encode(),
    )
    .expect("open");
    let open_rep = svc.handle_request(open).expect("open rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let write = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(fd, 0, 5, 0).encode(),
    )
    .expect("write");
    let rep = svc.handle_request(write).expect("write rep");
    println!("console driver demo write opcode={}", rep.opcode);
}
