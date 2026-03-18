use crate::kernel::vfs::{OpenAtRequest, ReadWriteRequest, openat_message, write_message};
use crate::services::common::service::FsService;
use crate::services::fs::devfs::nodes::{DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH_PTR, DevFsBackend};

pub type DevFsService = FsService<DevFsBackend>;

pub fn run() {
    let mut svc = DevFsService::with_backend(DevFsBackend::default());

    let open_console = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: DEV_CONSOLE_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .expect("open console");
    let open_null = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: DEV_NULL_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .expect("open null");

    let console_rep = svc.handle(open_console).expect("console rep");
    let null_rep = svc.handle(open_null).expect("null rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(console_rep.as_slice());
    let console_fd = u64::from_le_bytes(fd_bytes);
    fd_bytes.copy_from_slice(null_rep.as_slice());
    let null_fd = u64::from_le_bytes(fd_bytes);

    let write_console = write_message(ReadWriteRequest {
        fd: console_fd,
        buf_ptr: 0,
        len: 12,
    })
    .expect("write console");
    let write_null = write_message(ReadWriteRequest {
        fd: null_fd,
        buf_ptr: 0,
        len: 12,
    })
    .expect("write null");

    let _ = svc.handle(write_console).expect("write console rep");
    let _ = svc.handle(write_null).expect("write null rep");

    crate::yarm_log!(
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
        let open_console = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: DEV_CONSOLE_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .expect("open console");
        let open_null = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: DEV_NULL_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .expect("open null");

        let _ = svc.handle(open_console).expect("console rep");
        let _ = svc.handle(open_null).expect("null rep");
        assert_eq!(svc.handled_count(), 2);
    }
}
