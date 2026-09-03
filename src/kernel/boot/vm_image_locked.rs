// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN-VM2 — the rank-local VM/memory bodies a process image is built out of.
//!
//! # What this file is
//!
//! Every function here takes `&mut AddressSpaceManager` (VM, rank 5) and/or
//! `&mut MemorySubsystem` (rank 6) EXPLICITLY, and nothing else. None of them can reach the
//! scheduler, the task table, IPC, capabilities, the current CPU or `KernelState` — not by
//! convention, but because they are free functions that were never handed one.
//!
//! The `KernelState` methods that used to hold these bodies are now one-line delegations that
//! acquire the locks and call in. That direction matters: it is what keeps a single policy. A
//! second copy "for the split path" is exactly the failure mode the U9 programme keeps finding,
//! so there is no second copy — the broad path executes the same instructions.
//!
//! # The two lower-rank reaches, and where they went
//!
//! An audit of the transitive callee closure of the whole image path found exactly two places
//! that touched anything below rank 5:
//!
//! 1. **`create_user_address_space` minted a capability.** It returned `(Asid, CapId)`, and the
//!    `CapId` came from `mint_capability_for_current_context`, which reads the CURRENT TASK's
//!    CNode (task rank 2) and mints into it (capability rank 4) — two ranks below VM, acquired
//!    while VM was held. [`create_address_space_locked`] returns the ASID alone. Publishing the
//!    capability is the broad wrapper's job, after both VM and memory are released.
//!
//! 2. **`destroy_user_address_space_by_asid` submitted TLB shootdowns.** It reads the online and
//!    wake-only CPU bitmaps (scheduler rank 1) and posts SMP mailbox work. That work is real and
//!    stays, for a live address space. It is not owed at all for one that was never CPU-resident,
//!    and that is the only kind this file destroys: see [`destroy_unresident_address_space_locked`].
//!
//! Everything else in the closure — frame allocation and release, raw page mapping, the ELF
//! segment loaders, `copy_to_user`, the user stack and its guard page, the COW page set, the
//! MemoryObject map/pin refcounts — was already VM+memory only. The arch page-table backend
//! reached from here takes the page-table frame pool's own leaf lock, which sits outside the
//! rank ladder and is already taken from rank-5 paths today.
//!
//! # Ordering
//!
//! Callers that need both take them through [`KernelState::with_vm_then_memory_mut`], which fixes
//! the order at VM(5) → memory(6). Nothing here acquires a lock itself, so nothing here can get
//! that order wrong.

use super::defs::MemorySubsystem;
use super::{KernelError, KernelState, kernel_mut};
use crate::kernel::vm::{
    AddressSpaceManager, Asid, DrainedMapping, MAX_MAPPINGS, Mapping, PAGE_SIZE, PageFlags,
    PhysAddr, VirtAddr, VmError,
};

/// Create a user address space and return its ASID — and ONLY its ASID.
///
/// The broad `create_user_address_space` returns `(Asid, CapId)`, and that second value is the
/// reason the whole image path used to reach two ranks below VM: minting it reads the current
/// task's CNode (rank 2) and writes a capability space (rank 4). Splitting it off is what makes
/// the provisioning expressible in VM+memory terms at all.
///
/// `create_user_space` is already careful about its own rollback: it confirms a free registry slot
/// BEFORE allocating an ASID or registering an arch page-table root, so a full manager performs
/// neither.
pub(crate) fn create_address_space_locked(
    vm: &mut AddressSpaceManager,
) -> Result<Asid, KernelError> {
    vm.create_user_space().map_err(KernelError::Vm)
}

