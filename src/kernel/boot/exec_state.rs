// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState, SpawnedUserTask, UserImageSpec};
use crate::arch::hal::Hal;
use crate::kernel::frame_allocator::alloc_pt_frame;
use crate::kernel::task::{TaskStatus, ThreadGroupId, UserRegisterContext, WaitReason};
use crate::kernel::vm::{Asid, CachePolicy, Mapping, PageFlags, PhysAddr, VirtAddr, PAGE_SIZE};

const ELF64_EHDR_SIZE: usize = 64;
const ELF64_PHDR_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;

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

impl KernelState {
    /// Minimal ELF64 loader for PT_LOAD segments:
    /// validates headers, maps pages for each load segment, copies file bytes,
    /// and zero-fills the BSS tail.
    pub fn load_elf_pt_load_segments(
        &mut self,
        asid: Asid,
        image: &[u8],
    ) -> Result<usize, KernelError> {
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
        let phend = phoff.checked_add(table_size).ok_or(KernelError::WrongObject)?;
        if phend > image.len() {
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
            let p_flags = read_u32_le(image, base + 4)?;
            let p_offset = read_u64_le(image, base + 8)? as usize;
            let p_vaddr = read_u64_le(image, base + 16)?;
            let p_filesz = read_u64_le(image, base + 32)? as usize;
            let p_memsz = read_u64_le(image, base + 40)? as usize;
            if p_filesz > p_memsz {
                return Err(KernelError::WrongObject);
            }
            let file_end = p_offset
                .checked_add(p_filesz)
                .ok_or(KernelError::WrongObject)?;
            if file_end > image.len() {
                return Err(KernelError::WrongObject);
            }

            let page_size = PAGE_SIZE as u64;
            let seg_start = p_vaddr;
            let seg_end = p_vaddr
                .checked_add(p_memsz as u64)
                .ok_or(KernelError::WrongObject)?;
            let page_start = seg_start & !(page_size - 1);
            let page_end = (seg_end + page_size - 1) & !(page_size - 1);
            let flags = PageFlags {
                // Loader maps writable/readable so copy_to_user can materialize
                // PT_LOAD bytes; tightening to final RX/RW permissions can be
                // layered later once per-segment reprotect is wired.
                read: true,
                write: true,
                execute: (p_flags & PF_X) != 0,
                user: true,
                cache_policy: CachePolicy::WriteBack,
            };
            let mut va = page_start;
            while va < page_end {
                let phys = alloc_pt_frame().map_err(|_| KernelError::MemoryObjectFull)?;
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!(
                        "ELF_MAP_PAGE_BEGIN asid={} seg_vbase=0x{:x} page_va=0x{:x} phys=0x{:x} memsz={} filesz={}",
                        asid.0,
                        p_vaddr,
                        va,
                        phys,
                        p_memsz,
                        p_filesz
                    );
                }
                self.map_user_page_in_asid_raw(
                    asid,
                    VirtAddr(va),
                    Mapping {
                        phys: PhysAddr(phys),
                        flags,
                    },
                )?;
                let post_map_present =
                    crate::arch::selected_isa::page_table::resolve_page(asid, VirtAddr(va))
                        .is_some();
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!(
                        "ELF_MAP_PAGE_DONE asid={} page_va=0x{:x} post_resolve={}",
                        asid.0,
                        va,
                        post_map_present
                    );
                }
                if !post_map_present {
                    if cfg!(not(feature = "hosted-dev")) {
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
        Ok(entry as usize)
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
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
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
        if addr == 0 {
            return Err(KernelError::WrongObject);
        }
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

    pub fn spawn_user_task_from_image(
        &mut self,
        spec: UserImageSpec,
    ) -> Result<SpawnedUserTask, KernelError> {
        if spec.entry == 0 {
            return Err(KernelError::WrongObject);
        }
        let asid = spec.asid.ok_or(KernelError::UserMemoryFault)?;
        if self.with_user_spaces(|spaces| spaces.get(asid).is_none()) {
            return Err(KernelError::UserMemoryFault);
        }

        self.register_task_with_class(spec.tid, spec.class)?;
        let cnode = self.task_cnode(spec.tid).ok_or(KernelError::TaskMissing)?;
        self.set_process_cnode_for_pid(spec.tid, cnode)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == spec.tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.asid = Some(asid);
            Ok::<_, KernelError>(())
        })?;
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!("BOOTSTRAP_STAGE: before stack allocation");
        }
        let stack_top = match self.allocate_user_stack_with_guard(spec.tid, 64) {
            Ok(top) => top,
            Err(err) => {
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!("BOOTSTRAP_ERROR: {:?}", err);
                }
                return Err(err);
            }
        };
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!("BOOTSTRAP_STAGE: after stack allocation");
            crate::yarm_log!("BOOTSTRAP_STAGE: before entry setup");
            crate::yarm_log!("USER_ENTRY rip=0x{:x}", spec.entry);
        }
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == spec.tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.thread_group_id = ThreadGroupId(spec.tid);
            tcb.asid = Some(asid);
            tcb.user_entry = Some(VirtAddr(spec.entry as u64));
            tcb.user_stack_top = Some(stack_top);
            tcb.user_context = UserRegisterContext {
                instruction_ptr: VirtAddr(spec.entry as u64),
                stack_ptr: stack_top,
                arg0: 0,
                arg1: 0,
            };
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        Ok(SpawnedUserTask {
            tid: spec.tid,
            entry: spec.entry,
            asid: Some(asid),
        })
    }

    pub(crate) fn dispatch_next_task(&mut self) -> Result<Option<u64>, KernelError> {
        if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!("DISPATCH: begin");
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_dispatch_calls =
                ipc.telemetry.scheduler_dispatch_calls.saturating_add(1);
        });
        let outgoing_tid = self.current_tid();
        let next = self.dispatch_next_current_cpu();
        if let Some(tid) = next {
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!("DISPATCH: selected_tid={}", tid);
            }
            let incoming_asid = self.task_asid(tid);
            if let Some(asid) = incoming_asid {
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!("DISPATCH: before switch_address_space asid={}", asid.0);
                }
                self.hal.switch_address_space(asid);
                if cfg!(not(feature = "hosted-dev")) {
                    crate::yarm_log!("DISPATCH: after switch_address_space asid={}", asid.0);
                }
            }
            if cfg!(not(feature = "hosted-dev")) {
                let (context_ptr, kernel_stack_top) = self.with_tcbs(|tcbs| {
                    tcbs.iter()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == tid)
                        .map(|tcb| {
                            (
                                &tcb.kernel_context.frame as *const _ as usize,
                                tcb.kernel_context.stack_top.map(|top| top.0).unwrap_or(0),
                            )
                        })
                        .unwrap_or((0, 0))
                });
                crate::yarm_log!(
                    "DISPATCH: before loading context tid={} ctx_ptr=0x{:x} kernel_stack_top=0x{:x}",
                    tid,
                    context_ptr,
                    kernel_stack_top
                );
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
            if cfg!(not(feature = "hosted-dev")) {
                crate::yarm_log!("DISPATCH: task_running tid={}", tid);
            }
        } else if cfg!(not(feature = "hosted-dev")) {
            crate::yarm_log!("DISPATCH: no_runnable_task");
        }
        Ok(next)
    }

    pub fn dispatch_ready_task(&mut self) -> Result<Option<u64>, KernelError> {
        self.dispatch_next_task()
    }

    pub fn yield_current(&mut self) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.scheduler_yield_calls =
                ipc.telemetry.scheduler_yield_calls.saturating_add(1);
        });
        let outgoing_tid = self.current_tid();
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
        }
        Ok(())
    }
}
