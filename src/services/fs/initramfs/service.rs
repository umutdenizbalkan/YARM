use crate::kernel::vfs::VfsError;
use crate::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, openat_message, read_message, write_message,
};
use crate::services::common::service::{FsService, run_typed_request_loop};
use crate::services::fs::initramfs::archive::{INITRAMFS_BOOT_MARKER_PATH_PTR, InitramfsBackend};

pub type InitramfsService = FsService<InitramfsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitramfsLoopSummary {
    pub fd: u64,
    pub write_allowed: bool,
    pub handled: usize,
}

pub fn run_request_loop(service: &mut InitramfsService) -> Result<InitramfsLoopSummary, VfsError> {
    let open_rep = run_typed_request_loop(
        service,
        [openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: INITRAMFS_BOOT_MARKER_PATH_PTR,
            flags: 0,
            mode: 0,
        })
        .map_err(|_| VfsError::Malformed)?],
    )?[0];

    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let _ = run_typed_request_loop(
        service,
        [read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0,
            len: 512,
        })
        .map_err(|_| VfsError::Malformed)?],
    )?;

    let write = write_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 1,
    })
    .map_err(|_| VfsError::Malformed)?;
    let write_allowed = service.handle(write).is_ok();

    Ok(InitramfsLoopSummary {
        fd,
        write_allowed,
        handled: service.handled_count(),
    })
}

pub fn run() {
    let mut svc = InitramfsService::with_backend(InitramfsBackend::new(8192));
    let summary = run_request_loop(&mut svc).expect("initramfs loop");

    crate::yarm_log!(
        "initramfs.srv request-loop ready: fd={}, write_allowed={}, handled={}",
        summary.fd,
        summary.write_allowed,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initramfs_is_read_only() {
        let mut svc = InitramfsService::with_backend(InitramfsBackend::new(4096));
        let summary = run_request_loop(&mut svc).expect("loop");
        assert!(!summary.write_allowed);
        assert_eq!(summary.handled, 2);
    }
}
