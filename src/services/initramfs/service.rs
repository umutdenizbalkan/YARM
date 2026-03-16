extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_WRITE, VfsV1Args};
use crate::services::common::service::FsService;
use crate::services::initramfs::archive::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};

pub type InitramfsService = FsService<InitramfsBackend>;

pub fn run() {
    let mut svc = InitramfsService::with_backend(InitramfsBackend::new(8192));

    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, INITRAMFS_BUSYBOX_PATH_PTR, 0, 0).encode(),
    )
    .expect("open");
    let open_rep = svc.handle(open).expect("open rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let read = Message::with_header(
        0,
        VFS_OP_READ,
        0,
        None,
        &VfsV1Args::new(fd, 0, 512, 0).encode(),
    )
    .expect("read");
    let _ = svc.handle(read).expect("read rep");

    let write = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(fd, 0, 1, 0).encode(),
    )
    .expect("write");
    let write_result = svc.handle(write);

    println!(
        "initramfs.srv demo: fd={}, write_allowed={}, handled={}",
        fd,
        write_result.is_ok(),
        svc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::vfs::VfsLiteError;

    #[test]
    fn initramfs_is_read_only() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, INITRAMFS_BUSYBOX_PATH_PTR, 0, 0).encode(),
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
            &VfsV1Args::new(fd, 0, 1, 0).encode(),
        )
        .expect("write");
        assert_eq!(svc.handle(write), Err(VfsLiteError::Unsupported));
    }
}