/// Take an address space out of the registry and hand back everything it had mapped.
///
/// `pending_cpu_bitmap` is the set of CPUs that must acknowledge a shootdown before the ASID may
/// be reissued. Passing `0` means "no CPU can be holding a translation for this ASID", and
/// `destroy_and_collect_mappings` then skips the retired-ASID slot entirely — which is both
/// correct and necessary for a never-resident space: see
/// [`destroy_unresident_address_space_locked`].
///
/// This is deliberately only the FIRST half of a teardown. The second half —
/// [`reclaim_drained_mappings_locked`] — is separate so the broad caller can keep the documented
/// two-phase-unmap ordering, submitting its shootdown work items between the two.
pub(crate) fn drain_address_space_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    asid: Asid,
    pending_cpu_bitmap: crate::kernel::topology::CpuBitmap,
) -> Result<[Option<DrainedMapping>; MAX_MAPPINGS], KernelError> {
    memory.cow_pages.remove(&asid.0);
    vm.destroy_and_collect_mappings(asid, pending_cpu_bitmap)
        .map_err(KernelError::Vm)
}

/// Return every page a drained address space was holding, through the owner each page belongs to.
///
/// Three owners, in this order, per page — the U9-MO2 and U9-ASPACE1 result, unchanged:
///
/// 1. `note_mapping_removed_locked` drops the MemoryObject's map refcount, if one describes it;
/// 2. `reclaim_memory_object_for_phys_locked` runs the backing-aware MemoryObject lifecycle;
/// 3. [`release_unreferenced_user_frame_locked`] returns the frames no MemoryObject ever
///    described — every ELF `PT_LOAD` page, the stack, the guard page — to the allocator.
pub(crate) fn reclaim_drained_mappings_locked(
    vm: &AddressSpaceManager,
    memory: &mut MemorySubsystem,
    drained: [Option<DrainedMapping>; MAX_MAPPINGS],
) {
    for dm in drained.into_iter().flatten() {
        for page in 0..dm.pages {
            let phys = PhysAddr(dm.mapping.phys.0 + (page as u64 * PAGE_SIZE as u64));
            KernelState::note_mapping_removed_locked(memory, phys);
            KernelState::reclaim_memory_object_for_phys_locked(memory, phys);
            release_unreferenced_user_frame_locked(vm, memory, phys);
        }
    }
}

/// THE exact destroy for an address space that was never CPU-resident.
///
/// # Why this may skip the shootdown, and why it must
///
/// An ASID is CPU-resident exactly when some online CPU's CURRENT task carries it — that is the
/// definition `live_cpu_bitmap_for_asid` uses. A task carries an ASID only through
/// `tcb.asid = Some(..)`, and in the spawn path the only sites that do so are inside
/// `spawn_image_after_claim`, i.e. the commit, which runs after provisioning has returned. So
/// while a provisioning is in flight no TCB names this ASID, no CPU is running with it, and the
/// shootdown target set is empty by construction.
///
/// "May skip" is the easy half. "Must" is the half worth stating: the broad teardown passes
/// `online & !wake_only` rather than the resident set, so it registers a retired ASID and posts
/// mailbox work for EVERY destroy. The retired array is `MAX_ADDRESS_SPACES` deep and drains only
/// on acknowledgement, so a rollback path that used it would consume a slot per failed spawn and,
/// after enough of them, `destroy_and_collect_mappings` would correctly refuse with
/// `VmError::Full` — turning an exact rollback into a leak. A never-resident space owes nothing,
/// takes no slot, and can therefore be rolled back without bound.
///
/// Rollback order, which is the reverse of how a provisioning acquires:
/// stack and guard page, initrd window, ELF segments (all unmapped together by the drain, which
/// also frees the arch page-table pages behind them) → the frames and MemoryObject references
/// behind those mappings → the address-space registry entry → the ASID itself.
///
/// Inert on repetition: the second call finds no registry entry and returns
/// `KernelError::Vm(VmError::InvalidAsid)` having mutated nothing.
pub(crate) fn destroy_unresident_address_space_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    asid: Asid,
) -> Result<(), KernelError> {
    let drained = drain_address_space_locked(vm, memory, asid, 0)?;
    reclaim_drained_mappings_locked(vm, memory, drained);
    Ok(())
}

