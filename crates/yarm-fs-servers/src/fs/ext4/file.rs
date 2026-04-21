// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm::kernel::vfs::VfsError;

pub fn checked_append(current_len: u64, delta: u64, max_len: u64) -> Result<u64, VfsError> {
    let Some(next) = current_len.checked_add(delta) else {
        return Err(VfsError::Unsupported);
    };
    if next > max_len {
        return Err(VfsError::Unsupported);
    }
    Ok(next)
}
