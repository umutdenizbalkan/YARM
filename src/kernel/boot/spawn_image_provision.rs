// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-SPAWN-VM1 — THE spawn-image provisioner: one owner for a child's address space, image and
//! user stack, with one exact rollback.
//!
//! # What this consolidates
//!
//! Provisioning a process image was four steps spread across two files: create the address space
//! (and mint its capability), load the ELF, map the initrd window for `initramfs_srv`, and — much
//! later, on the far side of the reservation claim — allocate the user stack. Each had its own
//! failure arm, and until U9-SPAWN1 SP-3 gave the spawn a resource ledger, most of those arms
//! leaked. SP-3 made the *unwind* correct by calling the right owners; this makes the
//! *provisioning* one transaction, so the phases and their rollback live together.
//!
//! # Why rollback owes no TLB shootdown
//!
//! This is the property that makes an exact rollback possible at all, and it is established from
//! source rather than assumed:
//!
//! 1. An ASID is CPU-resident exactly when some online CPU's CURRENT task carries it —
//!    that is the definition `live_cpu_bitmap_for_asid` uses
//!    (`current_tid_on_cpu(cpu).and_then(task_asid) == Some(asid)`).
//! 2. A task carries an ASID only through `tcb.asid = Some(..)`. Every such site in the kernel is
//!    enumerated by the guard `every_asid_binding_site_is_accounted_for`: two are inside
//!    `spawn_image_after_claim` (the COMMIT, after this provisioner has returned), one is AP
//!    bring-up, one is the shared-region client ASID, one is the fork child, and `bind_task_asid`
//!    has test-only callers.
//! 3. `AddressSpaceManager::allocate_asid` returns a candidate only when `!asid_in_use(candidate)`,
//!    which excludes live AND retired-but-unacknowledged ASIDs.
//!
//! So between `create_user_address_space` and the commit, no TCB carries this ASID, therefore no
//! CPU is running with it, therefore `live_cpu_bitmap_for_asid` is empty and there is nothing to
//! shoot down. The child is `ReservedUnstarted` throughout — it cannot be dispatched, so it cannot
//! make its own ASID resident either.
//!
//! Rollback consequently reduces to: unmap and free what we mapped, then destroy the address
//! space. The repaired teardown owner (`destroy_user_address_space_by_asid`, U9-ASPACE1 §1) does
//! exactly that in one call — it drains every mapping, reclaims MemoryObject-backed frames through
//! the backing-aware lifecycle, and returns plain user backing to the allocator — so this module
//! adds no unmapping policy of its own.
//!
//! # Image-source adapters carry no VM policy
//!
//! [`ImageSource`] is the ONLY thing NR 23 and NR 29 differ by: where immutable image bytes come
//! from and which loader reads them. Everything after that — the address space, its capability,
//! the stack, the extents, the rollback — is shared and stated once.

use super::*;
use crate::kernel::vm::{CachePolicy, PAGE_SIZE};

/// How a spawn obtains its immutable image bytes. The ONLY axis NR 23 and NR 29 differ on.
pub(crate) enum ImageSource<'a> {
    /// NR 23: PT_LOAD segments staged from a kernel-side slice, entry taken from the caller's
    /// already-parsed header.
    PtLoadSegments { elf: &'a [u8], entry: usize },
    /// NR 29: the initramfs-backed zero-copy grant loader, which reports the entry itself.
    ZeroCopyInitramfsSlice {
        image_id: u64,
        elf: &'a [u8],
        initrd_phys_base: u64,
        file_initrd_offset: u64,
    },
}

/// What one provisioning produced, and what it owns until the caller commits or unwinds.
///
/// Identity-bearing: the ASID and the capability naming it are the two things a caller needs to
/// either finish the spawn or give the provisioning back, and the entry/stack are the facts the
/// commit writes into the child's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageProvision {
    /// The child's address space. Not CPU-resident until the commit binds it to a task.
    pub(crate) asid: Asid,
    /// The MAP/READ/WRITE capability `create_user_address_space` minted for it, in the caller's
    /// cspace. Recorded because the caller's ledger has to revoke it — the address-space teardown
    /// does not own it (U9-SPAWN1 SP-3 found this the hard way).
    pub(crate) aspace_cap: CapId,
    /// The image's entry point.
    pub(crate) entry: usize,
    /// Top of the child's user stack, already mapped in `asid`.
    pub(crate) stack_top: VirtAddr,
    /// Zero-copy and copied page counts, for the `PM_ELF_ZC_DONE` accounting NR 29 reports.
    pub(crate) zc_pages: usize,
    pub(crate) copied_pages: usize,
}

