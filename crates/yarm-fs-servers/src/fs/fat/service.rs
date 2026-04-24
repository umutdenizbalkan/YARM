// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::fs::FatBackend;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, statx_inline_message, write_message,
};
use super::super::common::service::FsService;
use yarm_srv_common::vfs_reply::VfsReply;
use super::fs::FAT_HELLO_PATH;

pub type FatService = FsService<FatBackend>;

pub fn run() {
    let mut svc = FatService::with_backend(FatBackend::new());
    let open = openat_inline_message(0, FAT_HELLO_PATH, 0, 0).expect("open");
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

    let stat = statx_inline_message(0, FAT_HELLO_PATH, 0, 0).expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");
    let len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .expect("decode stat")
        .as_u64();
    yarm_user_rt::user_log!(
        "fat.srv demo: fd={}, len={}, handled={}",
        fd,
        len,
        svc.handled_count()
    );
}
