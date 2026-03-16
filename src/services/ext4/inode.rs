#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Inode {
    pub path_ptr: u64,
    pub file_len: u64,
}
