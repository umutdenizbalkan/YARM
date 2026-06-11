// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! NR 27 `InitramfsReadChunk` and NR 28 `CreateInitramfsFileSliceMo` syscall
//! handlers.
//!
//! Stage 102: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. The dispatch arms in `syscall.rs` are unchanged; this
//! module only hosts the moved bodies. Syscall 27 is deprecated after Phase 3B
//! but must not be removed (Phase 2B fallback — see `doc/AI_AGENT_RULES.md
//! §3.4`). See `doc/KERNEL_UNLOCKING_STAGE101_AUDIT.md §3` for the
//! decomposition map.

use super::{
    INITRAMFS_READ_CHUNK_TRACE, PM_BOOTSTRAP_TID, SyscallError, current_tid, validate_user_region,
};
use crate::kernel::boot::KernelState;
use crate::kernel::task::TaskClass;
use crate::kernel::trapframe::TrapFrame;
use yarm_srv_common::cpio::CpioArchive;

/// Phase 2 bulk-copy bridge: reads up to 4096 bytes from a named initramfs CPIO
/// file at the given byte offset into the caller's (or a target task's) user buffer.
///
/// TEMPORARY stepping stone for Phase 2. Replace with page-cap zero-copy in Phase 3.
///
/// Access control: caller must be `TaskClass::SystemServer`.
/// Any other task class receives `SyscallError::MissingRight`.
///
/// syscall nr=27 args:
///   arg0 = name_ptr    (user VA of file name bytes, no leading slash required)
///   arg1 = name_len    (1..=128)
///   arg2 = offset      (byte offset into file)
///   arg3 = dst_ptr     (user VA of destination buffer)
///   arg4 = max_len     (max bytes to copy; clamped to 4096)
///   arg5 = target_tid  (0 = write to caller's own ASID;
///                       PM_BOOTSTRAP_TID(3) = write to PM's ASID — Phase 2B bridge)
///
/// Returns: ret0=0 (status OK), ret1=bytes_copied
///          ret0=SyscallError (non-zero) on all error cases, including not_found.
///
/// Note: EOF (offset >= file_len) returns ret0=0, ret1=0 — NOT an error.
///       File-not-found returns ret0=SyscallError::Internal — NOT 0/EOF.
pub(super) fn handle_initramfs_read_chunk(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    // ── 0. Access gate: SystemServer only ────────────────────────────────────
    let caller_tid = current_tid(kernel)?;
    let caller_class = kernel.task_class(caller_tid);
    if caller_class != Some(TaskClass::SystemServer) {
        crate::yarm_log!("INITRAMFS_READ_CHUNK_DENIED tid={}", caller_tid);
        return Err(SyscallError::MissingRight);
    }

    let name_ptr = frame.arg(0);
    let name_len = frame.arg(1);
    let offset = frame.arg(2) as u64;
    let dst_ptr = frame.arg(3);
    let max_len = core::cmp::min(frame.arg(4), 4096);
    // Phase 2B extension: arg5 = target_tid (0 = self, PM_BOOTSTRAP_TID = PM's ASID)
    let target_tid_arg = frame.arg(5) as u64;

    if name_len == 0 || name_len > 128 {
        return Err(SyscallError::InvalidArgs);
    }
    if dst_ptr == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    // Validate target_tid: only 0 (self) or PM_BOOTSTRAP_TID allowed as Phase 2B bridge.
    if target_tid_arg != 0 && target_tid_arg != PM_BOOTSTRAP_TID {
        crate::yarm_log!(
            "INITRAMFS_READ_CHUNK_DENIED tid={} target_tid={} reason=invalid_target",
            caller_tid,
            target_tid_arg
        );
        return Err(SyscallError::MissingRight);
    }

    let name_buf = kernel
        .copy_from_current_user(name_ptr, name_len)
        .map_err(|_| SyscallError::InvalidArgs)?;
    let raw_name =
        core::str::from_utf8(&name_buf[..name_len]).map_err(|_| SyscallError::InvalidArgs)?;
    // Accept both "sbin/driver_manager" and "/sbin/driver_manager" and
    // "/initramfs/sbin/driver_manager" — strip leading slashes and optional
    // "/initramfs/" prefix so callers can reuse VFS path strings.
    let name = raw_name.trim_start_matches('/');
    let name = name.strip_prefix("initramfs/").unwrap_or(name);
    let name = name.trim_start_matches('/');

    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = CpioArchive::new(initrd)
        .find(name)
        .map_err(|_| SyscallError::InvalidArgs)?;
    let data = match entry {
        Some(e) => e.file_data(),
        None => {
            // File not found — return a real error (NOT 0/EOF).
            // EOF is reserved for "file exists but offset >= file_len".
            crate::yarm_log!(
                "INITRAMFS_READ_CHUNK_NOT_FOUND name={} offset={} max_len={}",
                name,
                offset,
                max_len
            );
            return Err(SyscallError::Internal);
        }
    };

    let offset_usize = offset as usize;
    if offset_usize >= data.len() {
        // EOF — file exists, offset is past the end.
        if INITRAMFS_READ_CHUNK_TRACE {
            crate::yarm_log!(
                "INITRAMFS_READ_CHUNK_EOF name={} offset={} file_len={}",
                name,
                offset,
                data.len()
            );
        }
        frame.set_ok(0, 0, 0);
        return Ok(());
    }

    let available = data.len() - offset_usize;
    let to_copy = core::cmp::min(available, max_len);

    if INITRAMFS_READ_CHUNK_TRACE {
        crate::yarm_log!(
            "INITRAMFS_READ_CHUNK name={} offset={} to_copy={} file_len={} target_tid={}",
            name,
            offset,
            to_copy,
            data.len(),
            target_tid_arg
        );
    }

    // ── Copy to destination ASID ──────────────────────────────────────────────
    if target_tid_arg == 0 {
        // Default: copy to caller's own address space.
        validate_user_region(dst_ptr as u64, to_copy as u64)?;
        kernel
            .copy_to_current_user_from_slice(dst_ptr, &data[offset_usize..offset_usize + to_copy])
            .map_err(SyscallError::from)?;
    } else {
        // Phase 2B bridge: copy to PM's address space (target_tid = PM_BOOTSTRAP_TID).
        // SAFETY: dst_ptr is PM's VA; to_copy ≤ 4096; data slice is valid CPIO data.
        kernel
            .copy_slice_to_task(
                target_tid_arg,
                dst_ptr,
                &data[offset_usize..offset_usize + to_copy],
            )
            .map_err(|_| SyscallError::PageFault)?;
    }

    if INITRAMFS_READ_CHUNK_TRACE {
        crate::yarm_log!(
            "INITRAMFS_READ_CHUNK_FAIL name={} stage=copy_done to_copy={}",
            name,
            to_copy
        );
    }

    frame.set_ok(0, to_copy, 0);
    Ok(())
}

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
