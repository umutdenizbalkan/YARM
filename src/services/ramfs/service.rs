extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args};
use crate::services::common::service::FsService;
use crate::services::ramfs::tree::RamFsBackend;

pub type RamFsService = FsService<RamFsBackend>;

pub fn run() {
    let mut svc = RamFsService::with_backend(RamFsBackend::new());

    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, 0xA000, 0, 0).encode(),
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
        &VfsV1Args::new(fd, 0, 64, 0).encode(),
    )
    .expect("write");
    let _ = svc.handle(write).expect("write rep");

    let stat = Message::with_header(
        0,
        VFS_OP_STATX,
        0,
        None,
        &VfsV1Args::new(0, 0xA000, 0, 0).encode(),
    )
    .expect("stat");
    let stat_rep = svc.handle(stat).expect("stat rep");

    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(stat_rep.as_slice());
    let file_len = u64::from_le_bytes(len_bytes);

    println!(
        "ramfs.srv demo: fd={}, file_len={}, handled={}",
        fd,
        file_len,
        svc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::vfs_lite::{VfsBackend, VfsLiteError};

    #[test]
    fn ramfs_service_supports_write_then_stat() {
        let mut svc = RamFsService::with_backend(RamFsBackend::new());
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, 0x1010, 0, 0).encode(),
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
            &VfsV1Args::new(fd, 0, 128, 0).encode(),
        )
        .expect("write");
        let _ = svc.handle(write).expect("write rep");

        let stat = Message::with_header(
            0,
            VFS_OP_STATX,
            0,
            None,
            &VfsV1Args::new(0, 0x1010, 0, 0).encode(),
        )
        .expect("stat");
        let stat_rep = svc.handle(stat).expect("stat rep");
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(stat_rep.as_slice());
        assert_eq!(u64::from_le_bytes(len_bytes), 128);
        assert_eq!(svc.handled_count(), 3);
    }

    #[test]
    fn ramfs_unknown_fd_read_fails() {
        let mut backend = RamFsBackend::new();
        assert_eq!(backend.read(42, 1), Err(VfsLiteError::BadFd));
    }
}
