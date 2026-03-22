use crate::kernel::vfs::{VfsBackend, VfsError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeRecord {
    pub path_ptr: u64,
    pub file_len: u64,
}

pub const MAX_SERVICE_FDS: usize = 16;
pub const MAX_SERVICE_INODES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdRecord {
    pub fd: u64,
    pub inode: u64,
}

pub fn find_inode_index(inodes: &[Option<InodeRecord>], path_ptr: u64) -> Option<usize> {
    inodes.iter().position(|slot| {
        slot.map(|inode| inode.path_ptr == path_ptr)
            .unwrap_or(false)
    })
}

pub trait ServiceFsBackend: VfsBackend {
    fn name(&self) -> &'static str;
    fn validate(&self) -> Result<(), VfsError>;
}
