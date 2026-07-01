// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Process-domain syscall handlers and helpers.
//!
//! D4 step 2: mechanically split from the parent `syscall.rs` module with zero
//! behavior change. `syscall.rs` keeps minimal delegation shims so dispatch arms
//! and source-grep guard rails remain stable.

use super::{
    PM_BOOTSTRAP_TID, SYSCALL_ARG_CAP, SYSCALL_ARG_INLINE_PAYLOAD0, SYSCALL_ARG_LEN,
    SYSCALL_ARG_PTR, SyscallError, current_tid, validate_user_region,
};
use crate::kernel::boot::{KernelError, KernelState, MemoryObjectKind, UserImageSpec};
use crate::kernel::capabilities::{CapId, CapObject, CapRights};
use crate::kernel::task::TaskClass;
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::{CachePolicy, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
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
    }
    match kernel.fork_user_process_cow(parent_tid) {
        Ok(child_tid) => {
            if proof {
                crate::yarm_log!("FORK_PROOF_PARENT_RET child_tid={}", child_tid);
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
            }
            Err(se)
        }
    }
}

pub(super) fn handle_spawn_process(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    fn normalize_initrd_phys_ptr(raw_ptr: u64) -> Result<u64, SyscallError> {
        let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
        let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
        if virt_base > phys_base && raw_ptr >= virt_base {
            let off = raw_ptr
                .checked_sub(virt_base)
                .ok_or(SyscallError::Internal)?;
            let phys = phys_base.checked_add(off).ok_or(SyscallError::Internal)?;
            return Ok(phys);
        }
        if raw_ptr < virt_base || virt_base == phys_base {
            return Ok(raw_ptr);
        }
        crate::yarm_log!(
            "INITRAMFS_INITRD_ADDR_INVALID raw_ptr=0x{:x} virt_base=0x{:x} phys_base=0x{:x}",
            raw_ptr,
            virt_base,
            phys_base
        );
        Err(SyscallError::InvalidArgs)
    }

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
    // For initramfs_srv (image_id=4), we will map the boot initrd read-only
    // into its address space and pass the user VA + length via startup slots 15/16.
    // The mapping happens after the ASID is created below.
    const INITRAMFS_IMAGE_ID: u64 = 4;
    const INITRD_USER_VA_BASE: u64 = 0x0C00_0000;
    let image_path = spawn_image_path_for_image_id(image_id).ok_or(SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_PATH path={}", image_path);
    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = CpioArchive::new(initrd)
        .find(image_path)
        .map_err(|_| SyscallError::InvalidArgs)?
        .ok_or(SyscallError::InvalidArgs)?;
    let elf_bytes = entry.file_data();
    crate::yarm_log!("KSPAWN_ELF_FOUND size={}", elf_bytes.len());
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_ELF_PARSED entry={}", elf.entry);
    let tid = kernel.allocate_thread_id().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=allocate_tid err={:?}", err);
        SyscallError::from(err)
    })?;
    let (asid, _aspace_cap) = kernel.create_user_address_space().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=create_asid err={:?}", err);
        SyscallError::from(err)
    })?;
    crate::yarm_log!("KSPAWN_ASID_OK tid={} asid={}", tid, asid.0);
    kernel
        .load_elf_pt_load_segments(asid, elf_bytes)
        .map_err(|err| {
            crate::yarm_log!("KSPAWN_FAIL phase=load_elf err={:?}", err);
            SyscallError::from(err)
        })?;
    crate::yarm_log!("KSPAWN_LOAD_OK tid={}", tid);

    // Map boot initrd pages read-only into initramfs_srv (image_id=4).
    // This provides the CPIO data in userspace without syscall bridge.
    if image_id == INITRAMFS_IMAGE_ID {
        if let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() {
            let initrd_virt_raw = initrd.as_ptr() as u64;
            let initrd_phys_raw = normalize_initrd_phys_ptr(initrd_virt_raw)?;
            let initrd_len = initrd.len() as u64;
            let mut first6 = [0u8; 6];
            let first6_len = core::cmp::min(initrd.len(), first6.len());
            first6[..first6_len].copy_from_slice(&initrd[..first6_len]);
            crate::yarm_log!(
                "INITRAMFS_INITRD_SOURCE_RANGE raw_ptr=0x{:x} phys_start=0x{:x} len={}",
                initrd_virt_raw,
                initrd_phys_raw,
                initrd_len
            );
            crate::yarm_log!("INITRAMFS_INITRD_FIRST6 bytes={:?}", first6);
            let page: u64 = PAGE_SIZE as u64;
            let phys_start = initrd_phys_raw & !(page - 1);
            let phys_end = (initrd_phys_raw + initrd_len + page - 1) & !(page - 1);
            let pages_to_map = ((phys_end - phys_start) / page) as usize;
            let initrd_offset_in_first_page = (initrd_phys_raw - phys_start) as u64;
            crate::yarm_log!(
                "INITRAMFS_INITRD_MAP_BEGIN phys_start=0x{:x} phys_end=0x{:x} len={} pages={}",
                phys_start,
                phys_end,
                initrd_len,
                pages_to_map
            );
            let initrd_flags = PageFlags {
                read: true,
                write: false,
                execute: false,
                user: true,
                cache_policy: CachePolicy::WriteBack,
            };
            let mut map_ok = true;
            for i in 0..pages_to_map {
                let virt = VirtAddr(INITRD_USER_VA_BASE + (i as u64) * page);
                let phys = PhysAddr(phys_start + (i as u64) * page);
                if let Err(e) = kernel.map_user_page_in_asid_raw(
                    asid,
                    virt,
                    Mapping {
                        phys,
                        flags: initrd_flags,
                    },
                ) {
                    crate::yarm_log!(
                        "INITRAMFS_INITRD_MAP_FAIL page={} virt=0x{:x} err={:?}",
                        i,
                        virt.0,
                        e
                    );
                    map_ok = false;
                    break;
                }
            }
            if map_ok {
                let user_initrd_ptr = INITRD_USER_VA_BASE + initrd_offset_in_first_page;
                startup_args[15] = user_initrd_ptr;
                startup_args[16] = initrd_len;
                crate::yarm_log!(
                    "INITRAMFS_INITRD_MAP_DONE user_ptr=0x{:x} len={} rights=ro",
                    user_initrd_ptr,
                    initrd_len
                );
            }
        } else {
            crate::yarm_log!("INITRAMFS_INITRD_MAP_SKIP reason=no_boot_initrd");
        }
    }

    let spawner_tid = current_tid(kernel).unwrap_or(0);
    let (service_send_cap, service_recv_cap) = match kernel.create_endpoint(8) {
        Ok((_, send_cap, recv_cap)) => {
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                send_cap.0,
                recv_cap.0
            );
            (send_cap.0, recv_cap.0)
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
            (0u64, 0u64)
        }
    };
    let service_reply_recv_cap = match kernel.create_endpoint(8) {
        Ok((eid, _, recv_cap)) => {
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                eid,
                recv_cap.0
            );
            recv_cap.0
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err={:?}", e);
            0u64
        }
    };
    // If the caller supplied a parent_pid, grant a SEND copy of the new endpoint
    // into the parent's cnode and return that local cap so the parent can use it
    // directly without going through the spawner.
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.grant_capability_task_to_task_with_rights(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(cap) => {
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATED parent_tid={} cap={}",
                    parent_pid,
                    cap.0
                );
                cap.0
            }
            Err(e) => {
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATE_FAIL parent_tid={} err={:?}",
                    parent_pid,
                    e
                );
                service_send_cap
            }
        }
    } else {
        service_send_cap
    };

    crate::yarm_log!(
        "KSPAWN_BEFORE_SPAWN_TASK tid={} asid={} entry=0x{:x} parent_pid={} args_count={}",
        tid,
        asid.0,
        elf.entry,
        parent_pid,
        startup_args_count
    );
    let spawned = kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid,
            entry: elf.entry as usize,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args,
            spawner_tid,
            service_recv_cap,
            service_reply_recv_cap,
            extra_send_caps,
        })
        .map_err(|err| {
            crate::yarm_log!(
                "KSPAWN_SPAWN_TASK_FAIL tid={} asid={} err={:?}",
                tid,
                asid.0,
                err
            );
            SyscallError::from(err)
        })?;
    crate::yarm_log!("KSPAWN_TASK_READY tid={}", spawned.tid);
    // When parent delegation occurred, pack both the spawner's own send cap (high
    // 32 bits) and the parent-delegated cap (low 32 bits) into ret2 so the
    // spawner can use its own copy while forwarding the parent's copy.
    let packed_ret2 =
        if parent_pid != 0 && service_send_cap != 0 && caller_send_cap != service_send_cap {
            ((service_send_cap as u64) << 32) | (caller_send_cap as u64)
        } else {
            caller_send_cap as u64
        };
    frame.set_ok(
        0,
        usize::try_from(spawned.tid).map_err(|_| SyscallError::Internal)?,
        packed_ret2 as usize,
    );
    Ok(())
}