/// The bounded, PURE result of validating an image before anything is mutated.
///
/// "Bounded" is the operative word: it records the extents the load will occupy without walking
/// the program headers a second time at map time, so a malformed or overlarge image is refused
/// while the transaction still owns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageLoadPlan {
    /// `e_entry` as the header states it. A FACT about the image, not the entry the spawn will
    /// use: NR 23 carries its own already-parsed entry, and NR 29's loader reports one.
    pub(crate) header_entry: u64,
    /// Lowest and highest virtual addresses any PT_LOAD segment will occupy.
    pub(crate) first_vaddr: u64,
    pub(crate) last_vaddr_end: u64,
}

/// Validate an image and compute its load plan. Pure: touches no kernel state, allocates nothing,
/// and — this is the point — runs before the address space exists, so a refusal costs nothing to
/// unwind.
///
/// # Admission is deliberately IDENTICAL to the loader's
///
/// This checks exactly what `load_elf_pt_load_segments` checks, in the same order and with the
/// same `WrongObject` verdict: ELF64 magic and class, a non-empty program-header table of legal
/// entry size that lies inside the image, and per-PT_LOAD `p_filesz <= p_memsz` with a file range
/// inside the image and no address arithmetic that overflows. It is a second EVALUATION of the
/// same rules, not a second POLICY — an image this refuses is one the loader would have refused a
/// few hundred instructions later, holding an address space. Making it any stricter would silently
/// narrow what the kernel will spawn, so it does not: `p_flags` are the loader's to interpret, and
/// `e_entry` is recorded as a fact rather than judged.
///
/// The one check the loader cannot make is the extent bound, because it discovers the extents as
/// it maps them: a PT_LOAD span that reaches into kernel space is refused here, before a single
/// page of it is installed.
pub(crate) fn plan_image_load(elf: &[u8]) -> Result<ImageLoadPlan, KernelError> {
    let (header_entry, first_vaddr, last_vaddr_end) = elf_load_extents(elf)?;
    if last_vaddr_end <= first_vaddr || last_vaddr_end > crate::kernel::vm::KERNEL_SPACE_BASE {
        return Err(KernelError::WrongObject);
    }
    Ok(ImageLoadPlan {
        header_entry,
        first_vaddr,
        last_vaddr_end,
    })
}

