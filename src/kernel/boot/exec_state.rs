// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, SpawnedUserTask, UserImageSpec};
use crate::kernel::capabilities::{CapId, CapRights};
use crate::arch::hal::Hal;
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

    fn maybe_switch_kernel_context(
        &mut self,
        outgoing_tid: Option<u64>,
        incoming_tid: u64,
    ) -> Result<(), KernelError> {
        let Some(outgoing_tid) = outgoing_tid else {
            return Ok(());
        };
        if outgoing_tid == incoming_tid {
            return Ok(());
        }

        let outgoing_idx = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == outgoing_tid))
            })
            .ok_or(KernelError::TaskMissing)?;
        let incoming_idx = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == incoming_tid))
            })
            .ok_or(KernelError::TaskMissing)?;

        if outgoing_idx == incoming_idx {
            return Ok(());
        }

        self.with_tcbs_mut(|tcbs| {
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

            if !outgoing_tcb.kernel_context.initialized || !incoming_tcb.kernel_context.initialized
            {
                return Ok(());
            }

            crate::arch::selected_isa::context_switch::switch_frames(
                &mut outgoing_tcb.kernel_context.frame,
                &incoming_tcb.kernel_context.frame,
                incoming_tcb.kernel_context.stack_top.map(|top| top.0),
            );
            Ok(())
        })
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

        if spec.spawner_tid != 0 && spec.service_recv_cap != 0 {
            match self.grant_capability_task_to_task_with_rights(
                spec.spawner_tid,
                CapId(spec.service_recv_cap),
                spec.tid,
                CapRights::RECEIVE,
            ) {
                Ok(local_cap) => {
                    spec.startup_args[12] = local_cap.0;
                    crate::yarm_log!("KSPAWN_RECV_CAP_DELEGATED tid={} local_cap={}", spec.tid, local_cap.0);
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
                        "KSPAWN_REPLY_RECV_CAP_DELEGATED tid={} local_cap={}",
                        spec.tid,
                        local_cap.0
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
                        crate::yarm_log!("KSPAWN_EXTRA_CAP_DELEGATED tid={} slot={} local_cap={}", spec.tid, 13 + i, local_cap.0);
                    }
                    Err(e) => {
                        crate::yarm_log!("KSPAWN_EXTRA_CAP_DELEGATE_FAIL tid={} slot={} err={:?}", spec.tid, 13 + i, e);
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
                "AARCH64_INITIAL_CONTEXT tid={} elr=0x{:016x} sp=0x{:016x} x0=0x{:016x} x1=0x{:016x} x29=0x{:016x} x30=0x{:016x} ctx_ptr=0x{:x}",
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
        let next = self.dispatch_next_current_cpu();
        if let Some(tid) = next {
            crate::yarm_log!("SCHED_DISPATCH_NEXT chosen_tid={}", tid);
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
                assert_eq!(
                    current_cpu.0,
                    crate::arch::platform_constants::BOOTSTRAP_CPU_ID
                );
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
    }

    #[test]
    fn load_elf_copies_into_staging_then_finalizes_rx_permissions() {
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
    }
}
