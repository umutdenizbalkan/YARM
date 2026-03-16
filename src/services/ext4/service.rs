extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args};
use crate::services::common::service::FsService;
use crate::services::ext4::fs::Ext4Backend;

pub type Ext4Service = FsService<Ext4Backend>;

pub fn run() {
    let mut svc = Ext4Service::with_backend(Ext4Backend::new());

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
    let _ = svc.handle(write).expect("write rep");

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::vfs::{VfsBackend, VfsLiteError};

    #[test]
    fn ext4_service_supports_write_stat() {
        let mut svc = Ext4Service::with_backend(Ext4Backend::new());
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, 0x2020, 0, 0).encode(),
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
            &VfsV1Args::new(fd, 0, 4096, 0).encode(),
        )
        .expect("write");
        let _ = svc.handle(write).expect("write rep");

        let stat = Message::with_header(
            0,
            VFS_OP_STATX,
            0,
            None,
            &VfsV1Args::new(0, 0x2020, 0, 0).encode(),
        )
        .expect("stat");
        let stat_rep = svc.handle(stat).expect("stat rep");
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(stat_rep.as_slice());
        assert_eq!(u64::from_le_bytes(len_bytes), 4096);
    }

    #[test]
    fn ext4_backend_rejects_oversized_write() {
        let mut backend = Ext4Backend::new();
        let fd = backend.openat(0x3030).expect("open");
        assert_eq!(
            backend.write(fd, (16 * 1024 * 1024) + 1),
            Err(VfsLiteError::Unsupported)
        );
    }
}
