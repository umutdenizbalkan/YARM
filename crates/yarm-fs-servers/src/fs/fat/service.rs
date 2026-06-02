// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, close_message, openat_inline_message, read_message, statx_inline_message,
};
use super::fs::{FAT_HELLO_PATH, FatBackend};
use yarm_srv_common::vfs_reply::VfsReply;

pub type FatService = FsService<FatBackend>;

pub fn run() {
    yarm_user_rt::user_log!("FAT_SRV_ENTRY");
    let mut svc = FatService::with_backend(FatBackend::new());
    let open = openat_inline_message(0, FAT_HELLO_PATH, 0, 0).expect("open");
    let rep = svc.handle(open).expect("open rep");
    let fd = VfsReply::from_opcode_payload_checked(rep.opcode, rep.as_slice())
        .expect("decode open")
        .as_u64();
    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 32,
    })
    .expect("read");
    let _ = svc.handle(read).expect("read rep");
    let stat = statx_inline_message(0, FAT_HELLO_PATH, 0, 0).expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");
    let len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .expect("decode stat")
        .as_u64();
    let _ = svc
        .handle(close_message(super::super::common::vfs_ipc::CloseRequest { fd }).expect("close"));
    yarm_user_rt::user_log!(
        "fat.srv readonly ready: fd={}, len={}, handled={}",
        fd,
        len,
        svc.handled_count()
    );
}
