// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::service::FsService;
use super::super::common::vfs_ipc::{openat_inline_message, statx_inline_message};
use super::fs::Ext4Backend;
use super::fs::EXT4_DEMO_PATH;
use yarm_srv_common::vfs_reply::VfsReply;

pub type Ext4Service = FsService<Ext4Backend>;

pub fn run() {
    let mut svc = Ext4Service::with_backend(Ext4Backend::new());

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

#[cfg(test)]
mod tests {
    use super::super::super::common::vfs_ipc::{
        write_message, ReadWriteRequest, VfsBackend, VfsError,
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
