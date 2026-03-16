extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args};
use crate::services::common::service::FsService;
use crate::services::fat::fs::FatBackend;

pub type FatService = FsService<FatBackend>;

pub fn run() {
    let mut svc = FatService::with_backend(FatBackend::new());
    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, 0x5050, 0, 0).encode(),
    )
    .expect("open");
    let rep = svc.handle(open).expect("open rep");
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let write = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(fd, 0, 33, 0).encode(),
    )
    .expect("write");
    let _ = svc.handle(write).expect("write rep");

    let stat = Message::with_header(
        0,
        VFS_OP_STATX,
        0,
        None,
        &VfsV1Args::new(0, 0x5050, 0, 0).encode(),
    )
    .expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(stat_rep.as_slice());
    let len = u64::from_le_bytes(len_bytes);
    println!(
        "fat.srv demo: fd={}, len={}, handled={}",
        fd,
        len,
        svc.handled_count()
    );
}
