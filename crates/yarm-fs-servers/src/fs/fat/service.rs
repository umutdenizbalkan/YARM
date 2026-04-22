// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::fs::FatBackend;
use yarm::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, StatxRequest, openat_message, statx_message, write_message,
};
use yarm::service_common::service::FsService;
use yarm_srv_common::vfs_reply::VfsReply;

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
    let fd = VfsReply::from_opcode_payload_checked(rep.opcode, rep.as_slice())
        .expect("decode open")
        .as_u64();

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
    let len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .expect("decode stat")
        .as_u64();
    yarm::yarm_log!(
        "fat.srv demo: fd={}, len={}, handled={}",
        fd,
        len,
        svc.handled_count()
    );
}
