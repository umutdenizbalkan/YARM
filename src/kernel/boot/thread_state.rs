// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::capabilities::CapObject;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    KernelExecutionContext, RobustFutexState, TaskStatus, ThreadDetachState, ThreadGroupId,
    UserRegisterContext, WaitReason,
};
use crate::kernel::trapframe::TrapFrame;

const KERNEL_STACK_REGION_BASE: usize = 0xFFFF_8000_0000_0000;
const KERNEL_STACK_REGION_SIZE: usize = 0x4000;
const USER_STACK_STRIDE_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const USER_VIRT_TOP_EXCLUSIVE: u64 = 0x0000_8000_0000_0000;
#[cfg(not(target_arch = "x86_64"))]
const USER_VIRT_TOP_EXCLUSIVE: u64 = crate::kernel::vm::KERNEL_SPACE_BASE;
const USER_STACK_TOP_BASE: u64 = USER_VIRT_TOP_EXCLUSIVE - USER_STACK_STRIDE_BYTES;

#[unsafe(no_mangle)]
pub extern "C" fn yarm_kernel_thread_switch_trampoline() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

impl KernelState {
    fn fork_should_inherit_capability(object: CapObject) -> bool {
        match object {
            // Conservative fork inheritance policy: keep ordinary userspace IPC/memory-object caps.
            CapObject::Endpoint { .. }
            | CapObject::Notification { .. }
            | CapObject::Reply { .. }
            | CapObject::MemoryObject { .. } => true,
            // Skip privileged/global capability classes by default.
            CapObject::Kernel
            | CapObject::Irq { .. }
            | CapObject::IovaSpace { .. }
            | CapObject::DmaRegion { .. }
            | CapObject::AddressSpace { .. } => false,
        }
    }

    fn inherit_parent_capabilities_for_fork(
        &mut self,
        parent_tid: u64,
        child_tid: u64,
    ) -> Result<(), KernelError> {
        let parent_caps = self.snapshot_live_capabilities_for_task(parent_tid)?;
        let mut minted_child_caps = alloc::vec::Vec::new();
        for (parent_cap_id, capability) in parent_caps {
            if !Self::fork_should_inherit_capability(capability.object) {
                continue;
            }
            match self.grant_capability_task_to_task_with_rights(
                parent_tid,
                parent_cap_id,
                child_tid,
                capability.rights(),
            ) {
                Ok(child_cap_id) => minted_child_caps.push(child_cap_id),
                Err(err) => {
                    for cap in minted_child_caps {
                        self.revoke_capability_direct_in_process_cnode(child_tid, cap);
                    }
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    pub fn thread_group_id(&self, tid: u64) -> Option<ThreadGroupId> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.thread_group_id)
        })
    }

