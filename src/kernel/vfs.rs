//! Generic VFS primitives and shared abstractions.
//! Concrete filesystem services must live under `src/services/*`.

pub use super::vfs_lite::*;

use super::ipc::Message;
use super::vfs_proto::{
    VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX, VFS_OP_WRITE, VfsV1Args,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    CharDevice,
    BlockDevice,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VNodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub create: bool,
}

impl OpenFlags {
    pub const fn rdonly() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookupRequest {
    pub dir: VNodeId,
    pub path_ptr: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    pub fd: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub node: VNodeId,
    pub kind: FileType,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAtRequest {
    pub dirfd: u64,
    pub path_ptr: u64,
    pub flags: u64,
    pub mode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseRequest {
    pub fd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWriteRequest {
    pub fd: u64,
    pub buf_ptr: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatxRequest {
    pub dirfd: u64,
    pub path_ptr: u64,
    pub flags: u64,
    pub mask_or_buf: u64,
}

pub fn openat_message(req: OpenAtRequest) -> Result<Message, VfsLiteError> {
    Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(req.dirfd, req.path_ptr, req.flags, req.mode).encode(),
    )
    .map_err(|_| VfsLiteError::Malformed)
}

pub fn close_message(req: CloseRequest) -> Result<Message, VfsLiteError> {
    Message::with_header(
        0,
        VFS_OP_CLOSE,
        0,
        None,
        &VfsV1Args::new(req.fd, 0, 0, 0).encode(),
    )
    .map_err(|_| VfsLiteError::Malformed)
}

pub fn read_message(req: ReadWriteRequest) -> Result<Message, VfsLiteError> {
    Message::with_header(
        0,
        VFS_OP_READ,
        0,
        None,
        &VfsV1Args::new(req.fd, req.buf_ptr, req.len, 0).encode(),
    )
    .map_err(|_| VfsLiteError::Malformed)
}

pub fn write_message(req: ReadWriteRequest) -> Result<Message, VfsLiteError> {
    Message::with_header(
        0,
        VFS_OP_WRITE,
        0,
        None,
        &VfsV1Args::new(req.fd, req.buf_ptr, req.len, 0).encode(),
    )
    .map_err(|_| VfsLiteError::Malformed)
}

pub fn statx_message(req: StatxRequest) -> Result<Message, VfsLiteError> {
    Message::with_header(
        0,
        VFS_OP_STATX,
        0,
        None,
        &VfsV1Args::new(req.dirfd, req.path_ptr, req.flags, req.mask_or_buf).encode(),
    )
    .map_err(|_| VfsLiteError::Malformed)
}

pub trait FilesystemService {
    fn service_name(&self) -> &'static str;
    fn dispatch(&mut self, request: Message) -> Result<Message, VfsLiteError>;
}

pub fn dispatch_once<S: FilesystemService>(
    service: &mut S,
    request: Message,
) -> Result<Message, VfsLiteError> {
    service.dispatch(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;
    impl FilesystemService for Dummy {
        fn service_name(&self) -> &'static str {
            "dummy"
        }

        fn dispatch(&mut self, request: Message) -> Result<Message, VfsLiteError> {
            Ok(request)
        }
    }

    #[test]
    fn dispatch_helper_roundtrips_message() {
        let mut dummy = Dummy;
        let msg = Message::with_header(0, 7, 0, None, &[1]).expect("msg");
        let rep = dispatch_once(&mut dummy, msg).expect("dispatch");
        assert_eq!(rep.opcode, 7);
    }

    #[test]
    fn typed_openat_message_encodes_vfs_proto() {
        let req = OpenAtRequest {
            dirfd: 0,
            path_ptr: 0x1000,
            flags: 1,
            mode: 0,
        };
        let msg = openat_message(req).expect("open");
        assert_eq!(msg.opcode, VFS_OP_OPENAT);
    }
}