/// Kernel-side staging buffer for ELF images supplied via SpawnProcessFromUserBuf
/// and SpawnFromInitramfsFile.
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
    let tid = kernel.allocate_thread_id().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=allocate_tid err={:?}", err);
        SyscallError::from(err)
    })?;
    let (asid, _aspace_cap) = kernel.create_user_address_space().map_err(|err| {
        crate::yarm_log!("KSPAWN_FAIL phase=create_asid err={:?}", err);
        SyscallError::from(err)
    })?;
    crate::yarm_log!("KSPAWN_ASID_OK tid={} asid={}", tid, asid.0);
    kernel
        .load_elf_pt_load_segments(asid, elf_bytes)
        .map_err(|err| {
            crate::yarm_log!("KSPAWN_FAIL phase=load_elf err={:?}", err);
            SyscallError::from(err)
        })?;
    crate::yarm_log!("KSPAWN_LOAD_OK tid={}", tid);
    let spawner_tid = current_tid(kernel).unwrap_or(0);
    let (service_send_cap, service_recv_cap) = match kernel.create_endpoint(8) {
        Ok((_, send_cap, recv_cap)) => {
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                send_cap.0,
                recv_cap.0
            );
            (send_cap.0, recv_cap.0)
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
            (0u64, 0u64)
        }
    };
    let service_reply_recv_cap = match kernel.create_endpoint(8) {
        Ok((eid, _, recv_cap)) => {
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                eid,
                recv_cap.0
            );
            recv_cap.0
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err={:?}", e);
            0u64
        }
    };
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.grant_capability_task_to_task_with_rights(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(cap) => {
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATED parent_tid={} cap={}",
                    parent_pid,
                    cap.0
                );
                cap.0
            }
            Err(e) => {
                crate::yarm_log!(
                    "KSPAWN_PARENT_SEND_DELEGATE_FAIL parent_tid={} err={:?}",
                    parent_pid,
                    e
                );
                service_send_cap
            }
        }
    } else {
        service_send_cap
    };
    crate::yarm_log!(
        "KSPAWN_BEFORE_SPAWN_TASK tid={} asid={} entry=0x{:x} parent_pid={} args_count={}",
        tid,
        asid.0,
        elf.entry,
        parent_pid,
        startup_args_count
    );
    let spawned = kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid,
            entry: elf.entry as usize,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args,
            spawner_tid,
            service_recv_cap,
            service_reply_recv_cap,
            extra_send_caps,
        })
        .map_err(|err| {
            crate::yarm_log!(
                "KSPAWN_SPAWN_TASK_FAIL tid={} asid={} err={:?}",
                tid,
                asid.0,
                err
            );
            SyscallError::from(err)
        })?;
    crate::yarm_log!("KSPAWN_TASK_READY tid={}", spawned.tid);
    let packed_ret2 =
        if parent_pid != 0 && service_send_cap != 0 && caller_send_cap != service_send_cap {
            ((service_send_cap as u64) << 32) | (caller_send_cap as u64)
        } else {
            caller_send_cap as u64
        };
    frame.set_ok(
        0,
        usize::try_from(spawned.tid).map_err(|_| SyscallError::Internal)?,
        packed_ret2 as usize,
    );
    Ok(())
}

