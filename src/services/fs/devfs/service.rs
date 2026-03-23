use crate::kernel::vfs::VfsError;
use crate::kernel::vfs::{OpenAtRequest, ReadWriteRequest, openat_message, write_message};
use crate::services::common::service::{FsService, run_typed_request_loop};
use crate::services::fs::devfs::nodes::{DEV_CONSOLE_PATH_PTR, DEV_NULL_PATH_PTR, DevFsBackend};

pub type DevFsService = FsService<DevFsBackend>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevFsLoopSummary {
    pub console_fd: u64,
    pub null_fd: u64,
    pub handled: usize,
}

pub fn run_request_loop(service: &mut DevFsService) -> Result<DevFsLoopSummary, VfsError> {
    let replies = run_typed_request_loop(
        service,
        [
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: DEV_CONSOLE_PATH_PTR,
                flags: 0,
                mode: 0,
            })
            .map_err(|_| VfsError::Malformed)?,
            openat_message(OpenAtRequest {
                dirfd: 0,
                path_ptr: DEV_NULL_PATH_PTR,
                flags: 0,
                mode: 0,
            })
            .map_err(|_| VfsError::Malformed)?,
        ],
    )?;
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(replies[0].as_slice());
    let console_fd = u64::from_le_bytes(fd_bytes);
    fd_bytes.copy_from_slice(replies[1].as_slice());
    let null_fd = u64::from_le_bytes(fd_bytes);

    let _ = run_typed_request_loop(
        service,
        [
            write_message(ReadWriteRequest {
                fd: console_fd,
                buf_ptr: 0,
                len: 12,
            })
            .map_err(|_| VfsError::Malformed)?,
            write_message(ReadWriteRequest {
                fd: null_fd,
                buf_ptr: 0,
                len: 12,
            })
            .map_err(|_| VfsError::Malformed)?,
        ],
    )?;

    Ok(DevFsLoopSummary {
        console_fd,
        null_fd,
        handled: service.handled_count(),
    })
}

pub fn run() {
    let mut svc = DevFsService::with_backend(DevFsBackend::default());
    let summary = run_request_loop(&mut svc).expect("devfs loop");

    crate::yarm_log!(
        "devfs.srv request-loop ready: console_fd={}, null_fd={}, handled={}",
        summary.console_fd,
        summary.null_fd,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devfs_service_supports_console_and_null() {
        let mut svc = DevFsService::with_backend(DevFsBackend::default());
        let summary = run_request_loop(&mut svc).expect("loop");
        assert_eq!(summary.console_fd, 3);
        assert_eq!(summary.null_fd, 4);
        assert_eq!(summary.handled, 4);
    }
}
