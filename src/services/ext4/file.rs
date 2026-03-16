use crate::kernel::vfs::VfsLiteError;

pub fn checked_append(current_len: u64, delta: u64, max_len: u64) -> Result<u64, VfsLiteError> {
    let Some(next) = current_len.checked_add(delta) else {
        return Err(VfsLiteError::Unsupported);
    };
    if next > max_len {
        return Err(VfsLiteError::Unsupported);
    }
    Ok(next)
}