/// Take one frame for user data from the general allocator.
pub(crate) fn alloc_user_data_frame_locked(
    memory: &mut MemorySubsystem,
) -> Result<u64, KernelError> {
    let pa = kernel_mut(&mut memory.frame_allocator)
        .alloc_frame()
        .map_err(|_| KernelError::MemoryObjectFull)?;
    #[cfg(not(feature = "hosted-dev"))]
    if let Some((rs, re)) = crate::kernel::frame_allocator::is_pa_in_pt_pool(pa) {
        crate::yarm_log!(
            "PMEM_ALLOC_PT_POOL_BUG pa=0x{:x} pt_range=0x{:x}..0x{:x}",
            pa,
            rs,
            re
        );
        panic!("PMEM_ALLOC_PT_POOL_BUG: main frame allocator returned a PT-pool PA");
    }
    #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
    crate::yarm_log!("PMEM_ALLOC_FRAME pa=0x{:x} owner=user", pa);
    Ok(pa)
}

/// U9-ASPACE1 — return a drained user frame to the allocator once nothing can reach it.
///
/// # The leak this closes
///
/// Address-space teardown reclaimed a drained page through [`KernelState::note_mapping_removed_locked`] and
/// [`KernelState::reclaim_memory_object_for_phys_locked`], and both are MemoryObject-scoped: they find the
/// object whose `phys` matches and act on its refcount. A page that no MemoryObject ever
/// described was therefore never reclaimed by anything.
///
/// That is not an exotic case — it is the ordinary one. `alloc_user_data_frame` takes a bare
/// frame from the allocator and registers no object, so every ELF `PT_LOAD` page an image
/// loads is exactly this shape. Each address space destroyed lost one frame per such page,
/// for the life of the boot, whether the space died from a task exiting or from a spawn
/// failing.
///
/// # Why it is safe to free here
///
/// Four independent conditions have to hold, and each is checked against an owner that
/// already exists rather than against a count this function maintains:
///
/// 1. **No MemoryObject describes the frame.** If one does, the frame belongs to the
///    MemoryObject lifecycle, which has just had its say one line above. A still-referenced
///    object keeps its page; that decision is its refcount's to make, not this teardown's.
/// 2. **No live address space still maps it.** A COW child, a shared region and an aliased
///    zero-copy grant all map a frame from more than one space. The dying space has already
///    been taken out of the registry by `destroy_and_collect_mappings`, so
///    `AddressSpaceManager::any_mapping_for_phys` answers about everyone *else*: whoever
///    drops the last mapping is the one that frees it.
/// 3. **The allocator handed it out in the first place.** This is the condition that makes
///    the other kinds of physical memory safe, and it is exact rather than approximate.
///    `Bootstrap::init_state_into` sanitizes every reserved range out of the boot regions
///    BEFORE the allocator is seeded, and seeds the page-table pool from a strictly disjoint
///    slice, so the main allocator's inventory is by construction nothing but user-data
///    frames it issued: a borrowed initramfs page, a reserved range and a PT-pool page are
///    not in it and never were. `reserve_frame`, which would be the one way to track a frame
///    the allocator did not issue, has no production caller. `free_frame` refuses any frame
///    it has no tracking slot for, so those pages are declined rather than freed, and
///    `AlreadyFree` here means "not mine" — a correct outcome, not an error.
///
/// Deliberately NOT consulted: `is_pa_reserved` and `is_pa_in_pt_pool`. They look like the
/// obvious guard and they are the wrong authority — they read process-global registries that
/// are never reset, so in a test binary that builds many kernels one kernel's ranges answer
/// another kernel's question. Condition 3 asks the allocator that actually owns the frame.
///
/// Condition 3 also makes repetition inert: a second teardown naming the same page finds it
/// untracked and declines. And because `free_frame` decrements a shared frame's refcount
/// rather than releasing it outright, a frame that some future path retains stays safe even
/// before condition 2 could see it.///
/// U9-SPAWN-VM2 made this the only copy: the `KernelState` method that used to hold it reached
/// both subsystems one at a time, and every caller now goes through the address-space teardown,
/// which holds them together.
pub(crate) fn release_unreferenced_user_frame_locked(
    vm: &AddressSpaceManager,
    memory: &mut MemorySubsystem,
    phys: PhysAddr,
) {
    let described_by_memory_object = memory
        .memory_objects
        .iter()
        .flatten()
        .any(|object| object.phys == phys);
    if described_by_memory_object {
        return;
    }
    if vm.any_mapping_for_phys(phys) {
        return;
    }
    let released = kernel_mut(&mut memory.frame_allocator)
        .free_frame(phys.0)
        .is_ok();
    if released {
        crate::yarm_log!(
            "ASPACE_FRAME_RELEASED phys=0x{:x} owner=user_backing",
            phys.0
        );
    }
}

