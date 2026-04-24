// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::fs::Ext4Backend;
use super::super::common::vfs_ipc::{
    ReadWriteRequest, openat_inline_message, statx_inline_message, write_message,
};
use super::super::common::service::FsService;
use yarm_srv_common::vfs_reply::VfsReply;
use super::fs::EXT4_DEMO_PATH;

pub type Ext4Service = FsService<Ext4Backend>;

pub fn run() {
    let mut svc = Ext4Service::with_backend(Ext4Backend::new());

    let open = openat_inline_message(0, EXT4_DEMO_PATH, 0, 0).expect("open");
    let open_rep = svc.handle(open).expect("open rep");

    let fd = VfsReply::from_opcode_payload_checked(open_rep.opcode, open_rep.as_slice())
        .expect("decode open")
        .as_u64();

    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 8192,
    })
    .expect("write");
    let _ = svc.handle(write).expect("write rep");

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
    use super::*;
    use super::super::{
        EXT4_OVERSIZE_PATH_PTR, EXT4_SERVICE_PATH, EXT4_SERVICE_PATH_PTR,
    };
    use super::super::super::common::vfs_ipc::{VfsBackend, VfsError};
    use super::super::super::common::vfs_ipc::{
        OpenAtRequest, StatxRequest, openat_message, statx_message,
    };

    #[test]
    fn ext4_service_supports_write_stat() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: EXT4_SERVICE_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .expect("open");
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
        let _ = svc.handle(write).expect("write rep");

        let stat = statx_message(StatxRequest {
            dirfd: 0,
            path_ptr: EXT4_SERVICE_PATH_PTR,
            flags: 0,
            mask_or_buf: 0,
        })
        .expect("stat");
        let stat_rep = svc.handle(stat).expect("stat rep");
        assert_eq!(
            VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
                .expect("decode stat")
                .as_u64(),
            4096
        );
    }

    #[test]
    fn ext4_backend_rejects_oversized_write() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat(EXT4_OVERSIZE_PATH_PTR).expect("open");
        assert_eq!(
            backend.write(fd, (16 * 1024 * 1024) + 1),
            Err(VfsError::Unsupported)
        );
    }

    #[test]
    fn ext4_byte_path_open_and_statx_work() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat_path(EXT4_SERVICE_PATH).expect("open path");
        let _ = backend.write(fd, 512).expect("write");
        assert_eq!(backend.statx_path(EXT4_SERVICE_PATH), Ok(512));
    }

    #[test]
    fn ext4_byte_path_rejects_unknown_path() {
        let mut backend = Ext4Backend::new();
        assert_eq!(backend.openat_path(b"/ext4/missing"), Err(VfsError::InvalidPath));
        assert_eq!(backend.statx_path(b"/ext4/missing"), Err(VfsError::InvalidPath));
    }

    #[test]
    fn ext4_legacy_pointer_adapter_still_works() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat(EXT4_OVERSIZE_PATH_PTR).expect("open ptr");
        let _ = backend.write(fd, 64).expect("write");
        assert_eq!(backend.statx(EXT4_OVERSIZE_PATH_PTR), Ok(64));
        assert_eq!(backend.statx(0xDEAD), Err(VfsError::BadFd));
        assert_eq!(backend.openat(0xDEAD).map(|_| ()), Ok(()));
    }
}