/// The header check and PT_LOAD extent walk. Bounded by `e_phnum`, and every arithmetic step is
/// checked, so a crafted header cannot make the plan itself overflow.
///
/// Returns `(e_entry, first PT_LOAD vaddr, highest PT_LOAD vaddr end)`.
fn elf_load_extents(image: &[u8]) -> Result<(u64, u64, u64), KernelError> {
    const EHDR: usize = 64;
    const PT_LOAD: u32 = 1;
    // The loader's own header gate, evaluated here instead: ELF64 magic and ELFCLASS64.
    if image.len() < EHDR || &image[..4] != b"\x7FELF" || image[4] != 2 {
        return Err(KernelError::WrongObject);
    }
    let rd_u16 = |off: usize| -> Result<u16, KernelError> {
        image
            .get(off..off + 2)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
            .ok_or(KernelError::WrongObject)
    };
    let rd_u32 = |off: usize| -> Result<u32, KernelError> {
        image
            .get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or(KernelError::WrongObject)
    };
    let rd_u64 = |off: usize| -> Result<u64, KernelError> {
        image
            .get(off..off + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or(KernelError::WrongObject)
    };
    let phoff = rd_u64(32)? as usize;
    let phentsize = rd_u16(54)? as usize;
    let phnum = rd_u16(56)? as usize;
    if phnum == 0 || phentsize < 56 {
        return Err(KernelError::WrongObject);
    }
    let table = phnum
        .checked_mul(phentsize)
        .and_then(|n| phoff.checked_add(n))
        .ok_or(KernelError::WrongObject)?;
    if table > image.len() {
        return Err(KernelError::WrongObject);
    }
    let mut first = u64::MAX;
    let mut end = 0u64;
    let mut saw = false;
    for idx in 0..phnum {
        let base = phoff + idx * phentsize;
        if rd_u32(base)? != PT_LOAD {
            continue;
        }
        let p_offset = rd_u64(base + 8)?;
        let p_vaddr = rd_u64(base + 16)?;
        let p_filesz = rd_u64(base + 32)?;
        let p_memsz = rd_u64(base + 40)?;
        if p_filesz > p_memsz {
            return Err(KernelError::WrongObject);
        }
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or(KernelError::WrongObject)?;
        if file_end > image.len() as u64 {
            return Err(KernelError::WrongObject);
        }
        let seg_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or(KernelError::WrongObject)?;
        first = first.min(p_vaddr);
        end = end.max(seg_end);
        saw = true;
    }
    if !saw {
        return Err(KernelError::WrongObject);
    }
    Ok((rd_u64(24)?, first, end))
}

impl KernelState {
    /// THE spawn-image provisioner.
    ///
    /// Phases, in order, each releasing what it holds before the next begins:
    ///
    /// 1. **Plan** — validate the image and compute its extents. Pure; nothing is owned yet, so a
    ///    malformed image costs nothing to refuse.
    /// 2. **Address space** — create the ASID and mint its capability.
    /// 3. **Load** — the image-source adapter maps the segments and their backing.
    /// 4. **Initrd window** — NR 23's `initramfs_srv` only; tolerated on failure, exactly as
    ///    before, because the server falls back to its syscall bridge.
    /// 5. **Stack** — allocate and map the child's user stack in the same address space.
    /// 6. **Token** — return the ASID, its capability, the entry and the stack top.
    ///
    /// On any failure from phase 3 onward the whole address space is destroyed through the
    /// repaired teardown owner, which unmaps every page, returns MemoryObject-backed frames
    /// through the backing-aware lifecycle and plain user backing to the allocator — one call,
    /// once, and no shootdown is owed because the ASID was never CPU-resident. The address-space
    /// capability is handed back to the caller's ledger rather than revoked here: it lives in the
    /// CALLER's cspace, and the ledger is the owner that holds cspace entries.
    pub(crate) fn provision_spawn_image(
        &mut self,
        tid: u64,
        image_path: &str,
        source: ImageSource<'_>,
        map_initrd_window: bool,
        startup_args: &mut [u64; 18],
        stack_pages: usize,
    ) -> Result<ImageProvision, KernelError> {
        // ── 1. Plan. Nothing is owned yet, so a refusal here is free. ───────────────────
        let (elf, declared_entry) = match &source {
            ImageSource::PtLoadSegments { elf, entry } => (*elf, Some(*entry)),
            ImageSource::ZeroCopyInitramfsSlice { elf, .. } => (*elf, None),
        };
        let plan = plan_image_load(elf)?;
        // NR 23 supplies the entry from its own parse of this same header. Zero is refused by the
        // commit regardless; refusing it now means refusing it before an address space exists.
        // NR 29 has no declared entry — its loader reports one — so there is nothing to check.
        if declared_entry == Some(0) {
            return Err(KernelError::WrongObject);
        }

        // ── 2. Address space. From here a failure owes a teardown. ──────────────────────
        let (asid, aspace_cap) = self.create_user_address_space()?;

        // ── 3. Load, through the source's adapter. ──────────────────────────────────────
        let loaded = match source {
            ImageSource::PtLoadSegments { elf, entry } => self
                .load_elf_pt_load_segments(asid, elf)
                .map(|_| (entry, 0usize, 0usize)),
            ImageSource::ZeroCopyInitramfsSlice {
                image_id,
                elf,
                initrd_phys_base,
                file_initrd_offset,
            } => self
                .load_elf_with_mo_zero_copy(
                    image_id,
                    asid,
                    elf,
                    initrd_phys_base,
                    file_initrd_offset,
                )
                .map(|(entry, _first, _heap, zc, copied)| (entry, zc, copied)),
        };
        let (entry, zc_pages, copied_pages) = match loaded {
            Ok(facts) => facts,
            Err(err) => return Err(self.unwind_spawn_image(asid, aspace_cap, "load", err)),
        };

        // ── 4. NR 23's initrd window. Tolerated on failure, unchanged. ──────────────────
        if map_initrd_window {
            self.map_boot_initrd_window_into(asid, startup_args);
        }

        // ── 5. The child's user stack, in the address space it belongs to. ──────────────
        let stack_top = match self.allocate_user_stack_in_asid(asid, tid, stack_pages) {
            Ok(top) => top,
            Err(err) => return Err(self.unwind_spawn_image(asid, aspace_cap, "stack", err)),
        };

        crate::yarm_log!(
            "SPAWN_IMAGE_PROVISIONED tid={} path={} asid={} entry=0x{:x} stack_top=0x{:x} \
             first_vaddr=0x{:x} end=0x{:x} zc_pages={} copied_pages={}",
            tid,
            image_path,
            asid.0,
            entry,
            stack_top.0,
            plan.first_vaddr,
            plan.last_vaddr_end,
            zc_pages,
            copied_pages
        );
        Ok(ImageProvision {
            asid,
            aspace_cap,
            entry,
            stack_top,
            zc_pages,
            copied_pages,
        })
    }

    /// The ONE rollback, in exact reverse acquisition order.
    ///
    /// Phase 2 acquired two things in the order (address space, capability naming it), so the
    /// unwind releases (capability, address space):
    ///
    /// 1. **the address-space capability** — through `revoke_capability_in_cnode`, the owner of
    ///    cspace entries. It lives in the CALLER's cspace, not in the child's, so the teardown
    ///    below does not and cannot reach it. U9-SPAWN1 SP-3 found this leak on every arm.
    /// 2. **the address space** — through `destroy_user_address_space_by_asid`, the repaired
    ///    teardown owner (U9-ASPACE1 §1). One call drains every mapping installed in the ASID —
    ///    ELF segments, zero-copy grants, the initrd window, the stack and its guard page —
    ///    reclaims MemoryObject-backed frames through the backing-aware lifecycle, returns plain
    ///    user backing to the allocator, frees the page-table pages and retires the ASID.
    ///
    /// No TLB shootdown is owed: see this module's header. The ASID was never bound to a TCB, so
    /// `live_cpu_bitmap_for_asid` is empty by construction.
    ///
    /// Inert on repetition and on stale input: revoking an absent capability returns
    /// `InvalidCapability` and destroying an already-destroyed ASID returns an error; both are
    /// logged and neither mutates. Returns the error it was called with so a caller can
    /// `return Err(self.unwind_spawn_image(..))` in one line and cannot forget to propagate it.
    fn unwind_spawn_image(
        &mut self,
        asid: Asid,
        aspace_cap: CapId,
        phase: &'static str,
        err: KernelError,
    ) -> KernelError {
        let revoked = match self.current_task_cnode() {
            Some(cnode) => self.revoke_capability_in_cnode(cnode, aspace_cap).is_ok(),
            None => false,
        };
        let destroyed = self.destroy_user_address_space_by_asid(asid).is_ok();
        crate::yarm_log!(
            "SPAWN_IMAGE_UNWOUND asid={} phase={} cap={} revoked={} destroyed={} err={:?}",
            asid.0,
            phase,
            aspace_cap.0,
            u8::from(revoked),
            u8::from(destroyed),
            err
        );
        err
    }

    /// Map the boot initrd read-only into `asid` and publish it in startup slots 15/16.
    ///
    /// NR 23's `initramfs_srv` only. Failure is TOLERATED, exactly as it was before this moved
    /// here from the spawn transaction: the server falls back to its syscall bridge when the
    /// window is absent, so a mapping failure must not fail the spawn. Pages installed before a
    /// failure belong to `asid`, which the rollback above destroys wholesale — a partial window
    /// is not a leak.
    fn map_boot_initrd_window_into(&mut self, asid: Asid, startup_args: &mut [u64; 18]) {
        const INITRD_USER_VA_BASE: u64 = 0x0C00_0000;
        let Some(initrd) = crate::kernel::boot::Bootstrap::boot_initrd_bytes() else {
            crate::yarm_log!("INITRAMFS_INITRD_MAP_SKIP reason=no_boot_initrd");
            return;
        };
        let initrd_virt_raw = initrd.as_ptr() as u64;
        let virt_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE;
        let phys_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_PHYS_BASE;
        let initrd_phys_raw = if virt_base > phys_base && initrd_virt_raw >= virt_base {
            match initrd_virt_raw
                .checked_sub(virt_base)
                .and_then(|off| phys_base.checked_add(off))
            {
                Some(phys) => phys,
                None => {
                    crate::yarm_log!(
                        "INITRAMFS_INITRD_ADDR_INVALID raw_ptr=0x{:x} virt_base=0x{:x} phys_base=0x{:x}",
                        initrd_virt_raw,
                        virt_base,
                        phys_base
                    );
                    return;
                }
            }
        } else if initrd_virt_raw < virt_base || virt_base == phys_base {
            initrd_virt_raw
        } else {
            crate::yarm_log!(
                "INITRAMFS_INITRD_ADDR_INVALID raw_ptr=0x{:x} virt_base=0x{:x} phys_base=0x{:x}",
                initrd_virt_raw,
                virt_base,
                phys_base
            );
            return;
        };
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
        let initrd_offset_in_first_page = initrd_phys_raw - phys_start;
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
        for i in 0..pages_to_map {
            let virt = VirtAddr(INITRD_USER_VA_BASE + (i as u64) * page);
            let phys = PhysAddr(phys_start + (i as u64) * page);
            if let Err(e) = self.map_user_page_in_asid_raw(
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
                return;
            }
        }
        let user_initrd_ptr = INITRD_USER_VA_BASE + initrd_offset_in_first_page;
        startup_args[15] = user_initrd_ptr;
        startup_args[16] = initrd_len;
        crate::yarm_log!(
            "INITRAMFS_INITRD_MAP_DONE user_ptr=0x{:x} len={} rights=ro",
            user_initrd_ptr,
            initrd_len
        );
    }
}
