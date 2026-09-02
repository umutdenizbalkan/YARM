// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Process-domain syscall handlers and helpers.
//!
//! D4 step 2: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch arms
//! and source-grep guard rails remain stable.

use super::spawn_image_txn;
use super::{
    PM_BOOTSTRAP_TID, SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_LEN,
    SYSCALL_ARG_PTR, SyscallError, current_tid, validate_user_region,
};
use crate::kernel::boot::{KernelError, KernelState, MemoryObjectKind, UserImageSpec};
use crate::kernel::capabilities::{CapId, CapObject};
use crate::kernel::task::TaskClass;
use crate::kernel::trapframe::TrapFrame;
use yarm_srv_common::{cpio::CpioArchive, elf::ElfImageInfo};

pub(super) fn handle_spawn_thread(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let parent_tid = current_tid(kernel)?;
    let tls_base = frame.arg(SYSCALL_ARG_CAP);
    let user_stack_top = frame.arg(SYSCALL_ARG_PTR);
    let user_entry = frame.arg(SYSCALL_ARG_LEN);
    let tid = kernel
        .spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)
        .map_err(SyscallError::from)?;
    frame.set_ok(
        usize::try_from(tid).map_err(|_| SyscallError::Internal)?,
        0,
        0,
    );
    Ok(())
}

/// Stage 181C: map a `KernelError` returned by the fork/COW path to a stable,
/// greppable reason token from a small fixed vocabulary. The syscall ABI still
/// collapses most of these to `SyscallError::Internal` (err=255); this token
/// exposes the *actual* cause so the oracle/smoke can report WHY fork failed
/// (a capacity leak, a stale-state mismatch, or a genuine map/asid fault)
/// instead of the opaque `Internal`. It maps, it does NOT change behavior.
fn fork_cow_fail_reason(e: KernelError) -> &'static str {
    use crate::kernel::vm::VmError;
    match e {
        KernelError::VmFull => "asid_full",
        KernelError::Vm(VmError::Full) => "cow_capacity",
        KernelError::Vm(VmError::InvalidAsid) => "active_asid",
        KernelError::Vm(VmError::Misaligned)
        | KernelError::Vm(VmError::PrivilegeViolation)
        | KernelError::Vm(VmError::InvalidAddress) => "vm_fault",
        KernelError::SchedulerFull | KernelError::TaskTableFull => "task_full",
        KernelError::CapabilityFull => "cap_full",
        KernelError::EndpointFull | KernelError::EndpointQueueFull => "endpoint_full",
        KernelError::MemoryObjectFull => "mo_full",
        KernelError::MemoryObjectMissing => "mo_missing",
        KernelError::TaskMissing => "current_tid",
        KernelError::MissingRight => "rights",
        KernelError::InvalidCapability
        | KernelError::WrongObject
        | KernelError::StaleCapability => "cnode",
        KernelError::UserMemoryFault => "user_fault",
        KernelError::WouldBlock => "would_block",
    }
}

pub(super) fn handle_fork(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let parent_tid = current_tid(kernel)?;
    // Stage 163C: proof-gated fork diagnostics (only when the sender-wake sub-knob
    // is active, so normal boot logs are never polluted). Pinpoints whether the
    // failure is before/after child allocation and its exact reason.
    let proof = crate::kernel::boot::ipc_recv_proof_sender_wake_active();
    if proof {
        crate::yarm_log!("FORK_PROOF_ENTER parent_tid={}", parent_tid);
        // Stage 181C: stable named span markers around the whole fork/COW attempt.
        // The per-phase reasons come from the FORK_PROOF_* markers emitted inside
        // `fork_user_process_cow`; FORK_COW_FAIL below normalizes the terminal
        // KernelError into a fixed reason vocabulary the oracle greps for.
        // Stage 181C: PT-pool headroom at fork entry. The child cnode-slot Vec is drawn
        // from the slab heap backed by this pool, so a low value here predicts the
        // CapabilityFull/AllocFailed register failure.
        crate::yarm_log!(
            "FORK_COW_BEGIN parent_tid={} pt_pool_free_frames={}",
            parent_tid,
            crate::kernel::frame_allocator::pt_pool_free_frames()
        );
    }
    match kernel.fork_user_process_cow(parent_tid) {
        Ok(child_tid) => {
            if proof {
                crate::yarm_log!("FORK_PROOF_PARENT_RET child_tid={}", child_tid);
                crate::yarm_log!("FORK_COW_DONE child_tid={}", child_tid);
            }
            frame.set_ok(
                usize::try_from(child_tid).map_err(|_| SyscallError::Internal)?,
                0,
                0,
            );
            Ok(())
        }
        Err(e) => {
            let se = SyscallError::from(e);
            if proof {
                crate::yarm_log!("FORK_PROOF_RETURN_ERR code={} reason={:?}", se as usize, e);
                // Stage 181C: the actual cause behind an opaque `Internal` (err=255).
                crate::yarm_log!(
                    "FORK_COW_FAIL reason={} kernel_error={:?} syscall_code={}",
                    fork_cow_fail_reason(e),
                    e,
                    se as usize
                );
            }
            Err(se)
        }
    }
}