/// Install one page in one address space, with exactly the permissions given.
///
/// The displaced mapping, if any, is retired through its owners before the new one is counted —
/// the same sequence `map_user_page_in_asid_raw` always performed, now expressed against the two
/// subsystems instead of against `self`.
pub(crate) fn map_user_page_in_asid_raw_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    asid: Asid,
    virt: VirtAddr,
    mapping: Mapping,
) -> Result<Option<Mapping>, KernelError> {
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "MAP_USER_RAW_BEGIN asid={} virt=0x{:x} phys=0x{:x} user={} rwx={}{}{}",
            asid.0,
            virt.0,
            mapping.phys.0,
            mapping.flags.user,
            if mapping.flags.read { "r" } else { "-" },
            if mapping.flags.write { "w" } else { "-" },
            if mapping.flags.execute { "x" } else { "-" }
        );
    }
    let old = {
        let aspace = vm
            .get_mut(asid)
            .ok_or(KernelError::Vm(VmError::InvalidAsid))?;
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "MAP_USER_RAW_ASPACE asid={} aspace_asid={}",
                asid.0,
                aspace.asid().map(|asid| asid.0).unwrap_or(0)
            );
        }
        aspace.map_page(virt, mapping).map_err(KernelError::Vm)?
    };
    let resolved = crate::arch::selected_isa::page_table::resolve_page(asid, virt).is_some();
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "MAP_USER_RAW_DONE asid={} virt=0x{:x} had_old={} resolve_ok={}",
            asid.0,
            virt.0,
            old.is_some(),
            resolved
        );
    }
    if let Some(old_mapping) = old {
        KernelState::clear_cow_page_locked(memory, asid, virt);
        KernelState::note_mapping_removed_locked(memory, old_mapping.phys);
        KernelState::reclaim_memory_object_for_phys_locked(memory, old_mapping.phys);
    }
    if mapping.flags.write {
        KernelState::clear_cow_page_locked(memory, asid, virt);
    }
    KernelState::note_mapping_inserted_locked(memory, mapping.phys);
    Ok(old)
}

/// Resolve a user virtual address to its physical address, checking the access is permitted.
///
/// Hosted twin of the freestanding variant below; both read only the address-space shadow.
#[cfg(feature = "hosted-dev")]
pub(crate) fn validate_user_access_for_asid_locked(
    vm: &AddressSpaceManager,
    asid: Asid,
    va: usize,
    need_write: bool,
) -> Result<u64, KernelError> {
    let page_base = va & !(PAGE_SIZE - 1usize);
    let page_off = (va - page_base) as u64;
    let aspace = vm.get(asid).ok_or(KernelError::Vm(VmError::InvalidAsid))?;
    let mapping = aspace
        .resolve(VirtAddr(page_base as u64))
        .ok_or(KernelError::UserMemoryFault)?;
    if !mapping.flags.user || !mapping.flags.read || (need_write && !mapping.flags.write) {
        return Err(KernelError::UserMemoryFault);
    }
    mapping
        .phys
        .0
        .checked_add(page_off)
        .ok_or(KernelError::UserMemoryFault)
}

