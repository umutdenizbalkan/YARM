use super::inode::Ext4Inode;

pub fn find_inode_index(inodes: &[Option<Ext4Inode>], path_ptr: u64) -> Option<usize> {
    inodes.iter().position(|slot| {
        slot.map(|inode| inode.path_ptr == path_ptr)
            .unwrap_or(false)
    })
}