pub(super) fn handle_spawn_process(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let image_id = frame.arg(SYSCALL_ARG_CAP) as u64;
    let parent_pid = frame.arg(SYSCALL_ARG_PTR) as u64;
    let startup_args_ptr = frame.arg(SYSCALL_ARG_LEN);
    let startup_args_count = frame.arg(SYSCALL_ARG_INLINE_PAYLOAD0);
    crate::yarm_log!(
        "KSPAWN_ENTER image_id={} parent_pid={} args_count={}",
        image_id,
        parent_pid,
        startup_args_count
    );
    // Stage 175 (SPAWN-LIFECYCLE): default-off phase markers. U9-ASPACE1 §2 moved them here from
    // the retired NR 26 handler — they instrument a SPAWN, not a particular syscall number, and
    // NR 23 performs the same four steps (request, image resolve, ELF parse, load). Every
    // resolve/parse/load/spawn step below is unchanged; these only expose the phase boundaries.
    let spawn_lc = crate::kernel::boot::spawn_lifecycle_enabled();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_REQUEST_BEGIN image_id={} parent_pid={}",
            image_id,
            parent_pid
        );
    }
    let (startup_args, extra_send_caps) =
        normalized_startup_args(kernel, startup_args_ptr, startup_args_count)?;
    // For initramfs_srv (image_id=4) the transaction maps the boot initrd read-only into its
    // address space after the load, publishing the user VA + length in startup slots 15/16.
    const INITRAMFS_IMAGE_ID: u64 = 4;
    let Some(image_path) = spawn_image_path_for_image_id(image_id) else {
        if spawn_lc {
            crate::yarm_log!("SPAWN_LIFECYCLE_BAD_IMAGE_ID image_id={}", image_id);
        }
        return Err(SyscallError::InvalidArgs);
    };
    crate::yarm_log!("KSPAWN_PATH path={}", image_path);
    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = match CpioArchive::new(initrd).find(image_path) {
        Ok(Some(entry)) => entry,
        Ok(None) | Err(_) => {
            if spawn_lc {
                crate::yarm_log!("SPAWN_LIFECYCLE_IMAGE_RESOLVE_FAIL image_id={}", image_id);
            }
            return Err(SyscallError::InvalidArgs);
        }
    };
    let elf_bytes = entry.file_data();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_IMAGE_RESOLVE_OK image_id={} bytes={}",
            image_id,
            elf_bytes.len()
        );
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_PARSE_BEGIN image_id={}", image_id);
    }
    crate::yarm_log!("KSPAWN_ELF_FOUND size={}", elf_bytes.len());
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_ELF_PARSED entry={}", elf.entry);
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ELF_PARSE_OK image_id={} entry=0x{:x}",
            image_id,
            elf.entry
        );
    }
    let committed = spawn_image_txn::run_image_spawn_transaction(
        kernel,
        spawn_image_txn::SpawnImageRequest {
            image_id,
            image_path,
            source: spawn_image_txn::SpawnImageSource::PtLoadSegments {
                elf: elf_bytes,
                entry: elf.entry as usize,
            },
            class: TaskClass::SystemServer,
            parent_pid,
            startup_args,
            extra_send_caps,
            map_initrd_window: image_id == INITRAMFS_IMAGE_ID,
            lifecycle_markers: spawn_lc,
        },
    )?;
    frame.set_ok(0, committed.reply_tid, committed.packed_ret2 as usize);
    Ok(())
}