/// Freestanding twin: the shadow answers only "does this address space exist"; the HARDWARE page
/// table entry decides presence, permission and the frame the access lands in. Copied verbatim
/// from `KernelState::validate_user_access_for_asid` — the arch-specific permission decode stays
/// in `KernelState::pte_allows_user_access`, which is the one place that knows each ISA's bits.
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn validate_user_access_for_asid_locked(
    vm: &AddressSpaceManager,
    asid: Asid,
    va: usize,
    need_write: bool,
) -> Result<u64, KernelError> {
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "VALIDATE asid={} va=0x{:x} need_write={}",
            asid.0,
            va,
            need_write
        );
    }
    let page_base = va & !(PAGE_SIZE - 1usize);
    let page_off = (va - page_base) as u64;
    let user_space_exists = vm.get(asid).is_some();
    let shadow_mapping_present = vm
        .get(asid)
        .and_then(|aspace| aspace.resolve(VirtAddr(page_base as u64)))
        .is_some();
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!("ASID_EXISTS={}", user_space_exists);
    }
    if !user_space_exists {
        return Err(KernelError::Vm(VmError::InvalidAsid));
    }
    let pte_result =
        crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(page_base as u64));
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "VALIDATE_PAGE asid={} page_va=0x{:x} shadow_present={} resolve_ok={}",
            asid.0,
            page_base,
            shadow_mapping_present,
            pte_result.is_some()
        );
    }
    let pte = pte_result.ok_or(KernelError::UserMemoryFault)?;
    if !KernelState::pte_allows_user_access(pte, need_write) {
        if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
            crate::yarm_log!(
                "VALIDATE_PERM_FAIL asid={} page_va=0x{:x} pte=0x{:x}",
                asid.0,
                page_base,
                pte.0
            );
        }
        return Err(KernelError::UserMemoryFault);
    }
    let resolved_phys = pte
        .addr()
        .checked_add(page_off)
        .ok_or(KernelError::UserMemoryFault)?;
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "VALIDATE_OK asid={} page_va=0x{:x} page_off=0x{:x} phys=0x{:x}",
            asid.0,
            page_base,
            page_off,
            resolved_phys
        );
    }
    Ok(resolved_phys)
}

/// Write one byte of already-validated user memory.
#[cfg(feature = "hosted-dev")]
pub(crate) fn write_user_byte_locked(
    memory: &mut MemorySubsystem,
    asid: Asid,
    va: VirtAddr,
    value: u8,
) {
    memory.user_memory.insert((asid.0, va.0), value);
}

/// Freestanding twin: `va` is the physical address the validation resolved.
#[cfg(not(feature = "hosted-dev"))]
pub(crate) fn write_user_byte_locked(
    _memory: &mut MemorySubsystem,
    _asid: Asid,
    va: VirtAddr,
    value: u8,
) -> Result<(), KernelError> {
    let ptr = KernelState::phys_to_direct_map_ptr(va.0).ok_or(KernelError::UserMemoryFault)?;
    // SAFETY: `va` came from `validate_user_access_for_asid_locked`, which resolved it through
    // the installed page table and confirmed the mapping is a writable user page.
    unsafe {
        core::ptr::write_volatile(ptr, value);
    }
    Ok(())
}

/// Copy a byte range into a user address space, validating each page as it is crossed.
pub(crate) fn copy_to_user_locked(
    vm: &AddressSpaceManager,
    memory: &mut MemorySubsystem,
    asid: Asid,
    va: VirtAddr,
    bytes: &[u8],
) -> Result<(), KernelError> {
    if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
        crate::yarm_log!(
            "COPY_TO_USER asid={} va=0x{:x} len={}",
            asid.0,
            va.0,
            bytes.len()
        );
    }
    let mut last_page_base: Option<usize> = None;
    for (i, &byte) in bytes.iter().enumerate() {
        let addr = va.0 as usize + i;
        let page_base = addr & !(PAGE_SIZE - 1usize);
        if last_page_base != Some(page_base) {
            if cfg!(all(not(feature = "hosted-dev"), feature = "trace_boot_vm")) {
                let pte_present = crate::arch::selected_isa::page_table::resolve_page(
                    asid,
                    VirtAddr(page_base as u64),
                )
                .is_some();
                crate::yarm_log!(
                    "COPY_TO_USER_PAGE asid={} page_va=0x{:x} pte_present={} offset={}",
                    asid.0,
                    page_base,
                    pte_present,
                    i
                );
            }
            last_page_base = Some(page_base);
        }
        let phys = validate_user_access_for_asid_locked(vm, asid, addr, true)?;
        #[cfg(feature = "hosted-dev")]
        write_user_byte_locked(memory, asid, VirtAddr(phys), byte);
        #[cfg(not(feature = "hosted-dev"))]
        write_user_byte_locked(memory, asid, VirtAddr(phys), byte)?;
    }
    Ok(())
}

