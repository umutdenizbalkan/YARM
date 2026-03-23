use crate::kernel::vfs::VfsError;
use crate::kernel::vfs::{InMemoryBackend, OpenAtRequest, openat_message};
use crate::services::common::service::{FsService, run_typed_request_loop};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsLoopSummary {
    pub fd: u64,
    pub handled: usize,
}

pub fn run_request_loop(
    vfs: &mut FsService<InMemoryBackend>,
    path_ptr: u64,
) -> Result<VfsLoopSummary, VfsError> {
    let reply = run_typed_request_loop(
        vfs,
        [openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr,
            flags: 0,
            mode: 0,
        })
        .map_err(|_| VfsError::Malformed)?],
    )?[0];
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&reply.as_slice()[..8]);
    Ok(VfsLoopSummary {
        fd: u64::from_le_bytes(bytes),
        handled: vfs.handled_count(),
    })
}

pub fn run() {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let summary = run_request_loop(&mut vfs, 0x1000).expect("vfs loop");

    crate::yarm_log!(
        "vfs request-loop ready: fd={}, handled={}",
        summary.fd,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfs_request_loop_entrypoint_opens_one_fd() {
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let summary = run_request_loop(&mut vfs, 0x1010).expect("loop");

        assert_eq!(summary.fd, 3);
        assert_eq!(summary.handled, 1);
    }
}