/// Kernel-side staging buffer for ELF images supplied via SpawnProcessFromUserBuf.
///
/// A proper per-call allocation would require a kernel heap; the static buffer
/// avoids that dependency at the cost of exclusivity.  Rather than rely on an
/// out-of-band "single caller" comment guarding a `static mut`, the buffer is
/// wrapped in [`TakeOnceStagingBuffer`], which encodes exclusive access in the
/// type system: the only way to obtain a mutable view is via `try_take`, which
/// uses an atomic claim flag.  The claim is released when the returned
/// [`StagingBufferClaim`] guard is dropped, so the buffer can be reused by the
/// next spawn syscall (PM issues one spawn at a time, and a syscall handler runs
/// to completion before the next is dispatched).  If a claim is somehow already
/// outstanding the handler returns a stable error instead of aliasing the buffer.
static VFS_ELF_STAGING: TakeOnceStagingBuffer<{ 128 * 1024 }> = TakeOnceStagingBuffer::new();

/// A statically-allocated byte buffer that hands out at most one outstanding
/// mutable claim at a time.
///
/// The single-use ("take-once") invariant is enforced by an [`AtomicBool`]:
/// `try_take` atomically flips `claimed` from `false` to `true`, returning a
/// guard on success and `None` if a claim is already outstanding.  Dropping the
/// guard resets the flag, allowing reuse on the next call.  This replaces a raw
/// `static mut` and the `static_mut_refs` lint exposure with a type whose only
/// safe access path is exclusive by construction.
pub(super) struct TakeOnceStagingBuffer<const N: usize> {
    claimed: core::sync::atomic::AtomicBool,
    data: core::cell::UnsafeCell<[u8; N]>,
}

// SAFETY: the only access to `data` is through `try_take`, which uses the atomic
// `claimed` flag to guarantee that at most one `StagingBufferClaim` exists at a
// time.  No two threads can obtain overlapping mutable references to `data`.
unsafe impl<const N: usize> Sync for TakeOnceStagingBuffer<N> {}

impl<const N: usize> TakeOnceStagingBuffer<N> {
    pub(super) const fn new() -> Self {
        Self {
            claimed: core::sync::atomic::AtomicBool::new(false),
            data: core::cell::UnsafeCell::new([0u8; N]),
        }
    }

    /// Atomically claim exclusive access to the buffer.  Returns `None` if a
    /// claim is already outstanding.
    pub(super) fn try_take(&'static self) -> Option<StagingBufferClaim<'static, N>> {
        use core::sync::atomic::Ordering;
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| StagingBufferClaim { buf: self })
    }
}

/// RAII guard proving exclusive access to a [`TakeOnceStagingBuffer`].  Not
/// `Clone`/`Copy`: only one can exist at a time.  Releases the claim on drop.
pub(super) struct StagingBufferClaim<'a, const N: usize> {
    buf: &'a TakeOnceStagingBuffer<N>,
}

impl<'a, const N: usize> StagingBufferClaim<'a, N> {
    pub(super) fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: holding this guard means `claimed == true` and (because
        // `try_take` is the only producer and the flag is reset only on drop)
        // no other `StagingBufferClaim` for the same buffer exists, so this is
        // the unique mutable reference to `data`.
        unsafe { &mut *self.buf.data.get() }
    }
}

impl<'a, const N: usize> Drop for StagingBufferClaim<'a, N> {
    fn drop(&mut self) {
        self.buf
            .claimed
            .store(false, core::sync::atomic::Ordering::Release);
    }
}

