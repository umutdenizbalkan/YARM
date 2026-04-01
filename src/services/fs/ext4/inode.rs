// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Inode {
    pub path_ptr: u64,
    pub file_len: u64,
}
