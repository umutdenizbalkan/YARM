extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_WRITE, VfsV1Args};
use crate::services::common::service::FsService;
use crate::services::devfs::nodes::{DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH_PTR, DevFsBackend};

pub type DevFsService = FsService<DevFsBackend>;

pub fn run() {
    let mut svc = DevFsService::with_backend(DevFsBackend::default());

    let open_console = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, DEV_CONSOLE_PATH_PTR, 0, 0).encode(),
    )
    .expect("open console");
    let open_null = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, DEV_NULL_PATH_PTR, 0, 0).encode(),
    )
    .expect("open null");

    let console_rep = svc.handle(open_console).expect("console rep");
    let null_rep = svc.handle(open_null).expect("null rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(console_rep.as_slice());
    let console_fd = u64::from_le_bytes(fd_bytes);
    fd_bytes.copy_from_slice(null_rep.as_slice());
    let null_fd = u64::from_le_bytes(fd_bytes);

    let write_console = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(console_fd, 0, 12, 0).encode(),
    )
    .expect("write console");
    let write_null = Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(null_fd, 0, 12, 0).encode(),
    )
    .expect("write null");

    let _ = svc.handle(write_console).expect("write console rep");
    let _ = svc.handle(write_null).expect("write null rep");

    println!(
        "devfs.srv demo: console_fd={}, null_fd={}, handled={}",
        console_fd,
        null_fd,
        svc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devfs_service_supports_console_and_null() {
        let mut svc = DevFsService::with_backend(DevFsBackend::default());
        let open_console = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, DEV_CONSOLE_PATH_PTR, 0, 0).encode(),
        )
        .expect("open console");
        let open_null = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, DEV_NULL_PATH_PTR, 0, 0).encode(),
        )
        .expect("open null");

        let _ = svc.handle(open_console).expect("console rep");
        let _ = svc.handle(open_null).expect("null rep");
        assert_eq!(svc.handled_count(), 2);
    }
}