pub(super) fn handle_spawn_process_from_user_buf(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let image_id = frame.arg(0) as u64;
    let elf_user_ptr = frame.arg(1);
    let elf_len = frame.arg(2);
    let parent_pid = frame.arg(3) as u64;
    let startup_args_ptr = frame.arg(4);
    let startup_args_count = frame.arg(5);
    crate::yarm_log!(
        "KSPAWN_ENTER image_id={} parent_pid={} args_count={}",
        image_id,
        parent_pid,
        startup_args_count
    );
    if elf_len == 0 || elf_len > 128 * 1024 || elf_user_ptr == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    validate_user_region(elf_user_ptr as u64, elf_len as u64)?;
    // Exclusive, type-checked access to the shared ELF staging buffer; the claim
    // is released when `staging_claim` drops at end of handler.
    let mut staging_claim = VFS_ELF_STAGING.try_take().ok_or(SyscallError::Internal)?;
    let staging = staging_claim.as_mut_slice();
    kernel
        .copy_from_current_user_into_slice(elf_user_ptr, elf_len, staging)
        .map_err(SyscallError::from)?;
    let elf_bytes = &staging[..elf_len];
    let image_path = spawn_image_path_for_image_id(image_id).ok_or(SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_PATH path={}", image_path);
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_ELF_PARSED entry={}", elf.entry);
    let (startup_args, extra_send_caps) =
        normalized_startup_args(kernel, startup_args_ptr, startup_args_count)?;
    let committed = spawn_image_txn::run_image_spawn_transaction(
        kernel,
        spawn_image_txn::SpawnImageRequest {
            image_id,
            image_path,
            source: spawn_image_txn::SpawnImageSource::PtLoadSegments {
                elf: elf_bytes,
                entry: elf.entry as usize,
            },
            class: TaskClass::SystemServer,
            parent_pid,
            startup_args,
            extra_send_caps,
            map_initrd_window: false,
            lifecycle_markers: false,
        },
    )?;
    frame.set_ok(0, committed.reply_tid, committed.packed_ret2 as usize);
    Ok(())
}

fn spawn_image_path_for_image_id(image_id: u64) -> Option<&'static str> {
    match image_id {
        0 => Some("init"),
        1 => Some("sbin/supervisor"),
        2 => Some("sbin/process_manager"),
        3 => Some("sbin/init_server"),
        4 => Some("sbin/initramfs_srv"),
        5 => Some("sbin/devfs_srv"),
        6 => Some("sbin/vfs_server"),
        7 => Some("sbin/driver_manager"),
        8 => Some("sbin/blkcache_srv"),
        9 => Some("sbin/virtio_blk_srv"),
        // Stage 81B: optional FS servers staged in CPIO by Stage 80.
        // Kernel path table entries required for Phase 3A/Phase 2B spawn
        // to succeed when INIT_SPAWN_OPTIONAL_FS_SERVERS is enabled.
        10 => Some("sbin/fat_srv"),
        11 => Some("sbin/ramfs_srv"),
        12 => Some("sbin/ext4_srv"),
        _ => None,
    }
}

/// U9-SPAWN1 SP-3 — the startup-argument normalization every image-loading spawn performs.
///
/// All four handlers copied the caller's array and then applied the SAME edits, transcribed four
/// times: slot 2 (the service reply recv capability) and slot 12 (the service recv capability)
/// are kernel-filled, so a caller may not preselect them; slots 13..16 are lifted out as the
/// extra send capabilities to delegate and then cleared, because the values the caller put there
/// name capabilities in ITS cspace, not the child's.
///
/// Returned as a pair so a caller cannot use the array before the extras have been lifted out.
fn normalized_startup_args(
    kernel: &KernelState,
    startup_args_ptr: usize,
    startup_args_count: usize,
) -> Result<([u64; UserImageSpec::DEFAULT_STARTUP_ARGS.len()], [u64; 4]), SyscallError> {
    let mut startup_args = copy_spawn_startup_args(kernel, startup_args_ptr, startup_args_count)?;
    startup_args[2] = 0;
    let extra_send_caps = [
        startup_args[13],
        startup_args[14],
        startup_args[15],
        startup_args[16],
    ];
    startup_args[12] = 0;
    startup_args[13] = 0;
    startup_args[14] = 0;
    startup_args[15] = 0;
    startup_args[16] = 0;
    Ok((startup_args, extra_send_caps))
}

