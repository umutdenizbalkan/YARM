// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::inode::Ext4Inode;

pub fn find_inode_index(inodes: &[Option<Ext4Inode>], path_id: u64) -> Option<usize> {
    inodes.iter().position(|slot| {
        slot.map(|inode| inode.path_ptr == path_id)
            .unwrap_or(false)
    })
}
