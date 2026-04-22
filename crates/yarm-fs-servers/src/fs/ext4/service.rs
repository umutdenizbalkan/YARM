// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::fs::Ext4Backend;
use yarm::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, StatxRequest, openat_message, statx_message, write_message,
};
use yarm::service_common::service::FsService;
use yarm_srv_common::vfs_reply::VfsReply;

pub type Ext4Service = FsService<Ext4Backend>;

pub fn run() {
    let mut svc = Ext4Service::with_backend(Ext4Backend::new());

    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x4040,
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
        len: 8192,
    })
    .expect("write");
    let _ = svc.handle(write).expect("write rep");

    let stat = statx_message(StatxRequest {
        dirfd: 0,
        path_ptr: 0x4040,
        flags: 0,
        mask_or_buf: 0,
    })
    .expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");

    let file_len = VfsReply::from_opcode_payload_checked(stat_rep.opcode, stat_rep.as_slice())
        .expect("decode stat")
        .as_u64();

    yarm::yarm_log!(
        "ext4.srv demo: fd={}, file_len={}, handled={}",
        fd,
        file_len,
        svc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::kernel::vfs::{VfsBackend, VfsError};

    #[test]
    fn ext4_service_supports_write_stat() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: 0x2020,
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
            path_ptr: 0x2020,
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
        let fd = backend.openat(0x3030).expect("open");
        assert_eq!(
            backend.write(fd, (16 * 1024 * 1024) + 1),
            Err(VfsError::Unsupported)
        );
    }
}