fn copy_spawn_startup_args(
    kernel: &KernelState,
    startup_args_ptr: usize,
    startup_args_count: usize,
) -> Result<[u64; UserImageSpec::DEFAULT_STARTUP_ARGS.len()], SyscallError> {
    let mut out = UserImageSpec::DEFAULT_STARTUP_ARGS;
    if startup_args_count == 0 {
        return Ok(out);
    }
    if startup_args_count > out.len() || startup_args_ptr == 0 {
        return Err(SyscallError::InvalidArgs);
    }
    let byte_len = startup_args_count
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(SyscallError::InvalidArgs)?;
    validate_user_region(startup_args_ptr as u64, byte_len as u64)?;
    // copy_from_current_user is limited to Message::MAX_PAYLOAD (128 bytes) per call.
    // Read in chunks so that larger startup_args arrays (e.g. 18 * 8 = 144 bytes) work.
    let mut slot_idx = 0usize;
    let mut bytes_remaining = byte_len;
    let mut ptr = startup_args_ptr;
    while bytes_remaining > 0 {
        let chunk_bytes = bytes_remaining.min(crate::kernel::ipc::Message::MAX_PAYLOAD);
        let payload = kernel
            .copy_from_current_user(ptr, chunk_bytes)
            .map_err(SyscallError::from)?;
        for chunk in payload[..chunk_bytes].chunks_exact(core::mem::size_of::<u64>()) {
            if slot_idx >= out.len() {
                break;
            }
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            out[slot_idx] = u64::from_le_bytes(word);
            slot_idx += 1;
        }
        ptr = ptr
            .checked_add(chunk_bytes)
            .ok_or(SyscallError::InvalidArgs)?;
        bytes_remaining -= chunk_bytes;
    }
    Ok(out)
}

/// SUP-L7K-A: PM-only terminal-task reap used after PM has successfully created
/// a restart replacement. This is deliberately not a generic kill syscall:
/// only PM (TID 3) may call it, the caller may not target itself, and only
/// terminal Faulted/Exited/Dead tasks are accepted. Cleanup is delegated to the
/// dedicated `reap_faulted_task_noalloc_cleanup` path (SUP-L7K-C): it does NOT
/// call the broad `mark_task_dead` helper and performs no allocation, marking
/// the TCB Dead in place and revoking reply-caps / IPC waiters / kernel context /
/// process CNode via the no-allocation reap variants.
pub(super) fn handle_reap_faulted_task(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let caller = current_tid(kernel)?;
    let target = frame.arg(0) as u64;
    crate::yarm_log!(
        "TASK_REAP_FAULTED_BEGIN caller_tid={} target_tid={}",
        caller,
        target
    );

    if caller != PM_BOOTSTRAP_TID {
        crate::yarm_log!(
            "TASK_REAP_FAULTED_REJECT target_tid={} reason=not_pm",
            target
        );
        return Err(SyscallError::MissingRight);
    }
    if target == caller {
        crate::yarm_log!("TASK_REAP_FAULTED_REJECT target_tid={} reason=self", target);
        return Err(SyscallError::InvalidArgs);
    }

    let Some(status) = kernel.task_status(target) else {
        crate::yarm_log!("TASK_REAP_FAULTED_ALREADY_GONE target_tid={}", target);
        frame.set_ok(0, 0, 0);
        return Ok(());
    };

    match status {
        crate::kernel::task::TaskStatus::Faulted
        | crate::kernel::task::TaskStatus::Exited(_)
        | crate::kernel::task::TaskStatus::Dead => {}
        crate::kernel::task::TaskStatus::Runnable
        | crate::kernel::task::TaskStatus::Running
        | crate::kernel::task::TaskStatus::Blocked(_)
        // Stage 199D-WA3B: a spawn reservation has never run, so it is not a faulted task to
        // reap. Reaping one would destroy provisioning an in-flight bootstrap still needs.
        | crate::kernel::task::TaskStatus::Reserved => {
            crate::yarm_log!(
                "TASK_REAP_FAULTED_REJECT target_tid={} reason=non_terminal",
                target
            );
            return Err(SyscallError::WrongObject);
        }
    }

    kernel
        .reap_faulted_task_noalloc_cleanup(target)
        .map_err(SyscallError::from)?;
    crate::yarm_log!("TASK_REAP_FAULTED_OK target_tid={}", target);
    frame.set_ok(0, 0, 0);
    Ok(())
}

