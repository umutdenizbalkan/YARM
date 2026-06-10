// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{openat_inline_message, statx_inline_message};
use super::fs::EXT4_DEMO_PATH;
use super::fs::Ext4Backend;
use yarm_srv_common::vfs_reply::VfsReply;

pub type Ext4Service = FsService<Ext4Backend>;

fn run_demo_smoke(svc: &mut Ext4Service) {
    let open = openat_inline_message(0, EXT4_DEMO_PATH, 0, 0).expect("open");
    let open_rep = svc.handle(open).expect("open rep");

    let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
        .expect("decode open")
        .as_u64();

    let stat = statx_inline_message(0, EXT4_DEMO_PATH, 0, 0).expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");

    let file_len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .expect("decode stat")
        .as_u64();

    yarm_user_rt::user_log!(
        "ext4.srv demo: fd={}, file_len={}, handled={}",
        fd,
        file_len,
        svc.handled_count()
    );
}

fn run_resident_service_loop(svc: &mut Ext4Service) {
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("EXT4_SRV_RECV_CAP cap={}", recv_cap);
        yarm_user_rt::user_log!("EXT4_SRV_BLOCKING_RECV_LOOP");
        loop {
            // SAFETY: ext4_srv owns its startup-provided service recv endpoint.
            match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
                Ok(Some(received)) => {
                    let msg = received.message;
                    let Some(reply_cap) = received.reply_cap else {
                        continue;
                    };
                    let response = svc.handle(msg).unwrap_or_else(|_| {
                        yarm_user_rt::ipc::Message::new(1, &[]).expect("err-reply")
                    });
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &response) };
                }
                _ => {
                    let _ = yarm_user_rt::syscall::yield_now();
                }
            }
        }
    } else {
        yarm_user_rt::user_log!("EXT4_SRV_NO_RECV_CAP_RESIDENT_YIELD");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    }
}

pub fn run() {
    let mut svc = Ext4Service::with_backend(Ext4Backend::new());
    run_demo_smoke(&mut svc);
    yarm_user_rt::user_log!("EXT4_SRV_READY");
    run_resident_service_loop(&mut svc);
}

#[cfg(test)]
mod tests {
    use super::super::super::common::vfs_ipc::{
        ReadWriteRequest, VfsBackend, VfsError, write_message,
    };
    use super::super::{EXT4_OVERSIZE_PATH, EXT4_SERVICE_PATH};
    use super::*;
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, StatxInlinePath};

    #[test]
    fn ext4_service_rejects_write_and_preserves_stat() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("open");
        let decoded_open = OpenAtInlinePath::decode(open.as_slice()).expect("decode open");
        assert_eq!(decoded_open.path, EXT4_SERVICE_PATH);
        let open_rep = svc.handle(open).expect("open rep");
        let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
            .expect("decode open")
            .as_u64();

        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 4096,
        })
        .expect("write");
        assert_eq!(svc.handle(write), Err(VfsError::Unsupported));

        let stat = statx_inline_message(0, EXT4_SERVICE_PATH, 0, 0).expect("stat");
        let decoded_stat = StatxInlinePath::decode(stat.as_slice()).expect("decode stat");
        assert_eq!(decoded_stat.path, EXT4_SERVICE_PATH);
        let stat_rep = svc.handle(stat).expect("stat rep");
        assert_eq!(
            VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
                .expect("decode stat")
                .as_u64(),
            0
        );
    }

    #[test]
    fn ext4_backend_rejects_all_writes() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_OVERSIZE_PATH).expect("open");
        assert_eq!(
            backend.write(fd, (16 * 1024 * 1024) + 1),
            Err(VfsError::Unsupported)
        );
    }

    #[test]
    fn ext4_byte_path_open_and_statx_work() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open path");
        assert_eq!(backend.write(fd, 512), Err(VfsError::Unsupported));
        assert_eq!(backend.statx_path(EXT4_SERVICE_PATH), Ok(0));
    }

    #[test]
    fn ext4_byte_path_rejects_unknown_path() {
        let mut backend = Ext4Backend::new();
        assert_eq!(
            backend.openat_path(b"/ext4/missing"),
            Err(VfsError::InvalidPath)
        );
        assert_eq!(
            backend.statx_path(b"/ext4/missing"),
            Err(VfsError::InvalidPath)
        );
    }
}
