use crate::kernel::vfs::VfsError;
use crate::kernel::vfs::{
    InMemoryBackend, OpenAtRequest, ReadWriteRequest, StatxRequest, close_message, dup_message,
    epoll_create1_message, epoll_ctl_message, epoll_pwait_message, fcntl_message, ioctl_message,
    openat_message, poll_message, read_message, sendfile_message, statx_message, write_message,
};
use crate::services::common::service::{FsService, run_typed_request_loop};
use crate::services::common::vfs_service::VfsReply;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsLoopSummary {
    pub fd: u64,
    pub dup_fd: u64,
    pub epoll_fd: u64,
    pub handled: usize,
}

fn decode_fd_reply(reply: crate::kernel::ipc::Message) -> Result<u64, VfsError> {
    match VfsReply::from_message(reply)? {
        VfsReply::OpenAtFd(fd) | VfsReply::DupFd(fd) | VfsReply::EpollFd(fd) => Ok(fd),
        _ => Err(VfsError::Malformed),
    }
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
    let fd = decode_fd_reply(reply)?;
    let dup_fd = decode_fd_reply(
        run_typed_request_loop(vfs, [dup_message(fd).map_err(|_| VfsError::Malformed)?])?[0],
    )?;
    let epoll_fd = decode_fd_reply(
        run_typed_request_loop(
            vfs,
            [epoll_create1_message(0).map_err(|_| VfsError::Malformed)?],
        )?[0],
    )?;
    let _ = run_typed_request_loop(
        vfs,
        [
            read_message(ReadWriteRequest {
                fd,
                buf_ptr: 0x2000,
                len: 64,
            })
            .map_err(|_| VfsError::Malformed)?,
            write_message(ReadWriteRequest {
                fd,
                buf_ptr: 0x3000,
                len: 32,
            })
            .map_err(|_| VfsError::Malformed)?,
            statx_message(StatxRequest {
                dirfd: 0,
                path_ptr,
                flags: 0,
                mask_or_buf: 0,
            })
            .map_err(|_| VfsError::Malformed)?,
            ioctl_message(fd, 0x1234, 0x55).map_err(|_| VfsError::Malformed)?,
            fcntl_message(fd, 3, 9).map_err(|_| VfsError::Malformed)?,
            poll_message(0x9000, 2, 10).map_err(|_| VfsError::Malformed)?,
            epoll_ctl_message(epoll_fd, 1, fd, 0xA000).map_err(|_| VfsError::Malformed)?,
            epoll_pwait_message(epoll_fd, 0xB000, 4, 10).map_err(|_| VfsError::Malformed)?,
            sendfile_message(fd, dup_fd, 0xC000, 99).map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd: dup_fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd })
                .map_err(|_| VfsError::Malformed)?,
            close_message(crate::kernel::vfs::CloseRequest { fd: epoll_fd })
                .map_err(|_| VfsError::Malformed)?,
        ],
    )?;
    Ok(VfsLoopSummary {
        fd,
        dup_fd,
        epoll_fd,
        handled: vfs.handled_count(),
    })
}

pub fn run() {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let summary = run_request_loop(&mut vfs, 0x1000).expect("vfs loop");

    crate::yarm_log!(
        "vfs request-loop ready: fd={}, dup_fd={}, epoll_fd={}, handled={}",
        summary.fd,
        summary.dup_fd,
        summary.epoll_fd,
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
        assert_eq!(summary.dup_fd, 4);
        assert_eq!(summary.epoll_fd, 5);
        assert_eq!(summary.handled, 15);
    }
}
