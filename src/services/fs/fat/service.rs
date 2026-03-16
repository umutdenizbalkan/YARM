extern crate std;

use std::println;

use crate::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, StatxRequest, openat_message, statx_message, write_message,
};
use crate::services::common::service::FsService;
use crate::services::fs::fat::fs::FatBackend;

pub type FatService = FsService<FatBackend>;

pub fn run() {
    let mut svc = FatService::with_backend(FatBackend::new());
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x5050,
        flags: 0,
        mode: 0,
    })
    .expect("open");
    let rep = svc.handle(open).expect("open rep");
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 33,
    })
    .expect("write");
    let _ = svc.handle(write).expect("write rep");

    let stat = statx_message(StatxRequest {
        dirfd: 0,
        path_ptr: 0x5050,
        flags: 0,
        mask_or_buf: 0,
    })
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