/// Phase 3A: Spawn a process from an InitramfsFileSlice MemoryObject capability.
///
/// Access control: caller must be PM (TID == PM_BOOTSTRAP_TID).
///
/// ABI: arg0=image_id, arg1=mo_cap (CapId), arg2=parent_pid,
///      arg3=startup_args_ptr, arg4=startup_args_count
///
/// Resolves the MemoryObject → reads initrd slice → loads ELF via load_elf_with_mo_zero_copy
/// → spawns through the same compensated transaction as every other image-loading spawn.
///
/// Returns: ret0=0, ret1=spawned_tid, ret2=packed_send_caps on success.
pub(super) fn handle_spawn_from_memory_object(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    // Access gate: PM only.
    let caller_tid = current_tid(kernel)?;
    if caller_tid != PM_BOOTSTRAP_TID {
        crate::yarm_log!("SPAWN_FROM_MO_DENIED tid={} reason=not_pm", caller_tid);
        return Err(SyscallError::MissingRight);
    }

    let image_id = frame.arg(0) as u64;
    let mo_cap_raw = frame.arg(1) as u64;
    let parent_pid = frame.arg(2) as u64;
    let startup_args_ptr = frame.arg(3);
    let startup_args_count = frame.arg(4);

    crate::yarm_log!(
        "SPAWN_FROM_MO_ENTER image_id={} mo_cap={} parent_pid={}",
        image_id,
        mo_cap_raw,
        parent_pid
    );

    let mo_cap = CapId(mo_cap_raw);

    // Resolve capability → must be a MemoryObject.
    let capability = kernel
        .resolve_capability_for_task(caller_tid, mo_cap)
        .map_err(SyscallError::from)?;
    let mo_id = match capability.object {
        CapObject::MemoryObject { id } => id,
        _ => {
            crate::yarm_log!(
                "SPAWN_FROM_MO_WRONG_CAP image_id={} mo_cap={}",
                image_id,
                mo_cap_raw
            );
            return Err(SyscallError::WrongObject);
        }
    };

    // Look up MemoryObject slot to get the InitramfsFileSlice kind.
    let (file_data_offset, file_len) = kernel
        .with_memory_state(|memory| {
            memory
                .memory_objects
                .iter()
                .flatten()
                .find(|mo| mo.id == mo_id)
                .and_then(|mo| match mo.kind {
                    MemoryObjectKind::InitramfsFileSlice {
                        initrd_offset,
                        file_len,
                    } => Some((initrd_offset as usize, file_len as usize)),
                    _ => None,
                })
                .ok_or(KernelError::WrongObject)
        })
        .map_err(SyscallError::from)?;

    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;

    if file_data_offset
        .checked_add(file_len)
        .ok_or(SyscallError::InvalidArgs)?
        > initrd.len()
    {
        crate::yarm_log!(
            "SPAWN_FROM_MO_BOUNDS_ERR image_id={} off={} len={} initrd_len={}",
            image_id,
            file_data_offset,
            file_len,
            initrd.len()
        );
        return Err(SyscallError::InvalidArgs);
    }

    let elf_bytes = &initrd[file_data_offset..file_data_offset + file_len];
    crate::yarm_log!(
        "SPAWN_FROM_MO_ELF image_id={} elf_len={}",
        image_id,
        elf_bytes.len()
    );

    // Parse ELF for entry point.
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("SPAWN_FROM_MO_ENTRY entry=0x{:x}", elf.entry);

    let image_path = spawn_image_path_for_image_id(image_id).ok_or(SyscallError::InvalidArgs)?;

    let (startup_args, extra_send_caps) =
        normalized_startup_args(kernel, startup_args_ptr, startup_args_count)?;

    // Physical base of the initrd blob, for the zero-copy loader's feasibility check.
    let initrd_virt_raw = initrd.as_ptr() as u64;
    let initrd_phys_base = {
        let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
        let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
        if virt_base > phys_base && initrd_virt_raw >= virt_base {
            initrd_virt_raw - virt_base + phys_base
        } else {
            initrd_virt_raw
        }
    };

    let committed = spawn_image_txn::run_image_spawn_transaction(
        kernel,
        spawn_image_txn::SpawnImageRequest {
            image_id,
            image_path,
            source: spawn_image_txn::SpawnImageSource::ZeroCopyInitramfsSlice {
                elf: elf_bytes,
                initrd_phys_base,
                file_initrd_offset: file_data_offset as u64,
            },
            class: TaskClass::SystemServer,
            parent_pid,
            startup_args,
            extra_send_caps,
            map_initrd_window: false,
            lifecycle_markers: false,
        },
    )?;
    crate::yarm_log!(
        "SPAWN_FROM_MO_OK image_id={} spawned_tid={}",
        image_id,
        committed.tid
    );
    frame.set_ok(0, committed.reply_tid, committed.packed_ret2 as usize);
    Ok(())
}
