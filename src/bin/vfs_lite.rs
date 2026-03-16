#![no_std]
extern crate std;

use yarm::kernel::ipc::Message;
use yarm::kernel::vfs::{InMemoryBackend, VfsLiteService};
use yarm::kernel::vfs_proto::{VFS_OP_OPENAT, VfsV1Args};

use std::println;

fn main() {
    let mut vfs = VfsLiteService::with_backend(InMemoryBackend::new());

    let synthetic_open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, 0x1000, 0, 0).encode(),
    )
    .expect("request");
    let reply = vfs.handle_request(synthetic_open).expect("vfs reply");
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&reply.as_slice()[..8]);
    let fd = u64::from_le_bytes(bytes);

    println!("vfs-lite server demo: fd={}", fd);
}
