use crate::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, openat_message, read_message, write_message,
};
use crate::services::common::service::FsService;
use crate::services::fs::initramfs::archive::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};

pub type InitramfsService = FsService<InitramfsBackend>;

pub fn run() {
    let mut svc = InitramfsService::with_backend(InitramfsBackend::new(8192));

    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .expect("open");
    let open_rep = svc.handle(open).expect("open rep");

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 512,
    })
    .expect("read");
    let _ = svc.handle(read).expect("read rep");

    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 1,
    })
    .expect("write");
    let write_result = svc.handle(write);

    crate::yarm_log!(
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
        let open = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .expect("open");
        let open_rep = svc.handle(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let write = write_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 1,
        })
        .expect("write");
        assert_eq!(svc.handle(write), Err(VfsLiteError::Unsupported));
    }
}
