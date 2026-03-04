use super::ipc::Message;
use super::linux_compat::{VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE};

const MAX_FDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsLiteError {
    Malformed,
    NoFd,
    BadFd,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FdEntry {
    fd: u64,
    inode: u64,
}

#[derive(Debug)]
pub struct VfsLiteService {
    next_fd: u64,
    fds: [Option<FdEntry>; MAX_FDS],
}

impl Default for VfsLiteService {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsLiteService {
    pub const fn new() -> Self {
        Self {
            next_fd: 3,
            fds: [None; MAX_FDS],
        }
    }

    fn read_u64(payload: &[u8], idx: usize) -> Result<u64, VfsLiteError> {
        let start = idx.checked_mul(8).ok_or(VfsLiteError::Malformed)?;
        let end = start.checked_add(8).ok_or(VfsLiteError::Malformed)?;
        let bytes = payload.get(start..end).ok_or(VfsLiteError::Malformed)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(arr))
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, VfsLiteError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| VfsLiteError::Malformed)
    }

    fn alloc_fd(&mut self, inode: u64) -> Result<u64, VfsLiteError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.fds.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(FdEntry { fd, inode });
            Ok(fd)
        } else {
            Err(VfsLiteError::NoFd)
        }
    }

    fn has_fd(&self, fd: u64) -> bool {
        self.fds.iter().flatten().any(|entry| entry.fd == fd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsLiteError> {
        if let Some(slot) = self
            .fds
            .iter_mut()
            .find(|slot| slot.map(|entry| entry.fd == fd).unwrap_or(false))
        {
            *slot = None;
            Ok(())
        } else {
            Err(VfsLiteError::BadFd)
        }
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let payload = request.as_slice();
        match request.opcode {
            VFS_OP_OPENAT => {
                let path_ptr = Self::read_u64(payload, 1)?;
                let fd = self.alloc_fd(path_ptr)?;
                Self::u64_reply(VFS_OP_OPENAT, fd)
            }
            VFS_OP_CLOSE => {
                let fd = Self::read_u64(payload, 0)?;
                self.close_fd(fd)?;
                Self::u64_reply(VFS_OP_CLOSE, 0)
            }
            VFS_OP_READ => {
                let fd = Self::read_u64(payload, 0)?;
                let len = Self::read_u64(payload, 2)?;
                if !self.has_fd(fd) {
                    return Err(VfsLiteError::BadFd);
                }
                Self::u64_reply(VFS_OP_READ, len)
            }
            VFS_OP_WRITE => {
                let fd = Self::read_u64(payload, 0)?;
                let len = Self::read_u64(payload, 2)?;
                if !self.has_fd(fd) {
                    return Err(VfsLiteError::BadFd);
                }
                Self::u64_reply(VFS_OP_WRITE, len)
            }
            VFS_OP_STATX => {
                let path_ptr = Self::read_u64(payload, 1)?;
                Self::u64_reply(VFS_OP_STATX, path_ptr)
            }
            _ => Err(VfsLiteError::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::linux_compat::{VFS_OP_OPENAT, VFS_OP_READ};

    fn pack(a0: u64, a1: u64, a2: u64, a3: u64) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0..8].copy_from_slice(&a0.to_le_bytes());
        out[8..16].copy_from_slice(&a1.to_le_bytes());
        out[16..24].copy_from_slice(&a2.to_le_bytes());
        out[24..32].copy_from_slice(&a3.to_le_bytes());
        out
    }

    #[test]
    fn open_read_close_lifecycle_is_stable() {
        let mut svc = VfsLiteService::new();

        let open_req =
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &pack(0, 0x1000, 0, 0)).expect("open");
        let open_rep = svc.handle_request(open_req).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let read_req =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(fd, 0x2000, 64, 0)).expect("read");
        let read_rep = svc.handle_request(read_req).expect("read rep");
        assert_eq!(read_rep.opcode, VFS_OP_READ);

        let close_req =
            Message::with_header(0, VFS_OP_CLOSE, 0, None, &pack(fd, 0, 0, 0)).expect("close");
        let close_rep = svc.handle_request(close_req).expect("close rep");
        assert_eq!(close_rep.opcode, VFS_OP_CLOSE);
    }

    #[test]
    fn read_rejects_unknown_fd() {
        let mut svc = VfsLiteService::new();
        let read_req =
            Message::with_header(0, VFS_OP_READ, 0, None, &pack(99, 0, 1, 0)).expect("read");
        assert_eq!(svc.handle_request(read_req), Err(VfsLiteError::BadFd));
    }

    #[test]
    fn rejects_unsupported_opcode() {
        let mut svc = VfsLiteService::new();
        let req = Message::with_header(0, 0xFFFF, 0, None, &pack(0, 0, 0, 0)).expect("msg");
        assert_eq!(svc.handle_request(req), Err(VfsLiteError::Unsupported));
    }
}