/// Spawn a process directly from a named file in the boot initramfs CPIO.
///
/// ABI: arg0=image_id, arg1=name_ptr, arg2=name_len, arg3=parent_pid,
///      arg4=startup_args_ptr, arg5=startup_args_count
///
/// Reads the ELF into the kernel-side staging buffer (no user-space buffer),
/// then spawns exactly like `SpawnProcessFromUserBuf`.
pub(super) fn handle_spawn_from_initramfs_file(
    kernel: &mut KernelState,
    frame: &mut TrapFrame,
) -> Result<(), SyscallError> {
    let image_id = frame.arg(0) as u64;
    let name_ptr = frame.arg(1);
    let name_len = frame.arg(2);
    let parent_pid = frame.arg(3) as u64;
    let startup_args_ptr = frame.arg(4);
    let startup_args_count = frame.arg(5);

    // Stage 175 (SPAWN-LIFECYCLE): default-off phase markers. Every resolve/parse/
    // load/spawn step is UNCHANGED — these only expose the phase boundaries.
    let spawn_lc = crate::kernel::boot::spawn_lifecycle_enabled();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_REQUEST_BEGIN image_id={} parent_pid={}",
            image_id,
            parent_pid
        );
    }

    if name_len == 0 || name_len > 128 {
        return Err(SyscallError::InvalidArgs);
    }

    let name_buf = kernel
        .copy_from_current_user(name_ptr, name_len)
        .map_err(|_| SyscallError::InvalidArgs)?;
    let name =
        core::str::from_utf8(&name_buf[..name_len]).map_err(|_| SyscallError::InvalidArgs)?;
    let name = name.strip_prefix('/').unwrap_or(name);

    let initrd =
        crate::kernel::boot::Bootstrap::boot_initrd_bytes().ok_or(SyscallError::InvalidArgs)?;
    let entry = match CpioArchive::new(initrd).find(name) {
        Ok(Some(entry)) => entry,
        Ok(None) | Err(_) => {
            if spawn_lc {
                crate::yarm_log!("SPAWN_LIFECYCLE_IMAGE_RESOLVE_FAIL image_id={}", image_id);
            }
            return Err(SyscallError::InvalidArgs);
        }
    };
    let data = entry.file_data();
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_IMAGE_RESOLVE_OK image_id={} bytes={}",
            image_id,
            data.len()
        );
    }

    crate::yarm_log!(
        "KSPAWN_FROM_CPIO image_id={} name={} file_size={}",
        image_id,
        name,
        data.len()
    );

    // Exclusive, type-checked access to the shared ELF staging buffer; the claim
    // is released when `staging_claim` drops at end of handler.
    let mut staging_claim = VFS_ELF_STAGING.try_take().ok_or(SyscallError::Internal)?;
    let staging = staging_claim.as_mut_slice();
    let elf_len = data.len();
    if elf_len == 0 || elf_len > staging.len() {
        return Err(SyscallError::InvalidArgs);
    }
    staging[..elf_len].copy_from_slice(data);
    let elf_bytes = &staging[..elf_len];

    let image_path = match spawn_image_path_for_image_id(image_id) {
        Some(path) => path,
        None => {
            if spawn_lc {
                crate::yarm_log!("SPAWN_LIFECYCLE_BAD_IMAGE_ID image_id={}", image_id);
            }
            return Err(SyscallError::InvalidArgs);
        }
    };
    crate::yarm_log!("KSPAWN_FROM_CPIO path={}", image_path);
    if spawn_lc {
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_PARSE_BEGIN image_id={}", image_id);
    }
    let elf = ElfImageInfo::parse(image_id, elf_bytes).map_err(|_| SyscallError::InvalidArgs)?;
    crate::yarm_log!("KSPAWN_FROM_CPIO entry=0x{:x}", elf.entry);
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ELF_PARSE_OK image_id={} entry=0x{:x}",
            image_id,
            elf.entry
        );
    }

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

    let tid = kernel.allocate_thread_id().map_err(SyscallError::from)?;
    let (asid, _aspace_cap) = kernel
        .create_user_address_space()
        .map_err(SyscallError::from)?;
    crate::yarm_log!("KSPAWN_FROM_CPIO tid={} asid={}", tid, asid.0);
    if spawn_lc {
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_ASPACE_CREATE_OK tid={} asid={}",
            tid,
            asid.0
        );
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_BEGIN tid={} asid={}", tid, asid.0);
    }

    kernel
        .load_elf_pt_load_segments(asid, elf_bytes)
        .map_err(SyscallError::from)?;
    if spawn_lc {
        // load_elf_pt_load_segments finalizes the PT_LOAD segments (the
        // initramfs-backed zero-copy grant / staged copy) into the new ASID.
        crate::yarm_log!("SPAWN_LIFECYCLE_ELF_LOAD_OK tid={} asid={}", tid, asid.0);
        crate::yarm_log!("SPAWN_LIFECYCLE_ZC_LOAD_OK tid={} asid={}", tid, asid.0);
    }

    let spawner_tid = current_tid(kernel).unwrap_or(0);
    let (service_send_cap, service_recv_cap) = match kernel.create_endpoint(8) {
        Ok((_, send_cap, recv_cap)) => {
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                send_cap.0,
                recv_cap.0
            );
            (send_cap.0, recv_cap.0)
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
            (0u64, 0u64)
        }
    };
    let service_reply_recv_cap = match kernel.create_endpoint(8) {
        Ok((eid, _, recv_cap)) => {
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                eid,
                recv_cap.0
            );
            recv_cap.0
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err={:?}", e);
            0u64
        }
    };
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.grant_capability_task_to_task_with_rights(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(cap) => cap.0,
            Err(_) => service_send_cap,
        }
    } else {
        service_send_cap
    };

    let spawned = kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid,
            entry: elf.entry as usize,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args,
            spawner_tid,
            service_recv_cap,
            service_reply_recv_cap,
            extra_send_caps,
        })
        .map_err(SyscallError::from)?;

    crate::yarm_log!("KSPAWN_FROM_CPIO spawned_tid={}", spawned.tid);
    if spawn_lc {
        // Spawned system servers become services as they come up. TIDs are
        // monotonically allocated, so a spawned service tid that regresses below a
        // previously-observed service tid indicates a startup-order anomaly.
        use core::sync::atomic::{AtomicU64, Ordering};
        static LAST_SERVICE_TID: AtomicU64 = AtomicU64::new(0);
        let prev = LAST_SERVICE_TID.swap(spawned.tid, Ordering::Relaxed);
        if prev != 0 && spawned.tid < prev {
            crate::yarm_log!(
                "SPAWN_LIFECYCLE_SERVICE_ORDER_VIOLATION tid={} prev={}",
                spawned.tid,
                prev
            );
        }
        crate::yarm_log!(
            "SPAWN_LIFECYCLE_SERVICE_READY tid={} image_id={}",
            spawned.tid,
            image_id
        );
    }

    let packed_ret2 =
        if parent_pid != 0 && service_send_cap != 0 && caller_send_cap != service_send_cap {
            ((service_send_cap as u64) << 32) | (caller_send_cap as u64)
        } else {
            caller_send_cap as u64
        };
    frame.set_ok(
        0,
        usize::try_from(spawned.tid).map_err(|_| SyscallError::Internal)?,
        packed_ret2 as usize,
    );
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