/// THE user-stack allocator, in rank-local form.
///
/// The slot is derived from `tid`, but the stack is installed in the address space the CALLER
/// names — which is what lets a provisioning give a stack to a child whose ASID is deliberately
/// not yet bound to any task. The layout, guard page, overlap refusal and post-map resolve probe
/// are unchanged from `allocate_user_stack_with_guard`; only the plumbing moved.
pub(crate) fn allocate_user_stack_locked(
    vm: &mut AddressSpaceManager,
    memory: &mut MemorySubsystem,
    asid: Asid,
    tid: u64,
    stack_pages: usize,
) -> Result<VirtAddr, KernelError> {
    if stack_pages == 0 {
        return Err(KernelError::WrongObject);
    }
    let stack_bytes = (stack_pages as u64)
        .checked_mul(PAGE_SIZE as u64)
        .ok_or(KernelError::WrongObject)?;
    let stride = super::thread_state::USER_STACK_STRIDE_BYTES.max(stack_bytes + PAGE_SIZE as u64);
    // USER_STACK_TOP_BASE may be small on architectures with a narrow user VA range (e.g. the
    // AArch64 prototype: 1 GB). Dynamic TIDs (>= 10000) can exceed the available slots if we
    // multiply directly, causing checked_sub to return None. Wrap tid into the available slot
    // count instead; the per-address-space overlap check below catches any actual VA conflicts
    // within the same process.
    let max_slots = (super::thread_state::USER_STACK_TOP_BASE / stride).max(1);
    let slot = tid % max_slots;
    let top = super::thread_state::USER_STACK_TOP_BASE
        .checked_sub(slot.saturating_mul(stride))
        .ok_or(KernelError::WrongObject)?;
    let base = top
        .checked_sub(stack_bytes)
        .ok_or(KernelError::WrongObject)?;
    let guard = base
        .checked_sub(PAGE_SIZE as u64)
        .ok_or(KernelError::WrongObject)?;
    if top >= crate::kernel::vm::KERNEL_SPACE_BASE || guard == 0 {
        return Err(KernelError::WrongObject);
    }
    for page in (guard..top).step_by(PAGE_SIZE) {
        if vm
            .get(asid)
            .and_then(|aspace| aspace.resolve(VirtAddr(page)))
            .is_some()
        {
            return Err(KernelError::WrongObject);
        }
    }
    for page in (base..top).step_by(PAGE_SIZE) {
        let phys = PhysAddr(alloc_user_data_frame_locked(memory)?);
        map_user_page_in_asid_raw_locked(
            vm,
            memory,
            asid,
            VirtAddr(page),
            Mapping {
                phys,
                flags: PageFlags::USER_RW,
            },
        )?;
        #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
        crate::yarm_log!(
            "KSPAWN_NEW_TASK_STACK tid={} asid={} stack_va=0x{:x} pa=0x{:x} stack_base=0x{:x} stack_top=0x{:x}",
            tid,
            asid.0,
            page,
            phys.0,
            base,
            top
        );
    }
    let guard_phys = PhysAddr(alloc_user_data_frame_locked(memory)?);
    map_user_page_in_asid_raw_locked(
        vm,
        memory,
        asid,
        VirtAddr(guard),
        Mapping {
            phys: guard_phys,
            flags: PageFlags::GUARD,
        },
    )?;
    if cfg!(not(feature = "hosted-dev")) {
        crate::yarm_log!(
            "USER_STACK asid={} base=0x{:x} top=0x{:x}",
            asid.0,
            base,
            top
        );
    }
    let stack_probe = VirtAddr(top - 8);
    let stack_resolve =
        crate::arch::selected_isa::page_table::resolve_page(asid, stack_probe).is_some();
    if cfg!(not(feature = "hosted-dev")) {
        crate::yarm_log!(
            "USER_STACK_RESOLVE asid={} probe=0x{:x} ok={}",
            asid.0,
            stack_probe.0,
            stack_resolve
        );
    }
    if !stack_resolve {
        return Err(KernelError::UserMemoryFault);
    }
    Ok(VirtAddr(top))
}
