// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NR 28 `CreateInitramfsFileSliceMo` syscall handler.
//!
//! Stage 102: mechanically split from the parent `syscall.rs` module. Stage 197A
//! removed the former NR 27 `InitramfsReadChunk` byte-copy bridge once the
//! MemoryObject zero-copy grant loader became the sole, mandatory ELF-load path;
//! this module now hosts only the NR 28 slice-MO handler. See
//! `doc/KERNEL_UNLOCKING.md` for the decomposition map.

use super::{SyscallError, current_tid};
use crate::kernel::boot::KernelState;
use crate::kernel::task::TaskClass;
use crate::kernel::trapframe::TrapFrame;
use yarm_srv_common::cpio::CpioArchive;

/// Phase 3A: Create a read-only MemoryObject backed by a named initramfs CPIO file slice.
///
/// Access control: caller must be `TaskClass::SystemServer` (initramfs_srv only).
///
/// ABI: arg0=name_ptr, arg1=name_len, arg2=flags (reserved, must be 0)
///
/// Returns: ret0=0, ret1=cap_id (u64), ret2=file_len (u64) on success.
pub(super) fn handle_create_initramfs_file_slice_mo(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    // Access gate: SystemServer only.
    let caller_tid = current_tid(kernel)?;
    let caller_class = kernel.task_class(caller_tid);
    if caller_class != Some(TaskClass::SystemServer) {
        crate::yarm_log!(
            "CREATE_INITRAMFS_FILE_SLICE_MO_DENIED tid={} reason=not_system_server",
            caller_tid
        );
        return Err(SyscallError::MissingRight);
    }

    let name_ptr = frame.arg(0);
    let name_len = frame.arg(1);
    let flags = frame.arg(2) as u64;

    if name_len == 0 || name_len > 128 {
        return Err(SyscallError::InvalidArgs);
    }
    if flags != 0 {
        return Err(SyscallError::InvalidArgs);
    }

    let name_buf = kernel
        .copy_from_current_user(name_ptr, name_len)
        .map_err(|_| SyscallError::InvalidArgs)?;
    let raw_name =
        core::str::from_utf8(&name_buf[..name_len]).map_err(|_| SyscallError::InvalidArgs)?;
    // Strip leading slash and optional "/initramfs/" prefix.
    let name = raw_name.trim_start_matches('/');
    let name = name.strip_prefix("initramfs/").unwrap_or(name);
    let name = name.trim_start_matches('/');

    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = CpioArchive::new(initrd)
        .find(name)
        .map_err(|_| SyscallError::InvalidArgs)?;
    let cpio_entry = match entry {
        Some(e) => e,
        None => {
            crate::yarm_log!("CREATE_INITRAMFS_FILE_SLICE_MO_NOT_FOUND name={}", name);
            return Err(SyscallError::InvalidArgs);
        }
    };
    let file_data = cpio_entry.file_data();
    let file_len = file_data.len();
    if file_len == 0 {
        crate::yarm_log!("CREATE_INITRAMFS_FILE_SLICE_MO_EMPTY name={}", name);
        return Err(SyscallError::InvalidArgs);
    }
    // Compute byte offset of file_data within the initrd blob.
    let initrd_ptr = initrd.as_ptr() as usize;
    let data_ptr = file_data.as_ptr() as usize;
    let file_data_offset = data_ptr
        .checked_sub(initrd_ptr)
        .ok_or(SyscallError::InvalidArgs)?;

    let (mo_id, cap_id) = kernel
        .create_initramfs_file_slice_mo(initrd, file_data_offset, file_len)
        .map_err(SyscallError::from)?;

    crate::yarm_log!(
        "CREATE_INITRAMFS_FILE_SLICE_MO_OK tid={} name={} file_len={} mo_id={} cap={}",
        caller_tid,
        name,
        file_len,
        mo_id,
        cap_id.0
    );

    // ret1 = cap_id (u32 packed into u64), ret2 = file_len
    frame.set_ok(0, cap_id.0 as usize, file_len);
    Ok(())
}