    pub fn thread_tls_base(&self, tid: u64) -> Option<usize> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.tls_ptr.map(|ptr| ptr.0 as usize))
        })
    }

    pub fn process_id(&self, tid: u64) -> Option<u64> {
        self.thread_group_id(tid).map(|group_id| group_id.0)
    }

    pub fn is_thread_group_leader(&self, tid: u64) -> bool {
        self.thread_group_id(tid) == Some(ThreadGroupId(tid))
    }

    pub fn thread_user_context(&self, tid: u64) -> Option<UserRegisterContext> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.user_context)
        })
    }

    pub fn thread_kernel_context(&self, tid: u64) -> Option<KernelExecutionContext> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.kernel_context)
        })
    }

    pub fn set_thread_kernel_stack(
        &mut self,
        tid: u64,
        stack_base: usize,
        stack_top: usize,
    ) -> Result<(), KernelError> {
        if stack_base == 0 || stack_top == 0 || stack_base >= stack_top {
            return Err(KernelError::WrongObject);
        }
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.kernel_context.stack_base = Some(crate::kernel::vm::VirtAddr(stack_base as u64));
            tcb.kernel_context.stack_top = Some(crate::kernel::vm::VirtAddr(stack_top as u64));
            tcb.kernel_context.initialized = false;
            Ok(())
        })
    }

    pub fn initialize_thread_kernel_switch_frame(
        &mut self,
        tid: u64,
        switch_entry: usize,
    ) -> Result<(), KernelError> {
        if switch_entry == 0 {
            return Err(KernelError::WrongObject);
        }
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let stack_top = tcb
                .kernel_context
                .stack_top
                .ok_or(KernelError::WrongObject)?
                .0 as usize;
            tcb.kernel_context.frame.set_stack_ptr(stack_top & !0xF);
            tcb.kernel_context.frame.set_instruction_ptr(switch_entry);
            tcb.kernel_context.initialized = true;
            Ok(())
        })
    }

    pub(crate) fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        let idx = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid))
            })
            .ok_or(KernelError::TaskMissing)?;

        let stack_base = KERNEL_STACK_REGION_BASE
            .checked_add(idx.saturating_mul(KERNEL_STACK_REGION_SIZE))
            .ok_or(KernelError::VmFull)?;
        let stack_top = stack_base
            .checked_add(KERNEL_STACK_REGION_SIZE)
            .ok_or(KernelError::VmFull)?;
        self.set_thread_kernel_stack(tid, stack_base, stack_top)?;

        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.kernel_context.frame.set_stack_ptr(stack_top & !0xF);
            tcb.kernel_context
                .frame
                .set_instruction_ptr(yarm_kernel_thread_switch_trampoline as *const () as usize);
            tcb.kernel_context.initialized = false;
            tcb.kernel_context.owns_stack = true;
            Ok(())
        })
    }

    pub(crate) fn release_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.kernel_context.stack_base = None;
            tcb.kernel_context.stack_top = None;
            tcb.kernel_context.frame = Default::default();
            tcb.kernel_context.initialized = false;
            tcb.kernel_context.owns_stack = false;
            Ok(())
        })
    }

    pub fn set_thread_user_context(
        &mut self,
        tid: u64,
        context: UserRegisterContext,
    ) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.user_context = context;
            Ok(())
        })
    }

    pub fn tls_restore_pending(&self, tid: u64) -> Option<bool> {
        let thread_id = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.tid)
        })?;
        Some(
            self.tls_restore_pending
                .iter()
                .flatten()
                .any(|pending_tid| *pending_tid == thread_id),
        )
    }

    pub fn take_tls_restore_request(&mut self, tid: u64) -> Result<Option<usize>, KernelError> {
        let idx = self
            .tls_restore_pending
            .iter()
            .position(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid));
        let Some(idx) = idx else {
            return Ok(None);
        };
        self.tls_restore_pending[idx] = None;
        Ok(self.thread_tls_base(tid))
    }

    pub fn mark_thread_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.detach_state = ThreadDetachState::Detached;
            Ok(())
        })
    }

    pub fn thread_detach_state(&self, tid: u64) -> Option<ThreadDetachState> {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.detach_state)
        })
    }

    pub fn join_thread(&mut self, tid: u64) -> Result<Option<u64>, KernelError> {
        let (detach_state, status) = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid)
                    .map(|tcb| (tcb.detach_state, tcb.status))
            })
            .ok_or(KernelError::TaskMissing)?;
        if detach_state == ThreadDetachState::Detached {
            return Err(KernelError::WrongObject);
        }
        let TaskStatus::Exited(exit_code) = status else {
            let current_tid = self.current_tid();
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                let joiner_pid = self
                    .process_id(joiner_tid)
                    .ok_or(KernelError::TaskMissing)?;
                let target_pid = self.process_id(tid).ok_or(KernelError::TaskMissing)?;
                if joiner_pid != target_pid {
                    return Err(KernelError::WrongObject);
                }
            }
            if let Some(joiner_tid) = current_tid.filter(|joiner| *joiner != tid) {
                self.with_tcbs_mut(|tcbs| {
                    let joiner = tcbs
                        .iter_mut()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == joiner_tid)
                        .ok_or(KernelError::TaskMissing)?;
                    joiner.status = TaskStatus::Blocked(WaitReason::Join(ThreadId(tid)));
                    Ok::<_, KernelError>(())
                })?;
                let _ = self.block_current_cpu();
                self.dispatch_next_task()?;
            }
            return Ok(None);
        };
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            if tcb.blocked_recv_state.take().is_some() {
                crate::yarm_log!("IPC_RECV_BLOCKED_STATE_CLEAR tid={} reason=cancel", tid);
            }
            tcb.status = TaskStatus::Dead;
            Ok::<_, KernelError>(())
        })?;
        Ok(Some(exit_code))
    }

    pub fn set_robust_futex_head(
        &mut self,
        tid: u64,
        head: usize,
        len: usize,
    ) -> Result<(), KernelError> {
        if head == 0 || len == 0 {
            return Err(KernelError::WrongObject);
        }
        self.with_tcbs(|tcbs| tcbs.iter().flatten().any(|tcb| tcb.tid.0 == tid))
            .then_some(())
            .ok_or(KernelError::TaskMissing)?;
        if let Some(slot) = self
            .robust_futex
            .iter_mut()
            .find(|slot| slot.is_some_and(|entry| entry.tid == ThreadId(tid)) || slot.is_none())
        {
            *slot = Some(super::RobustFutexRecord {
                tid: ThreadId(tid),
                state: RobustFutexState { head, len },
            });
            Ok(())
        } else {
            Err(KernelError::TaskTableFull)
        }
    }

    pub fn robust_futex_state(&self, tid: u64) -> Option<RobustFutexState> {
        self.robust_futex
            .iter()
            .flatten()
            .find(|entry| entry.tid.0 == tid)
            .map(|entry| entry.state)
    }

    pub(crate) fn sync_current_thread_from_frame(
        &mut self,
        frame: &TrapFrame,
    ) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.user_context = frame.capture_user_context();
            Ok(())
        })
    }

    fn apply_current_thread_to_frame(&mut self, frame: &mut TrapFrame) -> Result<(), KernelError> {
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        let context = self
            .thread_user_context(tid)
            .ok_or(KernelError::TaskMissing)?;
        frame.apply_user_context(context);
        Ok(())
    }

    pub fn resume_current_thread_with_frame(
        &mut self,
        frame: &mut TrapFrame,
    ) -> Result<Option<usize>, KernelError> {
        self.apply_current_thread_to_frame(frame)?;
        let tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        self.take_tls_restore_request(tid)
    }

    pub(crate) fn wake_joiners_for(&mut self, target_tid: u64) -> Result<u32, KernelError> {
        let wake_tids = self.with_tcbs_mut(|tcbs| {
            let mut wake_tids = [None; super::MAX_TASKS];
            let mut wake_count = 0usize;
            for tcb in tcbs.iter_mut().flatten() {
                if tcb.status != TaskStatus::Blocked(WaitReason::Join(ThreadId(target_tid))) {
                    continue;
                }
                tcb.status = TaskStatus::Runnable;
                if wake_count < wake_tids.len() {
                    wake_tids[wake_count] = Some(tcb.tid.0);
                    wake_count += 1;
                }
            }
            (wake_tids, wake_count)
        });
        let (wake_tids, wake_count) = wake_tids;
        for wake_tid in wake_tids.iter().take(wake_count).flatten() {
            self.enqueue_task(*wake_tid)?;
        }
        Ok(wake_count as u32)
    }

    pub(crate) fn reap_if_detached(&mut self, tid: u64) -> Result<(), KernelError> {
        let detached = self
            .thread_detach_state(tid)
            .ok_or(KernelError::TaskMissing)?
            == ThreadDetachState::Detached;
        if detached {
            self.mark_task_dead(tid)?;
        }
        Ok(())
    }

    pub fn set_thread_tls_base(&mut self, tid: u64, tls_base: usize) -> Result<(), KernelError> {
        if tls_base == 0 {
            return Err(KernelError::WrongObject);
        }
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.tls_ptr = Some(crate::kernel::vm::VirtAddr(tls_base as u64));
            Ok::<_, KernelError>(())
        })?;
        if let Some(slot) = self
            .tls_restore_pending
            .iter_mut()
            .find(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid) || slot.is_none())
        {
            *slot = Some(ThreadId(tid));
        }
        Ok(())
    }

    pub(crate) fn allocate_user_stack_with_guard(
        &mut self,
        tid: u64,
        stack_pages: usize,
    ) -> Result<crate::kernel::vm::VirtAddr, KernelError> {
        if stack_pages == 0 {
            return Err(KernelError::WrongObject);
        }
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        let stack_bytes = (stack_pages as u64)
            .checked_mul(crate::kernel::vm::PAGE_SIZE as u64)
            .ok_or(KernelError::WrongObject)?;
        let stride = USER_STACK_STRIDE_BYTES.max(stack_bytes + crate::kernel::vm::PAGE_SIZE as u64);
        // USER_STACK_TOP_BASE may be small on architectures with a narrow user
        // VA range (e.g. AArch64 prototype: 1 GB).  Dynamic TIDs (>= 10000) can
        // exceed the available slots if we multiply directly, causing checked_sub
        // to return None.  Wrap tid into the available slot count instead; the
        // per-address-space overlap check below catches any actual VA conflicts
        // within the same process.
        let max_slots = (USER_STACK_TOP_BASE / stride).max(1);
        let slot = tid % max_slots;
        let top = USER_STACK_TOP_BASE
            .checked_sub(slot.saturating_mul(stride))
            .ok_or(KernelError::WrongObject)?;
        let base = top
            .checked_sub(stack_bytes)
            .ok_or(KernelError::WrongObject)?;
        let guard = base
            .checked_sub(crate::kernel::vm::PAGE_SIZE as u64)
            .ok_or(KernelError::WrongObject)?;
        if top >= crate::kernel::vm::KERNEL_SPACE_BASE || guard == 0 {
            return Err(KernelError::WrongObject);
        }
        for page in (guard..top).step_by(crate::kernel::vm::PAGE_SIZE) {
            if self.with_user_spaces(|spaces| {
                spaces
                    .get(asid)
                    .and_then(|aspace| aspace.resolve(crate::kernel::vm::VirtAddr(page)))
                    .is_some()
            }) {
                return Err(KernelError::WrongObject);
            }
        }
        for page in (base..top).step_by(crate::kernel::vm::PAGE_SIZE) {
            let phys = crate::kernel::vm::PhysAddr(self.alloc_user_data_frame()?);
            self.map_user_page_in_asid_raw(
                asid,
                crate::kernel::vm::VirtAddr(page),
                crate::kernel::vm::Mapping {
                    phys,
                    flags: crate::kernel::vm::PageFlags::USER_RW,
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
        let guard_phys = crate::kernel::vm::PhysAddr(self.alloc_user_data_frame()?);
        self.map_user_page_in_asid_raw(
            asid,
            crate::kernel::vm::VirtAddr(guard),
            crate::kernel::vm::Mapping {
                phys: guard_phys,
                flags: crate::kernel::vm::PageFlags::GUARD,
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
        let stack_probe = crate::kernel::vm::VirtAddr(top - 8);
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
        Ok(crate::kernel::vm::VirtAddr(top))
    }

    pub fn spawn_user_thread(
        &mut self,
        parent_tid: u64,
        tls_base: usize,
        user_stack_top: usize,
        user_entry: usize,
    ) -> Result<u64, KernelError> {
        if tls_base == 0 || user_stack_top == 0 || user_entry == 0 || (user_stack_top & 0xF) != 0 {
            return Err(KernelError::WrongObject);
        }
        let parent = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == parent_tid)
                    .cloned()
            })
            .ok_or(KernelError::TaskMissing)?;
        let parent_class = self
            .task_class(parent_tid)
            .ok_or(KernelError::TaskMissing)?;
        // Staged brk ownership policy: brk bounds remain leader-owned and
        // per-task keyed; spawned threads do not get independent copied bounds.
        let tid = self.allocate_thread_id()?;
        self.register_task_with_class_in_process(tid, parent_class, parent.thread_group_id.0)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.thread_group_id = parent.thread_group_id;
            tcb.asid = parent.asid;
            tcb.tls_ptr = Some(crate::kernel::vm::VirtAddr(tls_base as u64));
            tcb.user_entry = Some(crate::kernel::vm::VirtAddr(user_entry as u64));
            tcb.user_stack_top = Some(crate::kernel::vm::VirtAddr(user_stack_top as u64));
            tcb.user_context = UserRegisterContext {
                instruction_ptr: crate::kernel::vm::VirtAddr(user_entry as u64),
                stack_ptr: crate::kernel::vm::VirtAddr(user_stack_top as u64),
                user_gprs: [0; 32],
                arg0: 0,
                arg1: 0,
                arg2: 0,
                arg3: 0,
                arg4: 0,
                arg5: 0,
            };
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        if let Some(slot) = self
            .tls_restore_pending
            .iter_mut()
            .find(|slot| slot.is_some_and(|pending_tid| pending_tid.0 == tid) || slot.is_none())
        {
            *slot = Some(ThreadId(tid));
        }
        let _ = self.enqueue_task(tid)?;
        Ok(tid)
    }

    pub fn fork_user_process_cow(&mut self, parent_tid: u64) -> Result<u64, KernelError> {
        let parent = self
            .with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == parent_tid)
                    .cloned()
            })
            .ok_or(KernelError::TaskMissing)?;
        let parent_class = self
            .task_class(parent_tid)
            .ok_or(KernelError::TaskMissing)?;
        let parent_asid = parent.asid.ok_or(KernelError::UserMemoryFault)?;
        let child_asid = self.clone_user_address_space_cow(parent_asid)?;

        let child_tid = self.allocate_thread_id()?;
        self.register_task_with_class(child_tid, parent_class)?;
        let child_cnode = self.task_cnode(child_tid).ok_or(KernelError::TaskMissing)?;
        self.set_process_cnode_for_pid(child_tid, child_cnode)?;
        self.inherit_parent_capabilities_for_fork(parent_tid, child_tid)?;
        self.with_tcbs_mut(|tcbs| {
            let child = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == child_tid)
                .ok_or(KernelError::TaskMissing)?;
            child.thread_group_id = ThreadGroupId(child_tid);
            child.asid = Some(child_asid);
            child.tls_ptr = parent.tls_ptr;
            child.user_entry = parent.user_entry;
            child.user_stack_top = parent.user_stack_top;
            // Fork child resumes with the same user register context as parent;
            // only the return register differs (`0` in the child).
            child.user_context = parent.user_context;
            child.user_context.arg0 = 0;
            child.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        if parent.tls_ptr.is_some()
            && let Some(slot) = self.tls_restore_pending.iter_mut().find(|slot| {
                slot.is_some_and(|pending_tid| pending_tid.0 == child_tid) || slot.is_none()
            })
        {
            *slot = Some(ThreadId(child_tid));
        }
        for slot in self.robust_futex.iter_mut() {
            if slot.is_some_and(|entry| entry.tid.0 == child_tid) {
                *slot = None;
            }
        }
        if let Some((base, end)) = self.task_brk_bounds(parent_tid) {
            self.set_task_brk_bounds(child_tid, base, end)?;
        }
        let _ = self.enqueue_task(child_tid)?;
        Ok(child_tid)
    }
}
