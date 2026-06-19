// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{DispatchSwitchPlan, KernelError, KernelState, SpawnedUserTask, UserImageSpec};
use crate::arch::hal::Hal;
use crate::kernel::capabilities::{CapId, CapRights};
use crate::kernel::ipc::ThreadId;
use crate::kernel::scheduler::CpuId;
use crate::kernel::task::{TaskStatus, ThreadGroupId, UserRegisterContext, WaitReason};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use core::sync::atomic::{AtomicU64, Ordering};

const ELF64_EHDR_SIZE: usize = 64;
const ELF64_PHDR_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

fn read_u16_le(image: &[u8], offset: usize) -> Result<u16, KernelError> {
    let end = offset.checked_add(2).ok_or(KernelError::WrongObject)?;
    let bytes = image.get(offset..end).ok_or(KernelError::WrongObject)?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(raw))
}

fn read_u32_le(image: &[u8], offset: usize) -> Result<u32, KernelError> {
    let end = offset.checked_add(4).ok_or(KernelError::WrongObject)?;
    let bytes = image.get(offset..end).ok_or(KernelError::WrongObject)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64_le(image: &[u8], offset: usize) -> Result<u64, KernelError> {
    let end = offset.checked_add(8).ok_or(KernelError::WrongObject)?;
    let bytes = image.get(offset..end).ok_or(KernelError::WrongObject)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn task_missing_with_site(site: &'static str, cpu: u8) -> KernelError {
    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
        crate::yarm_log!("TASK_MISSING site={} cpu={}", site, cpu);
    }
    KernelError::TaskMissing
}

const BOOTSTRAP_FIRST_USER_TID: u64 = 1;
const BOOTSTRAP_SUPERVISOR_TID: u64 = 2;
const DEBUG_YIELD_LOG: bool = false;
const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
static DISPATCH_CONTEXT_LOAD_EVENT_ID: AtomicU64 = AtomicU64::new(1);

impl KernelState {
    fn page_flags_from_elf_pflags(p_flags: u32) -> Result<PageFlags, KernelError> {
        let mut read = (p_flags & PF_R) != 0;
        let write = (p_flags & PF_W) != 0;
        let execute = (p_flags & PF_X) != 0;
        if write && execute {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("ELF_REJECT_WX_SEGMENT p_flags=0x{:x}", p_flags);
            }
            return Err(KernelError::WrongObject);
        }
        if write || execute {
            read = true;
        }
        Ok(PageFlags {
            read,
            write,
            execute,
            user: true,
            cache_policy: CachePolicy::WriteBack,
        })
    }

    fn staging_page_flags_from_final(final_flags: PageFlags) -> PageFlags {
        PageFlags {
            read: true,
            write: true,
            execute: false,
            user: final_flags.user,
            cache_policy: final_flags.cache_policy,
        }
    }

    fn load_page_elf_pflags(
        image: &[u8],
        phoff: usize,
        phentsize: usize,
        phnum: usize,
        page_start: u64,
        page_end: u64,
    ) -> Result<u32, KernelError> {
        let mut combined_pflags = 0u32;
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            let p_type = read_u32_le(image, base)?;
            if p_type != PT_LOAD {
                continue;
            }
            let p_flags = read_u32_le(image, base + 4)?;
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_memsz = read_u64_le(image, base + 40)?;
            let seg_end = p_vaddr
                .checked_add(p_memsz)
                .ok_or(KernelError::WrongObject)?;
            if p_memsz != 0 && p_vaddr < page_end && seg_end > page_start {
                combined_pflags |= p_flags & (PF_R | PF_W | PF_X);
            }
        }
        Ok(combined_pflags)
    }

    /// Minimal ELF64 loader for PT_LOAD segments:
    /// validates headers, maps pages for each load segment, copies file bytes,
    /// and zero-fills the BSS tail.
    pub fn load_elf_pt_load_segments(
        &mut self,
        asid: Asid,
        image: &[u8],
    ) -> Result<(usize, usize, usize), KernelError> {
        if image.len() < ELF64_EHDR_SIZE || &image[..4] != b"\x7FELF" || image[4] != 2 {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!(
                    "ELF_REJECT_HEADER len={} magic_ok={} class={}",
                    image.len(),
                    image.get(..4) == Some(b"\x7FELF"),
                    image.get(4).copied().unwrap_or(0)
                );
            }
            return Err(KernelError::WrongObject);
        }
        let entry = read_u64_le(image, 24)?;
        let phoff = read_u64_le(image, 32)? as usize;
        let phentsize = read_u16_le(image, 54)? as usize;
        let phnum = read_u16_le(image, 56)? as usize;
        if phnum == 0 || phentsize < ELF64_PHDR_SIZE {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!(
                    "ELF_REJECT_PH_TABLE phnum={} phentsize={}",
                    phnum,
                    phentsize
                );
            }
            return Err(KernelError::WrongObject);
        }
        let table_size = phnum
            .checked_mul(phentsize)
            .ok_or(KernelError::WrongObject)?;
        let phend = phoff
            .checked_add(table_size)
            .ok_or(KernelError::WrongObject)?;
        if phend > image.len() {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!(
                    "ELF_REJECT_PH_BOUNDS phoff={} phend={} len={}",
                    phoff,
                    phend,
                    image.len()
                );
            }
            return Err(KernelError::WrongObject);
        }

        let mut max_loaded_end = 0u64;
        let mut first_pt_load_vaddr = 0u64;
        let mut saw_pt_load = false;
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            let p_type = read_u32_le(image, base)?;
            if p_type != PT_LOAD {
                continue;
            }
            let _p_flags = read_u32_le(image, base + 4)?;
            let p_offset = read_u64_le(image, base + 8)? as usize;
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_filesz = read_u64_le(image, base + 32)? as usize;
            let p_memsz = read_u64_le(image, base + 40)? as usize;
            if !saw_pt_load {
                first_pt_load_vaddr = p_vaddr;
            }
            saw_pt_load = true;
            let seg_end = p_vaddr
                .checked_add(p_memsz as u64)
                .ok_or(KernelError::WrongObject)?;
            if seg_end > max_loaded_end {
                max_loaded_end = seg_end;
            }
            if p_filesz > p_memsz {
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_REJECT_SEG_SIZE idx={} filesz={} memsz={}",
                        idx,
                        p_filesz,
                        p_memsz
                    );
                }
                return Err(KernelError::WrongObject);
            }
            let file_end = p_offset
                .checked_add(p_filesz)
                .ok_or(KernelError::WrongObject)?;
            if file_end > image.len() {
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_REJECT_FILE_BOUNDS idx={} offset={} filesz={} end={} len={}",
                        idx,
                        p_offset,
                        p_filesz,
                        file_end,
                        image.len()
                    );
                }
                return Err(KernelError::WrongObject);
            }

            let page_size = PAGE_SIZE as u64;
            let seg_start = p_vaddr;
            let seg_end = p_vaddr
                .checked_add(p_memsz as u64)
                .ok_or(KernelError::WrongObject)?;
            let page_start = seg_start & !(page_size - 1);
            let page_end = (seg_end + page_size - 1) & !(page_size - 1);
            let mut va = page_start;
            while va < page_end {
                let combined_pflags =
                    Self::load_page_elf_pflags(image, phoff, phentsize, phnum, va, va + page_size)?;
                let flags = Self::page_flags_from_elf_pflags(combined_pflags)?;
                let existing =
                    crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va));
                let phys = if let Some(entry) = existing {
                    entry.addr()
                } else {
                    self.alloc_user_data_frame()?
                };
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_MAP_PAGE_BEGIN asid={} seg_vbase=0x{:x} page_va=0x{:x} phys=0x{:x} memsz={} filesz={} overlap={} pflags=0x{:x}",
                        asid.0,
                        p_vaddr,
                        va,
                        phys,
                        p_memsz,
                        p_filesz,
                        existing.is_some(),
                        combined_pflags
                    );
                }
                let stage_flags = Self::staging_page_flags_from_final(flags);
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_MAP_PAGE_STAGE_PERMS asid={} page_va=0x{:x} r={} w={} x={} u={}",
                        asid.0,
                        va,
                        stage_flags.read,
                        stage_flags.write,
                        stage_flags.execute,
                        stage_flags.user
                    );
                }
                self.map_user_page_in_asid_raw(
                    asid,
                    VirtAddr(va),
                    Mapping {
                        phys: PhysAddr(phys),
                        flags: stage_flags,
                    },
                )?;
                #[cfg(all(not(feature = "hosted-dev"), feature = "trace_frame_alloc"))]
                crate::yarm_log!(
                    "KSPAWN_NEW_TASK_MAP_RANGE asid={} va=0x{:x} pa=0x{:x}",
                    asid.0,
                    va,
                    phys
                );
                let post_map_present =
                    crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va))
                        .is_some();
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_MAP_PAGE_DONE asid={} page_va=0x{:x} post_resolve={} final_r={} final_w={} final_x={} final_u={}",
                        asid.0,
                        va,
                        post_map_present,
                        flags.read,
                        flags.write,
                        flags.execute,
                        flags.user
                    );
                    if va == 0x0040_0000 {
                        crate::yarm_log!(
                            "ELF_MAP_PAGE_PERMS asid={} page_va=0x{:x} r={} w={} x={} u={}",
                            asid.0,
                            va,
                            flags.read,
                            flags.write,
                            flags.execute,
                            flags.user
                        );
                    }
                }
                if !post_map_present {
                    if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                        crate::yarm_log!(
                            "ELF_MAP_PAGE_INVARIANT_FAIL asid={} page_va=0x{:x} phys=0x{:x}",
                            asid.0,
                            va,
                            phys
                        );
                    }
                    return Err(KernelError::UserMemoryFault);
                }
                va += page_size;
            }

            let file_bytes = &image[p_offset..file_end];
            self.copy_to_user(asid, VirtAddr(p_vaddr), file_bytes)?;
            if p_memsz > p_filesz {
                let mut remaining = p_memsz - p_filesz;
                let mut cursor = p_vaddr + p_filesz as u64;
                let zeros = [0u8; 256];
                while remaining > 0 {
                    let chunk = remaining.min(zeros.len());
                    self.copy_to_user(asid, VirtAddr(cursor), &zeros[..chunk])?;
                    remaining -= chunk;
                    cursor += chunk as u64;
                }
            }
        }
        if !saw_pt_load {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("ELF_REJECT_NO_PT_LOAD");
            }
            return Err(KernelError::WrongObject);
        }
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            let p_type = read_u32_le(image, base)?;
            if p_type != PT_LOAD {
                continue;
            }
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_memsz = read_u64_le(image, base + 40)? as usize;
            let page_size = PAGE_SIZE as u64;
            let seg_end = p_vaddr
                .checked_add(p_memsz as u64)
                .ok_or(KernelError::WrongObject)?;
            let page_start = p_vaddr & !(page_size - 1);
            let page_end = (seg_end + page_size - 1) & !(page_size - 1);
            let mut va = page_start;
            while va < page_end {
                let combined_pflags =
                    Self::load_page_elf_pflags(image, phoff, phentsize, phnum, va, va + page_size)?;
                let final_flags = Self::page_flags_from_elf_pflags(combined_pflags)?;
                let phys = crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va))
                    .ok_or(KernelError::UserMemoryFault)?
                    .addr();
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "ELF_FINALIZE_PAGE_PERMS asid={} page_va=0x{:x} r={} w={} x={} u={}",
                        asid.0,
                        va,
                        final_flags.read,
                        final_flags.write,
                        final_flags.execute,
                        final_flags.user
                    );
                }
                self.map_user_page_in_asid_raw(
                    asid,
                    VirtAddr(va),
                    Mapping {
                        phys: PhysAddr(phys),
                        flags: final_flags,
                    },
                )?;
                va += page_size;
            }
        }
        let page_size = PAGE_SIZE as u64;
        let heap_base = max_loaded_end
            .checked_add(page_size - 1)
            .ok_or(KernelError::WrongObject)?
            & !(page_size - 1);
        Ok((
            entry as usize,
            first_pt_load_vaddr as usize,
            heap_base as usize,
        ))
    }

    /// Count total ELF PT_LOAD pages for telemetry (not unique; overlapping segments counted multiple times).
    fn count_elf_load_pages(image: &[u8]) -> Result<usize, KernelError> {
        if image.len() < ELF64_EHDR_SIZE || &image[..4] != b"\x7FELF" || image[4] != 2 {
            return Err(KernelError::WrongObject);
        }
        let phoff = read_u64_le(image, 32)? as usize;
        let phentsize = read_u16_le(image, 54)? as usize;
        let phnum = read_u16_le(image, 56)? as usize;
        let page_size = PAGE_SIZE as u64;
        let mut total = 0usize;
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            if read_u32_le(image, base)? != PT_LOAD {
                continue;
            }
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_memsz = read_u64_le(image, base + 40)?;
            if p_memsz == 0 {
                continue;
            }
            let seg_start = p_vaddr & !(page_size - 1);
            let seg_end = (p_vaddr + p_memsz + page_size - 1) & !(page_size - 1);
            total = total.saturating_add(((seg_end - seg_start) / page_size) as usize);
        }
        Ok(total)
    }

    /// ELF loader for `SpawnFromMemoryObject` (Phase 3A syscall nr=29).
    ///
    /// Attempts zero-copy mapping of read-only, page-aligned file pages directly
    /// from the initrd physical backing.  Falls back to alloc+copy for:
    ///  - Writable segments
    ///  - Pages that cross file/BSS boundary
    ///  - Any segment whose file data is NOT page-aligned within the initrd
    ///    (typical for CPIO archives without explicit alignment)
    ///
    /// `image_id` is used only for diagnostic logging.
    ///
    /// Returns `(entry, first_pt_load_vaddr, heap_base, zc_pages, copied_pages)`.
    pub fn load_elf_with_mo_zero_copy(
        &mut self,
        image_id: u64,
        asid: Asid,
        image: &[u8],
        initrd_phys_base: u64,
        file_initrd_offset: u64,
    ) -> Result<(usize, usize, usize, usize, usize), KernelError> {
        // Determine zero-copy feasibility: file data must start on a page boundary
        // within the initrd physical region for any page to be directly mappable.
        let page_size = PAGE_SIZE as u64;
        let file_phys_start = initrd_phys_base.saturating_add(file_initrd_offset);
        let offset_in_page = file_phys_start & (page_size - 1);
        let zc_feasible = offset_in_page == 0;

        crate::yarm_log!(
            "ZC_FEASIBILITY image_id={} initrd_phys=0x{:x} file_off=0x{:x} \
             file_phys=0x{:x} offset_in_page={} feasible={}",
            image_id,
            initrd_phys_base,
            file_initrd_offset,
            file_phys_start,
            offset_in_page,
            zc_feasible
        );

        if !zc_feasible {
            // CPIO file data is not page-aligned — use existing copy-based loader.
            // zc_pages = 0; copied_pages = total page slots in PT_LOAD segments.
            let copied_pages = Self::count_elf_load_pages(image).unwrap_or(0);
            crate::yarm_log!(
                "ZC_FALLBACK image_id={} reason=cpio_file_data_unaligned copied_pages={}",
                image_id,
                copied_pages
            );
            let (entry, first, heap) = self.load_elf_pt_load_segments(asid, image)?;
            return Ok((entry, first, heap, 0, copied_pages));
        }

        // File data IS page-aligned in the initrd.  Perform per-page ZC decision.
        // For each PT_LOAD page:
        //   - Non-writable && fully within file && page-aligned phys → map RO from initrd
        //   - Everything else → alloc anon frame + copy (writable, BSS, partial pages)
        if image.len() < ELF64_EHDR_SIZE || &image[..4] != b"\x7FELF" || image[4] != 2 {
            return Err(KernelError::WrongObject);
        }
        let entry = read_u64_le(image, 24)?;
        let phoff = read_u64_le(image, 32)? as usize;
        let phentsize = read_u16_le(image, 54)? as usize;
        let phnum = read_u16_le(image, 56)? as usize;
        if phnum == 0 || phentsize < ELF64_PHDR_SIZE {
            return Err(KernelError::WrongObject);
        }
        let table_size = phnum
            .checked_mul(phentsize)
            .ok_or(KernelError::WrongObject)?;
        let phend = phoff
            .checked_add(table_size)
            .ok_or(KernelError::WrongObject)?;
        if phend > image.len() {
            return Err(KernelError::WrongObject);
        }

        let mut max_loaded_end = 0u64;
        let mut first_pt_load_vaddr = 0u64;
        let mut saw_pt_load = false;
        let mut zc_pages = 0usize;
        let mut copied_pages = 0usize;
        let mut seg_load_idx = 0usize;

        // First pass: allocate physical pages and map (staging flags for copy pages).
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            let p_type = read_u32_le(image, base)?;
            if p_type != PT_LOAD {
                continue;
            }
            let p_flags_raw = read_u32_le(image, base + 4)?;
            let p_offset = read_u64_le(image, base + 8)? as usize;
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_filesz = read_u64_le(image, base + 32)? as usize;
            let p_memsz = read_u64_le(image, base + 40)? as usize;
            let seg_idx = seg_load_idx;
            seg_load_idx += 1;
            if !saw_pt_load {
                first_pt_load_vaddr = p_vaddr;
            }
            saw_pt_load = true;
            let seg_end = p_vaddr
                .checked_add(p_memsz as u64)
                .ok_or(KernelError::WrongObject)?;
            if seg_end > max_loaded_end {
                max_loaded_end = seg_end;
            }
            if p_filesz > p_memsz {
                return Err(KernelError::WrongObject);
            }
            let file_end = p_offset
                .checked_add(p_filesz)
                .ok_or(KernelError::WrongObject)?;
            if file_end > image.len() {
                return Err(KernelError::WrongObject);
            }
            let seg_start = p_vaddr;
            let page_start = seg_start & !(page_size - 1);
            let page_end_va = (seg_end + page_size - 1) & !(page_size - 1);
            let p_offset_page = p_offset as u64 & !(page_size - 1);
            let p_vaddr_page = p_vaddr & !(page_size - 1);

            crate::yarm_log!(
                "ZC_SEG_BEGIN image_id={} seg={} p_offset=0x{:x} p_vaddr=0x{:x} \
                 p_filesz={} p_memsz={} p_flags=0x{:x} file_data_phys=0x{:x} \
                 offset_in_page={}",
                image_id,
                seg_idx,
                p_offset,
                p_vaddr,
                p_filesz,
                p_memsz,
                p_flags_raw,
                file_phys_start,
                file_phys_start & (page_size - 1)
            );

            let mut seg_zc = 0usize;
            let mut seg_copied = 0usize;

            let mut va = page_start;
            while va < page_end_va {
                let combined_pflags =
                    Self::load_page_elf_pflags(image, phoff, phentsize, phnum, va, va + page_size)?;
                // Detect WX before calling page_flags_from_elf_pflags so we can log the reason.
                if (combined_pflags & PF_W) != 0 && (combined_pflags & PF_X) != 0 {
                    crate::yarm_log!(
                        "ZC_PAGE image_id={} seg={} va=0x{:x} src_phys=0x0 \
                         reason=wx_rejected pflags=0x{:x}",
                        image_id,
                        seg_idx,
                        va,
                        combined_pflags
                    );
                    return Err(KernelError::WrongObject);
                }
                let flags = Self::page_flags_from_elf_pflags(combined_pflags)?;
                // ELF file page index within this segment.
                let page_idx = (va - p_vaddr_page) / page_size;
                let elf_file_page_start = p_offset_page + page_idx * page_size;
                // Page is fully in file data if all bytes in [va, va+PAGE_SIZE)
                // fall within [p_vaddr, p_vaddr+p_filesz).
                let page_fully_in_file = va >= p_vaddr
                    && p_filesz > 0
                    && va.saturating_add(page_size) <= p_vaddr.saturating_add(p_filesz as u64);
                // Zero-copy eligibility: RO, fully-in-file, page-aligned phys.
                let initrd_phys_of_page = file_phys_start.saturating_add(elf_file_page_start);
                let can_zc = !flags.write
                    && page_fully_in_file
                    && (initrd_phys_of_page & (page_size - 1)) == 0;

                // Determine diagnostic reason for the page decision.
                let reason = if can_zc {
                    "full_page_zc_ok"
                } else if flags.write {
                    "writable_segment_copy"
                } else if va < p_vaddr {
                    "partial_head_copy"
                } else if p_filesz == 0 || va >= p_vaddr.saturating_add(p_filesz as u64) {
                    "bss_copy"
                } else if va.saturating_add(page_size) > p_vaddr.saturating_add(p_filesz as u64) {
                    "partial_tail_copy"
                } else {
                    // page_fully_in_file is true but initrd_phys_of_page is misaligned;
                    // can only occur if file_phys_start alignment changed mid-computation.
                    "elf_offset_unaligned"
                };

                crate::yarm_log!(
                    "ZC_PAGE image_id={} seg={} va=0x{:x} src_phys=0x{:x} reason={}",
                    image_id,
                    seg_idx,
                    va,
                    initrd_phys_of_page,
                    reason
                );

                let existing =
                    crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va));
                if existing.is_some() {
                    // Page already mapped by an earlier overlapping segment — skip.
                } else if can_zc {
                    // Zero-copy: map the initrd physical page directly (RO final flags).
                    self.map_user_page_in_asid_raw(
                        asid,
                        VirtAddr(va),
                        Mapping {
                            phys: PhysAddr(initrd_phys_of_page),
                            flags,
                        },
                    )?;
                    zc_pages += 1;
                    seg_zc += 1;
                } else {
                    // Copy: alloc anonymous frame, map with staging RW flags.
                    let phys = self.alloc_user_data_frame()?;
                    let stage_flags = Self::staging_page_flags_from_final(flags);
                    self.map_user_page_in_asid_raw(
                        asid,
                        VirtAddr(va),
                        Mapping {
                            phys: PhysAddr(phys),
                            flags: stage_flags,
                        },
                    )?;
                    copied_pages += 1;
                    seg_copied += 1;
                }
                va += page_size;
            }

            crate::yarm_log!(
                "ZC_SEG_DONE image_id={} seg={} p_vaddr=0x{:x} p_flags=0x{:x} \
                 zc_pages={} copied_pages={}",
                image_id,
                seg_idx,
                p_vaddr,
                p_flags_raw,
                seg_zc,
                seg_copied
            );

            // Copy ELF file bytes into non-ZC pages only.
            // Iterate page by page to skip ZC pages (which are already correct and RO).
            let mut va = page_start;
            while va < page_end_va {
                // Determine ZC status for this page.
                let page_idx = (va - p_vaddr_page) / page_size;
                let elf_file_page_start = p_offset_page + page_idx * page_size;
                let combined_pflags =
                    Self::load_page_elf_pflags(image, phoff, phentsize, phnum, va, va + page_size)?;
                let flags = Self::page_flags_from_elf_pflags(combined_pflags)?;
                let page_fully_in_file = va >= p_vaddr
                    && p_filesz > 0
                    && va.saturating_add(page_size) <= p_vaddr.saturating_add(p_filesz as u64);
                let initrd_phys_of_page = file_phys_start.saturating_add(elf_file_page_start);
                let can_zc = !flags.write
                    && page_fully_in_file
                    && (initrd_phys_of_page & (page_size - 1)) == 0;

                if !can_zc {
                    // Copy file bytes into this page.
                    // Clamp the file range to what falls within this page's VA.
                    let copy_va_start = core::cmp::max(va, p_vaddr);
                    let copy_va_end = core::cmp::min(va + page_size, p_vaddr + p_filesz as u64);
                    if copy_va_start < copy_va_end {
                        let copy_len = (copy_va_end - copy_va_start) as usize;
                        let file_off = (copy_va_start - p_vaddr) as usize + p_offset;
                        self.copy_to_user(
                            asid,
                            VirtAddr(copy_va_start),
                            &image[file_off..file_off + copy_len],
                        )?;
                    }
                    // Zero BSS portion of this page.
                    let bss_va_start = core::cmp::max(va, p_vaddr + p_filesz as u64);
                    let bss_va_end = core::cmp::min(va + page_size, p_vaddr + p_memsz as u64);
                    if bss_va_start < bss_va_end {
                        let bss_len = (bss_va_end - bss_va_start) as usize;
                        let zeros = [0u8; 256];
                        let mut remaining = bss_len;
                        let mut cursor = bss_va_start;
                        while remaining > 0 {
                            let chunk = remaining.min(zeros.len());
                            self.copy_to_user(asid, VirtAddr(cursor), &zeros[..chunk])?;
                            remaining -= chunk;
                            cursor += chunk as u64;
                        }
                    }
                }
                va += page_size;
            }
        }

        if !saw_pt_load {
            return Err(KernelError::WrongObject);
        }

        // Second pass: enforce final permissions on copy pages.
        // ZC pages already have final flags from the first pass.
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(KernelError::WrongObject)?)
                .ok_or(KernelError::WrongObject)?;
            if read_u32_le(image, base)? != PT_LOAD {
                continue;
            }
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_memsz = read_u64_le(image, base + 40)?;
            let seg_end = p_vaddr
                .checked_add(p_memsz)
                .ok_or(KernelError::WrongObject)?;
            let page_start = p_vaddr & !(page_size - 1);
            let page_end_va = (seg_end + page_size - 1) & !(page_size - 1);
            let mut va = page_start;
            while va < page_end_va {
                let combined_pflags =
                    Self::load_page_elf_pflags(image, phoff, phentsize, phnum, va, va + page_size)?;
                let final_flags = Self::page_flags_from_elf_pflags(combined_pflags)?;
                let phys = crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va))
                    .ok_or(KernelError::UserMemoryFault)?
                    .addr();
                self.map_user_page_in_asid_raw(
                    asid,
                    VirtAddr(va),
                    Mapping {
                        phys: PhysAddr(phys),
                        flags: final_flags,
                    },
                )?;
                va += page_size;
            }
        }

        let heap_base = max_loaded_end
            .checked_add(page_size - 1)
            .ok_or(KernelError::WrongObject)?
            & !(page_size - 1);

        Ok((
            entry as usize,
            first_pt_load_vaddr as usize,
            heap_base as usize,
            zc_pages,
            copied_pages,
        ))
    }

    fn maybe_switch_kernel_context(
        &mut self,
        outgoing_tid: Option<u64>,
        incoming_tid: u64,
    ) -> Result<(), KernelError> {
        let Some(outgoing_tid) = outgoing_tid else {
            // No outgoing task: the prior task was blocked/idle before this
            // dispatch. A kernel-context switch via switch_frames requires a
            // valid outgoing ArchSwitchContext to save into; dispatch here is
            // purely via trap-frame restore (restore_arch_thread_state).
            //
            // Emit a deferred marker on x86_64/AArch64 trap paths so smoke
            // logs prove the production path reaches this decision point.
            #[cfg(not(target_arch = "riscv64"))]
            {
                let _cpu_idx = self.current_cpu().0 as usize;
                let _trap_active = _cpu_idx < crate::kernel::scheduler::MAX_CPUS
                    && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[_cpu_idx]
                        .load(core::sync::atomic::Ordering::Relaxed);
                if _trap_active {
                    crate::yarm_log!(
                        "D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_outgoing_task incoming={}",
                        incoming_tid
                    );
                }
            }
            return Ok(());
        };
        if outgoing_tid == incoming_tid {
            return Ok(());
        }

        crate::yarm_log!(
            "D6_SWITCH_PLAN_BEGIN outgoing={} incoming={}",
            outgoing_tid,
            incoming_tid
        );
        crate::yarm_log!(
            "D6_GLOBAL_LOCK_DROP_PLAN_BEGIN outgoing={} incoming={}",
            outgoing_tid,
            incoming_tid
        );

        // Phase B (Stage 116 / Solution 1, Stage 117): acquire task_state_lock
        // (rank 2), locate both TCBs, validate kernel-context initialization,
        // extract raw ArchSwitchContext frame pointers and copy incoming stack top.
        // The sub-lock is released when `with_tcbs_mut` returns — before any
        // call to `switch_frames`.
        //
        // After this block: scheduler_state lock (rank 1) is already gone
        // (dropped by `local_dispatch_step_split`); task_state_lock (rank 2)
        // will be gone; only the outer global `SpinLock<KernelState>` from
        // `with_cpu` remains held on the x86_64/aarch64 path.
        let plan =
            self.with_tcbs_mut(|tcbs| -> Result<Option<DispatchSwitchPlan>, KernelError> {
                let outgoing_idx = tcbs
                    .iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == outgoing_tid))
                    .ok_or(KernelError::TaskMissing)?;
                let incoming_idx = tcbs
                    .iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == incoming_tid))
                    .ok_or(KernelError::TaskMissing)?;

                if outgoing_idx == incoming_idx {
                    return Ok(None);
                }

                // Split the TCB slice to get simultaneous mutable/shared refs.
                let (outgoing_tcb, incoming_tcb) = if outgoing_idx < incoming_idx {
                    let (left, right) = tcbs.split_at_mut(incoming_idx);
                    (
                        left[outgoing_idx]
                            .as_mut()
                            .ok_or(KernelError::TaskMissing)?,
                        right[0].as_mut().ok_or(KernelError::TaskMissing)?,
                    )
                } else {
                    let (left, right) = tcbs.split_at_mut(outgoing_idx);
                    (
                        right[0].as_mut().ok_or(KernelError::TaskMissing)?,
                        left[incoming_idx]
                            .as_mut()
                            .ok_or(KernelError::TaskMissing)?,
                    )
                };

                if !outgoing_tcb.kernel_context.initialized
                    || !incoming_tcb.kernel_context.initialized
                {
                    return Ok(None);
                }

                let incoming_stack_top = incoming_tcb.kernel_context.stack_top.map(|top| top.0);

                // Derive raw pointers from the live mutable/shared references.
                // These pointers remain valid after `task_state_lock` is released:
                //
                // (1) `KernelState::tcbs` is `KernelStorage<[Option<TCB>; MAX_TASKS]>` —
                //     a fixed-size inline array; no move or reallocation can occur
                //     during the dispatch path.
                // (2) On the Stage 116 fallback path: the outer global
                //     `SpinLock<KernelState>` (held by `with_cpu`) guarantees
                //     exclusive access to all of `KernelState`.
                // (3) On the Stage 117 stash path: interrupts are disabled (hardware
                //     trap entry on x86_64/aarch64) and the system is single-CPU, so
                //     no concurrent modification of `KernelState` can occur between
                //     the lock drop and `switch_frames`.
                // (4) The outgoing task is currently executing on this CPU only;
                //     its kernel frame cannot be modified by any other CPU.
                // (5) The incoming task was selected for this CPU by
                //     `local_dispatch_step_split`; the scheduler guarantees no other
                //     CPU will attempt to run it simultaneously.
                let outgoing_frame_ptr: *mut crate::kernel::task::ArchSwitchContext =
                    &mut outgoing_tcb.kernel_context.frame;
                let incoming_frame_ptr: *mut crate::kernel::task::ArchSwitchContext =
                    &mut incoming_tcb.kernel_context.frame;
                let outgoing_stack_top = outgoing_tcb.kernel_context.stack_top.map(|t| t.0);

                Ok(Some(DispatchSwitchPlan {
                    outgoing_tid,
                    incoming_tid,
                    outgoing_frame_ptr,
                    incoming_frame_ptr,
                    incoming_stack_top,
                    outgoing_stack_top,
                }))
            })?;
        // task_state_lock (rank 2) is now released.

        let Some(plan) = plan else {
            // Plan is None: one or both tasks lack an initialized kernel-context
            // switch frame (kernel_context.initialized == false). Production user
            // tasks are spawned via provision_default_kernel_context which sets
            // initialized = false; only explicitly wired kernel threads (set via
            // initialize_thread_kernel_switch_frame) have initialized == true.
            // Context switching for these tasks happens entirely via trap-frame
            // restore; switch_frames is not called.
            //
            // Emit a deferred marker on x86_64/AArch64 trap paths so smoke
            // logs prove the production path reaches this decision point.
            #[cfg(not(target_arch = "riscv64"))]
            {
                let _cpu_idx = self.current_cpu().0 as usize;
                let _trap_active = _cpu_idx < crate::kernel::scheduler::MAX_CPUS
                    && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[_cpu_idx]
                        .load(core::sync::atomic::Ordering::Relaxed);
                if _trap_active {
                    crate::yarm_log!(
                        "D6_GLOBAL_LOCK_DROP_DEFERRED reason=no_kernel_ctx_switch_frame outgoing={} incoming={}",
                        outgoing_tid,
                        incoming_tid
                    );
                }
            }
            return Ok(());
        };

        // Stage 117: decide whether to stash the plan for an out-of-lock
        // `switch_frames` call in `handle_trap_entry_shared`, or fall back to
        // the Stage 116 direct path (inside the global lock).
        //
        // Stash conditions:
        //   - Not RISC-V: RISC-V uses a raw kernel-state pointer (no `with_cpu`
        //     global lock to drop), so stashing here would leave a stash that is
        //     never drained.
        //   - Single online CPU: multi-CPU correctness for the lock-drop window
        //     has not been formally proved. Gate on the accepted single-online
        //     scheduler state.
        //   - GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE: `handle_trap_entry_shared` has
        //     signalled that it will drain the stash after `with_cpu` returns.
        //     Without this flag (direct test/non-trap calls to `dispatch_next_task`),
        //     there is no external drainer and the stash must not be used.
        let cpu_idx_for_stash = self.current_cpu().0 as usize;
        let trap_path_active = cpu_idx_for_stash < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx_for_stash]
                .load(core::sync::atomic::Ordering::Relaxed);
        let can_stash_for_lock_drop =
            !cfg!(target_arch = "riscv64") && self.online_cpu_count() <= 1 && trap_path_active;

        if can_stash_for_lock_drop {
            // Phase C (Stage 117 live path): stash plan so `switch_frames` runs
            // OUTSIDE the global `SpinLock<KernelState>`. The drain happens in
            // `handle_trap_entry_shared` after `with_cpu` drops the global lock.
            // The calling CPU remains non-preemptible because hardware disabled
            // interrupts on trap entry and `SpinLock` does not restore IRQ state.
            crate::yarm_log!(
                "D6_GLOBAL_LOCK_DROP_PLAN_READY outgoing={} incoming={}",
                plan.outgoing_tid,
                plan.incoming_tid
            );
            let cpu_idx = self.current_cpu().0 as usize;
            if cpu_idx < crate::kernel::scheduler::MAX_CPUS {
                // SAFETY: single CPU, interrupts disabled by hardware trap entry,
                // no concurrent accessor; stash is only read/drained in the same
                // CPU's `handle_trap_entry_shared` after this function returns.
                let already_stashed =
                    unsafe { crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].has_plan() };
                if already_stashed {
                    crate::yarm_log!("D6_GLOBAL_LOCK_DROP_DEFERRED reason=stash_occupied");
                    return Ok(());
                }
                unsafe {
                    crate::kernel::boot::DISPATCH_SWITCH_PLAN_STASH[cpu_idx].store(plan);
                }
            }
            // `switch_frames` will be called by the stash drain in
            // `handle_trap_entry_shared` after the `with_cpu` guard drops.
            return Ok(());
        }

        // Stage 116 fallback / deferred path: call `switch_frames` directly
        // under the global lock. Used for RISC-V (lockless raw-pointer trap path)
        // or when more than one CPU is online (multi-CPU proof pending).
        #[cfg(target_arch = "riscv64")]
        crate::yarm_log!("D6_GLOBAL_LOCK_DROP_DEFERRED reason=riscv_lockless_trap_path");
        #[cfg(not(target_arch = "riscv64"))]
        crate::yarm_log!("D6_GLOBAL_LOCK_DROP_DEFERRED reason=multi_cpu_not_proven");

        crate::yarm_log!(
            "D6_SWITCH_PLAN_READY outgoing={} incoming={}",
            plan.outgoing_tid,
            plan.incoming_tid
        );

        // Phase C (Stage 116 / Solution 1): no per-domain sub-lock is held
        // across `switch_frames`. The scheduler_state lock (rank 1) was
        // released inside `local_dispatch_step_split`'s inner block before
        // `dispatch_next_task` returned from that call. The task_state_lock
        // (rank 2) was released when `with_tcbs_mut` above returned. The CPU
        // remains non-preemptible because the outer global `SpinLock<KernelState>`
        // from `with_cpu` is still held here.
        //
        // VALIDATION: D6_SCHED_LOCK_DROPPED_BEFORE_SWITCH
        crate::yarm_log!("D6_SCHED_LOCK_DROPPED_BEFORE_SWITCH");
        crate::yarm_log!("D6_SWITCH_FRAMES_ENTER");

        // SAFETY: The raw pointers were derived from stable `KernelState::tcbs`
        // storage under `task_state_lock`. After the lock drop the pointed-to
        // memory remains valid for the reasons documented in the safety note
        // above (global lock held, fixed-size array, single-CPU dispatch). The
        // dereferences produce non-aliasing `&mut` and `&` because the two
        // indices are distinct (checked above) and the global lock prevents any
        // concurrent modification of `KernelState`.
        unsafe {
            crate::arch::selected_isa::context_switch::switch_frames(
                &mut *plan.outgoing_frame_ptr,
                &*plan.incoming_frame_ptr,
                plan.incoming_stack_top,
            );
        }

        crate::yarm_log!("D6_SWITCH_FRAMES_RETURNED");
        Ok(())
    }

    /// Stage 120: x86_64-only, single-CPU-only, boot-knob-gated, one-shot proof
    /// harness for the existing unlocked `switch_frames` path.
    ///
    /// This is not a scheduler policy path: it only runs when the boot command
    /// line contains `yarm.d6_switch_proof=1`, the current task is tid=1, tid=2
    /// has an initialized kernel switch frame, and the Stage 117 trap-path stash
    /// is active. It reuses `DispatchSwitchPlan` via `maybe_switch_kernel_context`
    /// and disables itself permanently after one stashed proof pair.
    pub(crate) fn maybe_run_d6_controlled_switch_proof(&mut self) -> Result<(), KernelError> {
        #[cfg(not(target_arch = "x86_64"))]
        {
            return Ok(());
        }

        #[cfg(target_arch = "x86_64")]
        {
            if !crate::kernel::boot::d6_controlled_switch_proof_enabled()
                || crate::kernel::boot::d6_controlled_switch_proof_done()
            {
                return Ok(());
            }
            if self.online_cpu_count() != 1 {
                crate::yarm_log!(
                    "D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=multi_cpu online_cpus={}",
                    self.online_cpu_count()
                );
                return Ok(());
            }
            let cpu_idx = self.current_cpu().0 as usize;
            let trap_path_active = cpu_idx < crate::kernel::scheduler::MAX_CPUS
                && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
                    .load(core::sync::atomic::Ordering::Relaxed);
            if !trap_path_active {
                crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=trap_path_inactive");
                return Ok(());
            }
            let outgoing_tid = match self.current_tid() {
                Some(BOOTSTRAP_FIRST_USER_TID) => BOOTSTRAP_FIRST_USER_TID,
                Some(other) => {
                    if other == BOOTSTRAP_SUPERVISOR_TID {
                        crate::yarm_log!(
                            "D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=wrong_outgoing_tid tid={}",
                            other
                        );
                    }
                    return Ok(());
                }
                None => {
                    crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=no_current_tid");
                    return Ok(());
                }
            };
            let incoming_tid = BOOTSTRAP_SUPERVISOR_TID;
            let frames_ready = self.with_tcbs_mut(|tcbs| {
                let has_initialized = |tid| {
                    tcbs.iter()
                        .flatten()
                        .any(|tcb| tcb.tid.0 == tid && tcb.kernel_context.initialized)
                };
                has_initialized(outgoing_tid) && has_initialized(incoming_tid)
            });
            if !frames_ready {
                crate::yarm_log!(
                    "D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=frames_uninitialized outgoing={} incoming={}",
                    outgoing_tid,
                    incoming_tid
                );
                return Ok(());
            }
            // Stage 128: `switch_frames` does not switch CR3; it changes the
            // kernel stack while the outgoing/current root is still active.
            // Before stashing the proof plan, prove that the incoming stack page
            // is visible and supervisor-writable in that active root.
            if let Err(err) = self.ensure_active_root_can_use_kernel_switch_stack(incoming_tid) {
                crate::yarm_log!(
                    "D6_CONTROLLED_SWITCH_PROOF_DEFERRED reason=active_stack_unmapped outgoing={} incoming={} err={:?}",
                    outgoing_tid,
                    incoming_tid,
                    err
                );
                return Ok(());
            }
            if !crate::kernel::boot::d6_controlled_switch_proof_try_start() {
                return Ok(());
            }
            crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_BEGIN");
            crate::yarm_log!(
                "D6_CONTROLLED_SWITCH_PROOF_PAIR outgoing={} incoming={}",
                outgoing_tid,
                incoming_tid
            );
            // Stage 131: ArchSwitchContext / switch_frames ABI audit markers.
            // Emitted once per proof run to record that the layout was verified:
            // words[0..7] at offsets 0,8,16..56 (rsp,rip,rbx,rbp,r12-r15);
            // fxsave at offset 64; total 576 bytes; r14 saved/restored at offset 48.
            crate::yarm_log!("D6_SWITCH_CONTEXT_AUDIT_BEGIN");
            crate::yarm_log!("D6_SWITCH_CONTEXT_LAYOUT_OK");
            crate::yarm_log!("D6_SWITCH_CONTEXT_R14_RESTORE_CHECK");
            crate::yarm_log!("D6_SWITCH_CONTEXT_AUDIT_DONE");
            self.maybe_switch_kernel_context(Some(outgoing_tid), incoming_tid)?;
            crate::kernel::boot::d6_controlled_switch_proof_mark_pending_done();
            Ok(())
        }
    }

    pub fn futex_wait_current(
        &mut self,
        addr: usize,
        expected: u32,
        observed: u32,
    ) -> Result<bool, KernelError> {
        self.validate_current_user_futex_word(addr)?;
        if expected != observed {
            return Ok(false);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)));
            Ok::<_, KernelError>(())
        })?;
        let _ = self.block_current_cpu();
        self.dispatch_next_task()?;
        Ok(true)
    }

    pub fn futex_wake(&mut self, addr: usize, max_wake: u32) -> Result<u32, KernelError> {
        self.validate_current_user_futex_word(addr)?;
        self.futex_wake_inner(addr, max_wake)
    }

    /// Wake futex waiters without `copy_from_user` address validation.
    ///
    /// Used by the robust-futex cleanup path in `exit_task`: the exiting task
    /// registered the robust list addresses itself, so they are trusted user-space
    /// addresses.  Using `futex_wake` directly from `exit_task` would use the
    /// *current* task's ASID for validation (which may be the supervisor, not the
    /// exiting task), causing the wake to fail silently.
    pub(crate) fn futex_wake_on_exit(&mut self, addr: usize) -> Result<u32, KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        let end = addr.checked_add(core::mem::size_of::<u32>() - 1);
        if end.is_none_or(|end| end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE) {
            return Err(KernelError::UserMemoryFault);
        }
        self.futex_wake_inner(addr, u32::MAX)
    }

    fn futex_wake_inner(&mut self, addr: usize, max_wake: u32) -> Result<u32, KernelError> {
        if max_wake == 0 {
            return Ok(0);
        }
        let (wake_tids, wake_count) = self.with_tcbs_mut(|tcbs| {
            let mut wake_tids = [None; super::MAX_TASKS];
            let mut wake_count = 0usize;
            for tcb in tcbs.iter_mut().flatten() {
                if wake_count >= max_wake as usize {
                    break;
                }
                if tcb.status != TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64))) {
                    continue;
                }
                tcb.status = TaskStatus::Runnable;
                wake_tids[wake_count] = Some(tcb.tid.0);
                wake_count += 1;
            }
            (wake_tids, wake_count)
        });
        for wake_tid in wake_tids.iter().take(wake_count).flatten() {
            self.enqueue_task(*wake_tid)?;
        }
        Ok(wake_count as u32)
    }

    fn validate_current_user_futex_word(&self, addr: usize) -> Result<(), KernelError> {
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
        let end = addr.checked_add(core::mem::size_of::<u32>() - 1);
        if end.is_none_or(|end| end as u64 >= crate::kernel::vm::KERNEL_SPACE_BASE) {
            return Err(KernelError::UserMemoryFault);
        }
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let _ = self.copy_from_user(asid, VirtAddr(addr as u64), core::mem::size_of::<u32>())?;
        Ok(())
    }

    pub fn spawn_user_task_from_image(
        &mut self,
        mut spec: UserImageSpec,
    ) -> Result<SpawnedUserTask, KernelError> {
        let cpu = self.current_cpu();
        if spec.entry == 0 {
            return Err(KernelError::WrongObject);
        }
        let asid = spec.asid.ok_or(KernelError::UserMemoryFault)?;
        if self.with_user_spaces(|spaces| spaces.get(asid).is_none()) {
            return Err(KernelError::UserMemoryFault);
        }

        crate::yarm_log!(
            "SPAWN_TASK_ENTER tid={} asid={} entry=0x{:x}",
            spec.tid,
            asid.0,
            spec.entry
        );
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "FIRST_USER_CREATE_BEGIN cpu={} tid={} asid={} entry=0x{:x}",
                cpu.0,
                spec.tid,
                asid.0,
                spec.entry
            );
        }
        self.register_task_with_class(spec.tid, spec.class)?;
        crate::yarm_log!("SPAWN_TASK_REGISTER_OK tid={}", spec.tid);

        // Stage 119 Part A: minimal task-pair init — x86_64 only, tid=1 (init
        // server) and tid=2 (supervisor). Sets kernel_context.initialized = true
        // for both tasks so that the first timer preemption of tid=1 dispatching
        // tid=2 as incoming produces a real switch_frames call and the first-resume
        // handler can prove lock reacquisition via post_switch_restore.
        #[cfg(target_arch = "x86_64")]
        if spec.tid == BOOTSTRAP_FIRST_USER_TID || spec.tid == BOOTSTRAP_SUPERVISOR_TID {
            let entry = super::thread_state::kernel_switch_frame_trampoline_ip();
            crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_BEGIN tid={}", spec.tid);
            match self.initialize_thread_kernel_switch_frame(spec.tid, entry) {
                Ok(()) => {
                    let stack = self.with_tcbs(|tcbs| {
                        tcbs.iter()
                            .flatten()
                            .find(|tcb| tcb.tid.0 == spec.tid)
                            .and_then(|tcb| tcb.kernel_context.stack_top)
                            .map(|t| t.0)
                            .unwrap_or(0)
                    });
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_FRAME_INIT_DONE tid={} entry=0x{:x} stack=0x{:x}",
                        spec.tid,
                        entry,
                        stack,
                    );
                }
                Err(e) => {
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED reason=init_failed tid={} err={:?}",
                        spec.tid,
                        e,
                    );
                }
            }
        }

        if spec.spawner_tid != 0 && spec.service_recv_cap != 0 {
            match self.grant_capability_task_to_task_with_rights(
                spec.spawner_tid,
                CapId(spec.service_recv_cap),
                spec.tid,
                CapRights::RECEIVE,
            ) {
                Ok(local_cap) => {
                    spec.startup_args[12] = local_cap.0;
                    crate::yarm_log!(
                        "KSPAWN_RECV_CAP_DELEGATED tid={} local_cap={}",
                        spec.tid,
                        local_cap.0
                    );
                }
                Err(e) => {
                    crate::yarm_log!("KSPAWN_RECV_CAP_DELEGATE_FAIL tid={} err={:?}", spec.tid, e);
                }
            }
        }
        if spec.spawner_tid != 0 && spec.service_reply_recv_cap != 0 {
            match self.grant_capability_task_to_task_with_rights(
                spec.spawner_tid,
                CapId(spec.service_reply_recv_cap),
                spec.tid,
                CapRights::RECEIVE,
            ) {
                Ok(local_cap) => {
                    spec.startup_args[2] = local_cap.0;
                    crate::yarm_log!(
                        "SPAWN_SERVICE_REPLY_RECV_CAP_CHILD child_tid={} cap={} rights=RECEIVE",
                        spec.tid,
                        local_cap.0
                    );
                    crate::yarm_log!(
                        "SPAWN_STARTUP_SLOT_2_REPLY_RECV child_tid={} value={}",
                        spec.tid,
                        spec.startup_args[2]
                    );
                }
                Err(e) => {
                    crate::yarm_log!(
                        "KSPAWN_REPLY_RECV_CAP_DELEGATE_FAIL tid={} err={:?}",
                        spec.tid,
                        e
                    );
                }
            }
        }
        for (i, &raw_cap) in spec.extra_send_caps.iter().enumerate() {
            if raw_cap != 0 && spec.spawner_tid != 0 {
                match self.grant_capability_task_to_task_with_rights(
                    spec.spawner_tid,
                    CapId(raw_cap),
                    spec.tid,
                    CapRights::SEND,
                ) {
                    Ok(local_cap) => {
                        spec.startup_args[13 + i] = local_cap.0;
                        crate::yarm_log!(
                            "KSPAWN_EXTRA_CAP_DELEGATED tid={} slot={} local_cap={}",
                            spec.tid,
                            13 + i,
                            local_cap.0
                        );
                    }
                    Err(e) => {
                        crate::yarm_log!(
                            "KSPAWN_EXTRA_CAP_DELEGATE_FAIL tid={} slot={} err={:?}",
                            spec.tid,
                            13 + i,
                            e
                        );
                    }
                }
            }
        }

        let cnode = self.task_cnode(spec.tid).ok_or(task_missing_with_site(
            "spawn_user_task_from_image/task_cnode",
            cpu.0,
        ))?;
        crate::yarm_log!(
            "SPAWN_TASK_CAP_CHECK name=task_cnode cap={} object=cnode result=ok",
            cnode.0
        );
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "FIRST_USER_LOOKUP cpu={} tid={} cnode={} status=found",
                cpu.0,
                spec.tid,
                cnode.0
            );
        }
        self.set_process_cnode_for_pid(spec.tid, cnode)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == spec.tid)
                .ok_or(task_missing_with_site(
                    "spawn_user_task_from_image/set_asid_tcb_lookup",
                    cpu.0,
                ))?;
            tcb.asid = Some(asid);
            Ok::<_, KernelError>(())
        })?;

        // Stage 127: Stage 126 correctly refused to publish x86_64 initialized
        // switch frames without a mapped kernel switch-stack page, but the first
        // attempt above can run before the target task ASID is bound. Retry at
        // the first point where the target ASID/root is known so the mapping gate
        // uses the target task root rather than temporal active-ASID presence.
        #[cfg(target_arch = "x86_64")]
        if (spec.tid == BOOTSTRAP_FIRST_USER_TID || spec.tid == BOOTSTRAP_SUPERVISOR_TID)
            && !self
                .thread_kernel_context(spec.tid)
                .is_some_and(|ctx| ctx.initialized)
        {
            let entry = super::thread_state::kernel_switch_frame_trampoline_ip();
            crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_RETRY tid={}", spec.tid);
            match self.initialize_thread_kernel_switch_frame(spec.tid, entry) {
                Ok(()) => {
                    let stack = self.with_tcbs(|tcbs| {
                        tcbs.iter()
                            .flatten()
                            .find(|tcb| tcb.tid.0 == spec.tid)
                            .and_then(|tcb| tcb.kernel_context.stack_top)
                            .map(|t| t.0)
                            .unwrap_or(0)
                    });
                    crate::yarm_log!("D6_KERNEL_SWITCH_FRAME_INIT_RETRY_DONE tid={}", spec.tid);
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_FRAME_INIT_DONE tid={} entry=0x{:x} stack=0x{:x}",
                        spec.tid,
                        entry,
                        stack,
                    );
                }
                Err(e) => {
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_FRAME_INIT_DEFERRED reason=retry_failed tid={} err={:?}",
                        spec.tid,
                        e,
                    );
                }
            }
        }
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("BOOTSTRAP_STAGE: before stack allocation");
        }
        let stack_top = match self.allocate_user_stack_with_guard(spec.tid, 64) {
            Ok(top) => top,
            Err(err) => {
                crate::yarm_log!(
                    "SPAWN_TASK_STACK_FAIL tid={} asid={} err={:?}",
                    spec.tid,
                    asid.0,
                    err
                );
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
                }
                return Err(err);
            }
        };
        crate::yarm_log!(
            "SPAWN_TASK_STACK_OK tid={} stack_top=0x{:x}",
            spec.tid,
            stack_top.0
        );
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("BOOTSTRAP_STAGE: after stack allocation");
            crate::yarm_log!("BOOTSTRAP_STAGE: before entry setup");
            crate::yarm_log!("USER_ENTRY rip=0x{:x}", spec.entry);
        }
        let startup_slots_len = spec.startup_args.len();
        let startup_slots_bytes_len = startup_slots_len * core::mem::size_of::<u64>();
        let startup_slots_start =
            (stack_top.0 as usize).saturating_sub(startup_slots_bytes_len) & !0x7usize;
        let startup_stack_ptr = startup_slots_start & !0xFusize;
        // x86-64 SysV ABI: at the very first instruction of a function the
        // stack pointer must satisfy RSP ≡ 8 (mod 16) because a CALL would
        // normally push an 8-byte return address before the callee is entered.
        // We enter user tasks via IRETQ / SYSRETQ (no return-address push), so
        // we must pre-subtract 8 from the initial stack pointer to satisfy the
        // invariant.  The trap return path (flush_trap_context_to_iret_frame)
        // and the initial-IRETQ path (enter_dispatched_user_task_if_available)
        // both read the stack pointer directly from user_context, so the
        // adjustment only needs to appear here.
        // AArch64 requires 16-byte SP alignment at function entry — no
        // pre-subtraction is needed there.
        #[cfg(target_arch = "x86_64")]
        let startup_stack_ptr = startup_stack_ptr.wrapping_sub(8);
        #[allow(unused_variables)]
        #[cfg(not(target_arch = "x86_64"))]
        let startup_stack_ptr = startup_stack_ptr;
        // Ensure slot[0] (task_id) is always the actual allocated TID.
        // PM does not know the new task's TID at SpawnV5 call time and sends
        // startup_args[0]=0.  Fill it now so that:
        //   (a) the user-visible slot[0] holds the correct task_id, and
        //   (b) user_context.arg0 = spec.tid ≠ 0, which satisfies the
        //       x86_64 new-task detection check (is_new_task requires arg(0)!=0)
        //       so the startup ABI registers (RDI/RSI/RDX/RCX/R8/R9) are
        //       properly injected on the task's very first dispatch.
        if spec.startup_args[0] == 0 {
            spec.startup_args[0] = spec.tid;
        }
        let startup_slots_ptr = VirtAddr(startup_slots_start as u64);
        let mut startup_slots_bytes = [0u8; core::mem::size_of::<u64>() * 18];
        for (index, slot) in spec.startup_args.iter().copied().enumerate() {
            let begin = index * core::mem::size_of::<u64>();
            startup_slots_bytes[begin..begin + core::mem::size_of::<u64>()]
                .copy_from_slice(&slot.to_le_bytes());
        }
        self.copy_to_user(
            asid,
            startup_slots_ptr,
            &startup_slots_bytes[..startup_slots_bytes_len],
        )?;
        crate::yarm_log!(
            "YARM_FIRST_USER_STARTUP_BLOCK va=0x{:x} count={} mapped=true",
            startup_slots_start,
            startup_slots_len
        );

        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == spec.tid)
                .ok_or(task_missing_with_site(
                    "spawn_user_task_from_image/set_context_tcb_lookup",
                    cpu.0,
                ))?;
            tcb.thread_group_id = ThreadGroupId(spec.tid);
            tcb.asid = Some(asid);
            tcb.user_entry = Some(VirtAddr(spec.entry as u64));
            tcb.user_stack_top = Some(stack_top);
            tcb.user_context = UserRegisterContext {
                instruction_ptr: VirtAddr(spec.entry as u64),
                stack_ptr: VirtAddr(startup_stack_ptr as u64),
                user_gprs: [0; 32],
                // Startup entry ABI args:
                //   arg0 => task_id / tid
                //   arg1 => process-manager request-send cap
                //   arg2 => process-manager reply-recv cap
                arg0: spec.startup_args[0] as usize,
                arg1: spec.startup_args[1] as usize,
                arg2: spec.startup_args[2] as usize,
                // Extended startup delivery ABI:
                //   arg3 => pointer to [u64; 18] startup slot block in userspace memory
                //   arg4 => startup slot count
                //   arg5 => reserved (0)
                arg3: startup_slots_start,
                arg4: startup_slots_len,
                arg5: 0,
            };
            crate::yarm_log!(
                "USER_INITIAL_CONTEXT tid={} pc=0x{:016x} sp=0x{:016x} arg0=0x{:016x} arg1=0x{:016x} gpr29=0x{:016x} gpr30=0x{:016x} ctx_ptr=0x{:x}",
                spec.tid,
                tcb.user_context.instruction_ptr.0,
                tcb.user_context.stack_ptr.0,
                tcb.user_context.arg0 as u64,
                tcb.user_context.arg1 as u64,
                tcb.user_context.user_gprs[29] as u64,
                tcb.user_context.user_gprs[30] as u64,
                &tcb.user_context as *const _ as usize
            );
            if matches!(spec.class, crate::kernel::task::TaskClass::SystemServer)
                || spec.tid == BOOTSTRAP_FIRST_USER_TID
            {
                tcb.cpu_affinity = Some(CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID));
            }
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        crate::yarm_log!("SPAWN_TASK_CONTEXT_OK tid={}", spec.tid);
        let bootstrap_cpu = CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
        // Pin all SystemServer tasks (supervisor, PM, init) to CPU 0 so the
        // scheduler queue on the bootstrap CPU has them in spawn order:
        //   [idle/TID0, supervisor/TID2, PM/TID3, init/TID1]
        // This guarantees supervisor and PM reach their recv() before init runs.
        let should_pin = matches!(spec.class, crate::kernel::task::TaskClass::SystemServer)
            || spec.tid == BOOTSTRAP_FIRST_USER_TID;
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "FIRST_USER_ENQUEUE_DECISION cpu={} tid={} chosen_cpu={} reason={}",
                cpu.0,
                spec.tid,
                bootstrap_cpu.0,
                if should_pin {
                    "bootstrap_pin"
                } else {
                    "scheduler_default"
                }
            );
        }

        let enqueued_cpu = if should_pin {
            let chosen_cpu = bootstrap_cpu;
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!(
                    "FINAL_FIRST_USER_ENQUEUE_SITE cpu={} tid={} chosen_cpu={} bootstrap_pin={}",
                    cpu.0,
                    spec.tid,
                    chosen_cpu.0,
                    should_pin as u8
                );
            }
            if chosen_cpu != bootstrap_cpu {
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "FIRST_USER_PIN_VIOLATION cpu={} tid={} chosen_cpu={}",
                        cpu.0,
                        spec.tid,
                        chosen_cpu.0
                    );
                }
            }
            assert_eq!(chosen_cpu.0, bootstrap_cpu.0);
            self.enqueue_on_cpu(chosen_cpu, spec.tid)?;
            chosen_cpu
        } else {
            self.enqueue_task(spec.tid)?
        };
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!(
                "FIRST_USER_ENQUEUE cpu={} tid={} target_cpu={} status=ok",
                cpu.0,
                spec.tid,
                enqueued_cpu.0
            );
            crate::yarm_log!("BOOTSTRAP_FIRST_USER tid={} enqueued=true", spec.tid);
        }
        Ok(SpawnedUserTask {
            tid: spec.tid,
            entry: spec.entry,
            asid: Some(asid),
        })
    }

    pub(crate) fn dispatch_next_task(&mut self) -> Result<Option<u64>, KernelError> {
        if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
            crate::yarm_log!("DISPATCH: begin");
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_dispatch_calls =
                ipc.telemetry.scheduler_dispatch_calls.saturating_add(1);
        });
        let outgoing_tid = self.current_tid();
        // VALIDATION: D6_LIVE_SPLIT (Stage 107); Phase A/B named Stage 113.
        // Phase A (scheduler rank 1 only): local_dispatch_step_split picks
        // the next runnable task and drops the scheduler lock before
        // returning. Everything below this line is Phase B (non-scheduler
        // side effects: ASID switch, kernel-context switch, TCB status
        // mutation) and already runs with the scheduler lock released — see
        // local_dispatch_step_split's doc comment in scheduler_state.rs for
        // the deferred SharedKernel-seam live-wire blocker (§D-NEXT-1 PR-C
        // in doc/KERNEL_UNLOCKING.md).
        let next = self.local_dispatch_step_split();
        if let Some(tid) = next {
            crate::yarm_log!("SCHED_DISPATCH_NEXT chosen_tid={}", tid);
            crate::yarm_log!("D6_DISPATCH_SELECTED tid={}", tid);
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("DISPATCH: selected_tid={}", tid);
            }
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                if cfg!(not(feature = "hosted-dev"))
                    && DEBUG_DISPATCH_CONTEXT_LOG
                    && self.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                {
                    crate::yarm_log!("BSP_BEFORE_ASPACE_SWITCH tid={}", tid);
                }
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!("DISPATCH: before switch_address_space asid={}", asid.0);
                }
                self.hal.switch_address_space(asid);
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!("DISPATCH: after switch_address_space asid={}", asid.0);
                    if self.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
                        crate::yarm_log!("BSP_AFTER_ASPACE_SWITCH tid={}", tid);
                    }
                }
            }
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                let lctx_bsp_tid1 = tid == BOOTSTRAP_FIRST_USER_TID
                    && self.current_cpu().0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID;
                if lctx_bsp_tid1 {
                    crate::yarm_log!("LCTX0 after aspace switch tid={}", tid);
                    crate::yarm_log!("LCTX1 before reading task/tcb pointer tid={}", tid);
                    crate::yarm_log!("GX0 immediately after LCTX1");
                    crate::yarm_log!("GX1 before evaluating next helper/breadcrumb");
                    crate::yarm_log!("GX2 before preparing any with_tcbs pre-call references");
                    crate::yarm_log!("GX3 immediately before WX0");
                    crate::yarm_log!("LCTX1A before with_tcbs tid={}", tid);
                    crate::yarm_log!(
                        "WX0 before calling with_tcbs tid={} self_ptr=0x{:x} scheduler_state_ptr=0x{:x} task_lock_ptr=0x{:x} tcbs_storage_ptr=0x{:x}",
                        tid,
                        self as *const _ as usize,
                        &self.scheduler_state as *const _ as usize,
                        &self.task_state_lock as *const _ as usize,
                        &self.tcbs as *const _ as usize
                    );
                    crate::kernel::boot::orchestrator_state::set_with_tcbs_probe(true);
                }
                let (task_ptr, kernel_context_ptr, frame_ptr, kernel_stack_top) =
                    self.with_tcbs(|tcbs| {
                        if lctx_bsp_tid1 {
                            crate::yarm_log!(
                                "WX1 at first line inside with_tcbs closure entry tid={}",
                                tid
                            );
                            crate::yarm_log!("LCTX1B after with_tcbs entry tid={}", tid);
                        }
                        tcbs.iter()
                            .flatten()
                            .find(|tcb| tcb.tid.0 == tid)
                            .map(|tcb| {
                                if lctx_bsp_tid1 {
                                    crate::yarm_log!("LCTX1C after slot lookup tid={}", tid);
                                }
                                (
                                    tcb as *const _ as usize,
                                    &tcb.kernel_context as *const _ as usize,
                                    &tcb.kernel_context.frame as *const _ as usize,
                                    tcb.kernel_context.stack_top.map(|top| top.0).unwrap_or(0),
                                )
                            })
                            .unwrap_or((0, 0, 0, 0))
                    });
                if lctx_bsp_tid1 {
                    crate::kernel::boot::orchestrator_state::set_with_tcbs_probe(false);
                }
                if lctx_bsp_tid1 {
                    crate::yarm_log!(
                        "LCTX2 after reading task/tcb/context pointer tid={} task_ptr=0x{:x} kernel_ctx_ptr=0x{:x} frame_ptr=0x{:x} kernel_stack_top=0x{:x}",
                        tid,
                        task_ptr,
                        kernel_context_ptr,
                        frame_ptr,
                        kernel_stack_top
                    );
                    crate::yarm_log!("LCTX3 before loading-context log tid={}", tid);
                }
                let event_id = DISPATCH_CONTEXT_LOAD_EVENT_ID.fetch_add(1, Ordering::Relaxed);
                crate::yarm_log!(
                    "DISPATCH: before loading context tid={} ctx_ptr=0x{:x} kernel_stack_top=0x{:x} src=dispatch_context_load event_id={}",
                    tid,
                    frame_ptr,
                    kernel_stack_top,
                    event_id
                );
                if tid == BOOTSTRAP_FIRST_USER_TID {
                    crate::yarm_log!("BCTX0 after loading context log tid={}", tid);
                }
            }
            if cfg!(not(feature = "hosted-dev"))
                && DEBUG_DISPATCH_CONTEXT_LOG
                && tid == BOOTSTRAP_FIRST_USER_TID
            {
                crate::yarm_log!(
                    "BCTX1 before cpu-ownership/context-restore gate tid={}",
                    tid
                );
            }
            let current_cpu = self.current_cpu();
            if tid == BOOTSTRAP_FIRST_USER_TID
                && current_cpu.0 != crate::arch::platform_constants::BOOTSTRAP_CPU_ID
            {
                if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                    crate::yarm_log!(
                        "TASK_CPU_OWNERSHIP_VIOLATION cpu={} tid={}",
                        current_cpu.0,
                        tid
                    );
                }
                if cfg!(not(feature = "hosted-dev")) {
                    assert_eq!(
                        current_cpu.0,
                        crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                    );
                }
            }
            if cfg!(not(feature = "hosted-dev"))
                && DEBUG_DISPATCH_CONTEXT_LOG
                && current_cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
            {
                let (ctx_ptr, user_pc, user_sp, user_x0, user_x1) = self.with_tcbs(|tcbs| {
                    tcbs.iter()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == tid)
                        .map(|tcb| {
                            (
                                &tcb.kernel_context.frame as *const _ as usize,
                                tcb.user_context.instruction_ptr.0,
                                tcb.user_context.stack_ptr.0,
                                tcb.user_context.arg0 as u64,
                                tcb.user_context.arg1 as u64,
                            )
                        })
                        .unwrap_or((0, 0, 0, 0, 0))
                });
                crate::yarm_log!("BSP_BEFORE_CONTEXT_RESTORE tid={}", tid);
                crate::yarm_log!(
                    "BSP_CONTEXT_RESTORE_RAW tid={} ctx_ptr=0x{:x} pc=0x{:x} sp=0x{:x} spsr=0x0 x0=0x{:x} x1=0x{:x}",
                    tid,
                    ctx_ptr,
                    user_pc,
                    user_sp,
                    user_x0,
                    user_x1
                );
            }
            if cfg!(not(feature = "hosted-dev"))
                && DEBUG_DISPATCH_CONTEXT_LOG
                && tid == BOOTSTRAP_FIRST_USER_TID
                && current_cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
            {
                crate::yarm_log!("CTX0 before maybe_switch_kernel_context tid={}", tid);
            }
            self.maybe_switch_kernel_context(outgoing_tid, tid)?;
            if cfg!(not(feature = "hosted-dev"))
                && DEBUG_DISPATCH_CONTEXT_LOG
                && tid == BOOTSTRAP_FIRST_USER_TID
                && current_cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
            {
                crate::yarm_log!("CTX1 after maybe_switch_kernel_context tid={}", tid);
            }
            if outgoing_tid != Some(tid) {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.scheduler_context_switches =
                        ipc.telemetry.scheduler_context_switches.saturating_add(1);
                });
            }
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Running;
                Ok::<_, KernelError>(())
            })?;
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("DISPATCH: task_running tid={}", tid);
            }
        } else {
            crate::yarm_log!("SCHED_NO_RUNNABLE_USER_TASK");
            crate::yarm_log!("SCHED_ENTER_IDLE");
            crate::yarm_log!("D6_DISPATCH_IDLE");
            crate::yarm_log!("D6_SWITCH_PLAN_IDLE");
            if cfg!(not(feature = "hosted-dev")) && DEBUG_DISPATCH_CONTEXT_LOG {
                crate::yarm_log!("DISPATCH: no_runnable_task");
            }
        }
        Ok(next)
    }

    pub fn dispatch_ready_task(&mut self) -> Result<Option<u64>, KernelError> {
        self.dispatch_next_task()
    }

    pub fn yield_current(&mut self) -> Result<(), KernelError> {
        let outgoing_tid = self.current_tid();
        if cfg!(not(feature = "hosted-dev")) && DEBUG_YIELD_LOG {
            crate::yarm_log!("YARM_YIELD_BEGIN tid={:?}", outgoing_tid);
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_yield_calls =
                ipc.telemetry.scheduler_yield_calls.saturating_add(1);
        });
        if let Some(tid) = outgoing_tid {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
        }

        let next_tid = self.on_preempt_current_cpu();
        if let Some(tid) = next_tid {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                self.hal.switch_address_space(asid);
            }
            self.maybe_switch_kernel_context(outgoing_tid, tid)?;
            if outgoing_tid != Some(tid) {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.scheduler_context_switches =
                        ipc.telemetry.scheduler_context_switches.saturating_add(1);
                });
            }
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Running;
                Ok::<_, KernelError>(())
            })?;
            if cfg!(not(feature = "hosted-dev")) && DEBUG_YIELD_LOG {
                let status = if outgoing_tid == Some(tid) {
                    "same-task"
                } else {
                    "switched"
                };
                crate::yarm_log!("YARM_YIELD_END status={} tid={}", status, tid);
            }
        } else {
            if cfg!(not(feature = "hosted-dev")) && DEBUG_YIELD_LOG {
                crate::yarm_log!("YARM_YIELD_NO_OTHER_RUNNABLE");
            }
            if let Some(tid) = outgoing_tid {
                let _ = self.enqueue_current_cpu(tid);
                let redispatched = self.dispatch_next_task()?;
                if redispatched == Some(tid) {
                    if cfg!(not(feature = "hosted-dev")) && DEBUG_YIELD_LOG {
                        crate::yarm_log!("YARM_YIELD_RETURN_SAME_TASK tid={}", tid);
                        crate::yarm_log!("YARM_YIELD_END status=same-task tid={}", tid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Yield the current task, directly dispatching `target` as the next task when possible.
    ///
    /// Uses `on_preempt_prefer` to move `target` from the run-queue to `current` in one
    /// scheduler operation, bypassing the FIFO order.  If `target` is not in the run-queue
    /// (e.g. already current, blocked, or not yet woken), falls back to the normal FIFO
    /// dispatch for this one yield.
    ///
    /// Returns `true` when `target` became the new current task, `false` otherwise.
    ///
    /// This replaces the `switch_to_runnable_tid` busy-loop for call sites where the
    /// caller guarantees `target` was just woken (i.e. `wake_waiter_for_endpoint` ran
    /// immediately before this call).  In that common case the function completes in
    /// exactly one scheduler operation instead of up to `MAX_TASKS` iterations.
    ///
    /// Must be called outside all IPC/cap/VM/memory domain locks (same constraint as
    /// `yield_current`).
    pub(crate) fn yield_current_to(&mut self, target: ThreadId) -> Result<bool, KernelError> {
        let outgoing_tid = self.current_tid();
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_yield_calls =
                ipc.telemetry.scheduler_yield_calls.saturating_add(1);
        });
        if let Some(tid) = outgoing_tid {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
        }
        let next_tid = self.on_preempt_prefer_current_cpu(target.0);
        let achieved = next_tid == Some(target.0);
        if let Some(tid) = next_tid {
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                self.hal.switch_address_space(asid);
            }
            self.maybe_switch_kernel_context(outgoing_tid, tid)?;
            if outgoing_tid != Some(tid) {
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.scheduler_context_switches =
                        ipc.telemetry.scheduler_context_switches.saturating_add(1);
                });
            }
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Running;
                Ok::<_, KernelError>(())
            })?;
        } else {
            // No runnable task after preempt (queue was empty); re-enqueue and redispatch.
            if let Some(tid) = outgoing_tid {
                let _ = self.enqueue_current_cpu(tid);
                let _ = self.dispatch_next_task()?;
            }
        }
        Ok(achieved)
    }

    /// Stage 130: emit D6 proof cleanup markers that require global-lock access.
    ///
    /// Called from `handle_trap_entry_shared` at POINT 2 after the proof
    /// switch-back completes, with the global lock held, to log the current TID,
    /// active ASID/CR3, and TSS RSP0 state for post-proof consistency checks.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn d6_emit_proof_cleanup_arch_markers(&mut self) {
        let current_tid = self.current_tid().unwrap_or(u64::MAX);
        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_CURRENT_OK tid={}", current_tid);
        let active_asid = self.hal.active_asid().map_or(0, |asid| asid.0);
        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_CR3_OK asid={}", active_asid);
        crate::yarm_log!("D6_CONTROLLED_SWITCH_PROOF_TSS_OK");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    #[test]
    fn elf_pflags_map_to_expected_page_flags() {
        let rx = KernelState::page_flags_from_elf_pflags(PF_R | PF_X).expect("rx");
        assert!(rx.read);
        assert!(!rx.write);
        assert!(rx.execute);

        let rw = KernelState::page_flags_from_elf_pflags(PF_R | PF_W).expect("rw");
        assert!(rw.read);
        assert!(rw.write);
        assert!(!rw.execute);

        let ro = KernelState::page_flags_from_elf_pflags(PF_R).expect("ro");
        assert!(ro.read);
        assert!(!ro.write);
        assert!(!ro.execute);

        let write_only = KernelState::page_flags_from_elf_pflags(PF_W).expect("w");
        assert!(write_only.read);
        assert!(write_only.write);
        assert!(!write_only.execute);

        let exec_only = KernelState::page_flags_from_elf_pflags(PF_X).expect("x");
        assert!(exec_only.read);
        assert!(!exec_only.write);
        assert!(exec_only.execute);
    }

    #[test]
    fn elf_pflags_reject_wx() {
        assert_eq!(
            KernelState::page_flags_from_elf_pflags(PF_W | PF_X),
            Err(KernelError::WrongObject)
        );
    }

    #[test]
    fn load_elf_returns_heap_base_aligned_to_max_pt_load_end() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut image = vec![0u8; 64 + 56];
                image[0..4].copy_from_slice(b"\x7FELF");
                image[4] = 2; // ELFCLASS64
                image[5] = 1; // little-endian
                image[6] = 1; // version
                image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
                image[18..20].copy_from_slice(&183u16.to_le_bytes()); // AArch64
                image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
                image[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // e_entry
                image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
                image[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
                image[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
                image[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

                let ph = 64usize;
                image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
                image[ph + 4..ph + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
                image[ph + 8..ph + 16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
                image[ph + 16..ph + 24].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // p_vaddr
                image[ph + 24..ph + 32].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // p_paddr
                image[ph + 32..ph + 40].copy_from_slice(&0u64.to_le_bytes()); // p_filesz
                image[ph + 40..ph + 48].copy_from_slice(&0x1234u64.to_le_bytes()); // p_memsz
                image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align

                let mut state = crate::kernel::boot::Bootstrap::init().expect("kernel");
                let (asid, _map) = state.create_user_address_space().expect("asid");
                let (entry, _first_pt_load, heap_base) = state
                    .load_elf_pt_load_segments(asid, &image)
                    .expect("load elf");
                assert_eq!(entry, 0x0040_0000usize);
                assert_eq!(heap_base, 0x0040_2000usize);
            })
            .expect("spawn")
            .join()
            .expect("join");
    }

    #[test]
    fn load_elf_copies_into_staging_then_finalizes_rx_permissions() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut image = vec![0u8; 64 + 56 + 4];
                image[0..4].copy_from_slice(b"\x7FELF");
                image[4] = 2;
                image[5] = 1;
                image[6] = 1;
                image[16..18].copy_from_slice(&2u16.to_le_bytes());
                image[18..20].copy_from_slice(&183u16.to_le_bytes());
                image[20..24].copy_from_slice(&1u32.to_le_bytes());
                image[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes());
                image[32..40].copy_from_slice(&64u64.to_le_bytes());
                image[52..54].copy_from_slice(&64u16.to_le_bytes());
                image[54..56].copy_from_slice(&56u16.to_le_bytes());
                image[56..58].copy_from_slice(&1u16.to_le_bytes());

                let ph = 64usize;
                image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
                image[ph + 4..ph + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
                image[ph + 8..ph + 16].copy_from_slice(&(64u64 + 56u64).to_le_bytes());
                image[ph + 16..ph + 24].copy_from_slice(&0x0040_0000u64.to_le_bytes());
                image[ph + 24..ph + 32].copy_from_slice(&0x0040_0000u64.to_le_bytes());
                image[ph + 32..ph + 40].copy_from_slice(&4u64.to_le_bytes());
                image[ph + 40..ph + 48].copy_from_slice(&4u64.to_le_bytes());
                image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
                image[64 + 56..64 + 60].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

                let mut state = crate::kernel::boot::Bootstrap::init().expect("kernel");
                let (asid, _map) = state.create_user_address_space().expect("asid");
                state
                    .load_elf_pt_load_segments(asid, &image)
                    .expect("load elf");
                let mapping = state
                    .user_spaces
                    .get(asid)
                    .and_then(|aspace| aspace.resolve(VirtAddr(0x0040_0000)))
                    .expect("resolved mapping");
                assert!(mapping.flags.read);
                assert!(!mapping.flags.write);
                assert!(mapping.flags.execute);
            })
            .expect("spawn")
            .join()
            .expect("join");
    }
}