/// Phase 3A: Spawn a process from an InitramfsFileSlice MemoryObject capability.
///
/// Access control: caller must be PM (TID == PM_BOOTSTRAP_TID).
///
/// ABI: arg0=image_id, arg1=mo_cap (CapId), arg2=parent_pid,
///      arg3=startup_args_ptr, arg4=startup_args_count
///
/// Resolves the MemoryObject → reads initrd slice → loads ELF via load_elf_with_mo_zero_copy
/// → spawns exactly like SpawnFromInitramfsFile.
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

    let tid = kernel.allocate_thread_id().map_err(SyscallError::from)?;
    let (asid, _aspace_cap) = kernel
        .create_user_address_space()
        .map_err(SyscallError::from)?;
    crate::yarm_log!("SPAWN_FROM_MO_TID tid={} asid={}", tid, asid.0);

    // Compute physical base of the initrd blob for zero-copy feasibility check.
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

    // Load ELF using zero-copy path (falls back to copy if alignment not feasible).
    let (entry, _first_vaddr, _heap_base, zc_pages, copied_pages) = kernel
        .load_elf_with_mo_zero_copy(
            image_id,
            asid,
            elf_bytes,
            initrd_phys_base,
            file_data_offset as u64,
        )
        .map_err(SyscallError::from)?;

    crate::yarm_log!(
        "PM_ELF_ZC_DONE image_id={} path={} zc_pages={} copied_pages={}",
        image_id,
        image_path,
        zc_pages,
        copied_pages
    );

    let spawner_tid = caller_tid;
    let (service_send_cap, service_recv_cap) = match kernel.create_endpoint(8) {
        Ok((_, send_cap, recv_cap)) => {
            crate::yarm_log!(
                "KSPAWN_EP_CREATED spawner_tid={} send_cap={} recv_cap={}",
                spawner_tid,
                send_cap.0,
                recv_cap.0
            );
            (send_cap.0, recv_cap.0)
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_EP_CREATE_FAIL err={:?}", e);
            (0u64, 0u64)
        }
    };
    let service_reply_recv_cap = match kernel.create_endpoint(8) {
        Ok((eid, _, recv_cap)) => {
            crate::yarm_log!(
                "SPAWN_SERVICE_REPLY_RECV_CAP_CREATED endpoint={} cap={}",
                eid,
                recv_cap.0
            );
            recv_cap.0
        }
        Err(e) => {
            crate::yarm_log!("KSPAWN_REPLY_EP_CREATE_FAIL err={:?}", e);
            0u64
        }
    };
    let caller_send_cap = if parent_pid != 0 && service_send_cap != 0 {
        match kernel.grant_capability_task_to_task_with_rights(
            spawner_tid,
            CapId(service_send_cap),
            parent_pid,
            CapRights::SEND,
        ) {
            Ok(cap) => cap.0,
            Err(_) => service_send_cap,
        }
    } else {
        service_send_cap
    };

    let spawned = kernel
        .spawn_user_task_from_image(UserImageSpec {
            tid,
            entry,
            asid: Some(asid),
            class: TaskClass::SystemServer,
            startup_args,
            spawner_tid,
            service_recv_cap,
            service_reply_recv_cap,
            extra_send_caps,
        })
        .map_err(SyscallError::from)?;

    crate::yarm_log!(
        "SPAWN_FROM_MO_OK image_id={} spawned_tid={}",
        image_id,
        spawned.tid
    );

    let packed_ret2 =
        if parent_pid != 0 && service_send_cap != 0 && caller_send_cap != service_send_cap {
            ((service_send_cap as u64) << 32) | (caller_send_cap as u64)
        } else {
            caller_send_cap as u64
        };
    frame.set_ok(
        0,
        usize::try_from(spawned.tid).map_err(|_| SyscallError::Internal)?,
        packed_ret2 as usize,
    );
    Ok(())
}
