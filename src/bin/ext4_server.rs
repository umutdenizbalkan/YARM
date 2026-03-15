#![no_std]
extern crate std;

use yarm::kernel::ipc::Message;
use yarm::kernel::vfs_lite::Ext4Service;
use yarm::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args};

use std::println;

fn main() {
    let mut svc = Ext4Service::new();

    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, 0x4040, 0, 0).encode(),
    )
    .expect("open");
    let open_rep = svc.handle(open).expect("open rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let write = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(fd, 0, 8192, 0).encode(),
    )
    .expect("write");
    svc.handle(write).expect("write rep");

    let stat = Message::with_header(
        0,
        VFS_OP_STATX,
        0,
        None,
        &VfsV1Args::new(0, 0x4040, 0, 0).encode(),
    )
    .expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");

    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(stat_rep.as_slice());
    let file_len = u64::from_le_bytes(len_bytes);

    println!(
        "ext4.srv demo: fd={}, file_len={}, handled={}",
        fd,
        file_len,
        svc.handled_count()
    );
}
