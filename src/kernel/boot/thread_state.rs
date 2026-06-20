// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::capabilities::CapObject;
use crate::kernel::ipc::ThreadId;
use crate::kernel::task::{
    KernelExecutionContext, RobustFutexState, TaskClass, TaskStatus, ThreadControlBlock,
    ThreadDetachState, ThreadGroupId, UserRegisterContext, WaitReason,
};
use crate::kernel::trapframe::TrapFrame;
use crate::kernel::vm::Asid;

pub(crate) const KERNEL_STACK_REGION_BASE: usize = 0xFFFF_8000_0000_0000;
/// Stage 134: increased from 0x4000 (16 KB) to 0x8000 (32 KB) per slot to
/// accommodate the handle_trap → syscall → spawn → create_user_space call
/// chain that overflowed a 16 KB stack by ~0x40 bytes (RSP descended to
/// 0xffff80000000bfc0, 0x40 below the old base 0xffff80000000c000).
pub(crate) const KERNEL_STACK_REGION_SIZE: usize = 0x8000;
/// Stage 134: one unmapped guard page at the bottom of every kernel-switch-
/// stack region.  `provision_default_kernel_context` sets stack_base =
/// region_base + KERNEL_STACK_GUARD_SIZE so the guard is never backed.
pub(crate) const KERNEL_STACK_GUARD_SIZE: usize = 0x1000;
const USER_STACK_STRIDE_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const USER_VIRT_TOP_EXCLUSIVE: u64 = 0x0000_8000_0000_0000;
#[cfg(not(target_arch = "x86_64"))]
const USER_VIRT_TOP_EXCLUSIVE: u64 = crate::kernel::vm::KERNEL_SPACE_BASE;
const USER_STACK_TOP_BASE: u64 = USER_VIRT_TOP_EXCLUSIVE - USER_STACK_STRIDE_BYTES;

#[cfg(all(target_arch = "x86_64", not(test)))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_kernel_thread_switch_trampoline
    .type yarm_kernel_thread_switch_trampoline, @function
yarm_kernel_thread_switch_trampoline:
    mov dx, 0x3f8
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov dx, 0x3f8
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov al, 0x41
    out dx, al
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov al, 0x4d
    out dx, al
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov al, 0x4a
    out dx, al
    jmp yarm_kernel_thread_switch_trampoline_rust_bridge

    .global yarm_kernel_thread_switch_trampoline_rust_bridge
    .type yarm_kernel_thread_switch_trampoline_rust_bridge, @function
yarm_kernel_thread_switch_trampoline_rust_bridge:
    mov dx, 0x3f8
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov al, 0x42
    out dx, al
    sub rsp, 8
    call yarm_kernel_thread_switch_trampoline_rust_real
    mov dx, 0x3f8
    mov al, 0x21
    out dx, al
    mov al, 0x52
    out dx, al
    mov al, 0x58
    out dx, al
1:
    cli
    hlt
    jmp 1b
"#
);

#[cfg(all(target_arch = "x86_64", not(test)))]
unsafe extern "C" {
    pub(crate) fn yarm_kernel_thread_switch_trampoline() -> !;
}

/// Returns the raw instruction-pointer value of `yarm_kernel_thread_switch_trampoline`,
/// used by the trap-entry stash drain to detect the first-resume path.
pub(crate) fn kernel_switch_frame_trampoline_ip() -> usize {
    yarm_kernel_thread_switch_trampoline as *const () as usize
}

/// Stage 125: the first-resume raw trampoline no longer jumps directly into a
/// normal Rust ABI function. The raw COM1 sequence emits `!R` at shim entry,
/// `!RA` at the former stack-adjust boundary, `!RM` where the removed Rust
/// marker bridge used to run, and `!RJ` immediately before jumping to the
/// x86_64 ABI bridge `yarm_kernel_thread_switch_trampoline_rust_bridge`. The
/// bridge emits `!RB`, subtracts 8 from the initialized `rsp % 16 == 8` shape so
/// the subsequent `call` enters Rust with SysV callee shape, and calls
/// `yarm_kernel_thread_switch_trampoline_rust_real`.
/// VALIDATION: D6_FIRST_RESUME_RUST_ENTER / !RM / !RJ / !RB
#[cfg(all(target_arch = "x86_64", test))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_kernel_thread_switch_trampoline() -> ! {
    yarm_kernel_thread_switch_trampoline_rust_real()
}

/// First-resume Rust handler. Entered only through the documented first-resume
/// entry path. On x86_64, `switch_frames` restores RIP to
/// `yarm_kernel_thread_switch_trampoline`; that raw shim emits `!R`, `!RA`,
/// `!RM`, and `!RJ`, then jumps to the assembly ABI bridge. The bridge emits
/// `!RB`, adjusts the stack for a normal SysV `call`, and calls this Rust real
/// handler. Non-x86_64 keeps the historical direct Rust entry and immediately
/// defers.
///
/// x86_64 ABI audit: `switch_frames` saves/restores `[rsp, rip, rbx, rbp,
/// r12..r15, fxsave]` in `ArchSwitchContext`. It enters the incoming frame with
/// `mov rsp, [next + 0]` and `jmp [next + 8]` (not `ret`). The initialized frame
/// reserves a fake return-address slot so the bridge starts at `rsp % 16 == 8`;
/// the bridge then uses `sub rsp, 8` before `call`, so this handler is entered
/// with normal SysV callee shape (`rsp % 16 == 8`). VALIDATION:
/// D6_FIRST_RESUME_RUST_ENTER
#[cfg_attr(
    target_arch = "x86_64",
    unsafe(export_name = "yarm_kernel_thread_switch_trampoline_rust_real")
)]
#[cfg_attr(
    not(target_arch = "x86_64"),
    unsafe(export_name = "yarm_kernel_thread_switch_trampoline_rust")
)]
pub extern "C" fn yarm_kernel_thread_switch_trampoline_rust_real() -> ! {
    #[cfg(not(target_arch = "x86_64"))]
    {
        crate::yarm_log!("D6_FIRST_RESUME_DEFERRED reason=non_x86_64_arch");
        loop {
            core::hint::spin_loop();
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        crate::yarm_log!("D6_FIRST_RESUME_RUST_ENTER");
        let stack_align = current_stack_alignment_for_diagnostics();
        crate::yarm_log!("D6_FIRST_RESUME_STACK_ALIGN value={}", stack_align);
        // Single-CPU precondition: the stash is always on CPU 0 (bootstrap CPU).
        let cpu_idx = crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize;
        // SAFETY: single CPU, interrupts disabled (trap path precondition for
        // can_stash_for_lock_drop), no concurrent accessor of FIRST_RESUME_STASH.
        let ctx = unsafe { crate::kernel::boot::FIRST_RESUME_STASH[cpu_idx].take() };
        let Some(ctx) = ctx else {
            crate::yarm_log!("D6_FIRST_RESUME_STASH_MISSING");
            crate::yarm_log!("D6_FIRST_RESUME_DEFERRED reason=stash_empty");
            loop {
                core::hint::spin_loop();
            }
        };
        crate::yarm_log!("D6_FIRST_RESUME_STASH_OK");
        crate::yarm_log!(
            "D6_FIRST_RESUME_ENTER tid={} cpu={}",
            ctx.incoming_tid,
            ctx.cpu_id.0
        );
        let Some(shared) = super::Bootstrap::shared_static_ref() else {
            crate::yarm_log!("D6_FIRST_RESUME_DEFERRED reason=shared_not_ready");
            loop {
                core::hint::spin_loop();
            }
        };
        crate::yarm_log!("D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN");
        let _ = shared.with_cpu(ctx.cpu_id, |kernel| {
            crate::yarm_log!("D6_FIRST_RESUME_LOCK_REACQUIRE_DONE");
            crate::yarm_log!("D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN");
            let r = crate::arch::trap_entry::post_switch_restore_arch_thread_state(
                kernel, ctx.cpu_id, None,
            );
            crate::yarm_log!("D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE");
            // Stage 139: capture hardware CR3 after post-switch restore so the
            // cleanup diagnostics can track any CR3 divergence introduced by
            // the proof's lock-drop switch.
            #[cfg(not(feature = "hosted-dev"))]
            {
                let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
                crate::yarm_log!("D6_PROOF_CR3_AFTER_FIRST_RESUME cr3=0x{:016x}", hw_cr3);
            }
            r
        });
        // Switch back to the outgoing task. In production, execution never returns
        // from switch_frames here — it jumps to the outgoing task's POINT 2.
        // In test builds (switch_frames is a no-op), we fall through to the spin.
        //
        // Pass ctx.outgoing_stack_top so TSS RSP0 is updated to the outgoing
        // task's (TID1's) kernel stack top. Without this, TSS RSP0 still points
        // to TID2's kernel stack top from the initial stash-drain switch, and any
        // interrupt that fires while TID1 is in user mode would push its frame
        // onto TID2's kernel stack — a stack-corruption bug.
        crate::arch::selected_isa::context_switch::switch_frames(
            // SAFETY: incoming_frame_ptr is stable (KernelState::tcbs fixed-size
            // array); no concurrent access (single CPU, interrupts disabled).
            unsafe { &mut *ctx.incoming_frame_ptr },
            unsafe { &*ctx.outgoing_frame_ptr },
            ctx.outgoing_stack_top,
        );
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn current_stack_alignment_for_diagnostics() -> usize {
    let rsp: usize;
    // SAFETY: read-only diagnostic snapshot of the architectural stack pointer.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }
    rsp & 0xF
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

    /// Stage 126/127/128 kernel switch-stack invariant gate.
    ///
    /// `incoming_stack_top`/`stack_top` values are virtual kernel stack tops in
    /// the fixed higher-half kernel-stack arena, not physical addresses. On
    /// x86_64 the Stage 125 bridge performs `sub rsp, 8; call rust_real`, so the
    /// page below the aligned top must cover the fake return slot (`top - 8`),
    /// the bridge alignment slot (`top - 16`), and the observed call-push write
    /// (`top - 24`, 0xffff800000007fe8 when top is 0xffff800000008000). Before
    /// publishing `kernel_context.initialized = true`, ensure that page is
    /// present, writable, supervisor/kernel-only (not user), and mapped into the
    /// target task ASID/root that owns the first-resume context.
    ///
    /// Stage 127 deliberately avoids active-ASID enumeration as the terminal
    /// gate: early supervisor/init spawn can initialize a target task before any
    /// ASID is currently running, but the target task root is still the correct
    /// initial mapping authority once `task_asid(tid)` is bound. Stage 128 adds
    /// the stronger CR3 coverage invariant: `switch_frames` is only a kernel
    /// stack/register switch and does not switch CR3, so the incoming stack page
    /// must also be installed as a kernel-shared mapping in every existing task
    /// root that may be the active/outgoing CR3 when the bridge uses that stack.
    #[cfg(all(target_arch = "x86_64", not(test)))]
    fn ensure_kernel_switch_stack_mapped(
        &mut self,
        tid: u64,
        stack_base: usize,
        stack_top: usize,
    ) -> Result<(), KernelError> {
        use crate::arch::selected_isa::page_table::{self, PageTableEntry};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        fn validate_entry(entry: page_table::PageTableEntry) -> bool {
            (entry.0 & PageTableEntry::WRITABLE) != 0 && (entry.0 & PageTableEntry::USER) == 0
        }

        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_CHECK_BEGIN tid={} top=0x{:x}",
            tid,
            stack_top
        );

        let aligned_top = stack_top & !0xF;
        let fake_return_probe = aligned_top
            .checked_sub(core::mem::size_of::<usize>())
            .ok_or(KernelError::WrongObject)?;
        let bridge_slot_probe = aligned_top
            .checked_sub(2 * core::mem::size_of::<usize>())
            .ok_or(KernelError::WrongObject)?;
        let call_push_probe = aligned_top
            .checked_sub(3 * core::mem::size_of::<usize>())
            .ok_or(KernelError::WrongObject)?;
        let probe_page = fake_return_probe & !(PAGE_SIZE - 1);

        if stack_base == 0
            || stack_base >= stack_top
            || probe_page < stack_base
            || call_push_probe < stack_base
            || fake_return_probe >= stack_top
            || bridge_slot_probe >= stack_top
        {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason=stack_bounds",
                tid,
                fake_return_probe
            );
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=stack_bounds tid={}",
                tid
            );
            return Err(KernelError::WrongObject);
        }

        let Some(target_asid) = self.task_asid(tid) else {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason=target_asid_unavailable",
                tid,
                fake_return_probe
            );
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=target_asid_unavailable tid={}",
                tid
            );
            return Err(KernelError::UserMemoryFault);
        };
        if self.with_user_spaces(|spaces| spaces.get(target_asid).is_none()) {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason=target_root_unavailable",
                tid,
                fake_return_probe
            );
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=target_root_unavailable tid={}",
                tid
            );
            return Err(KernelError::VmFull);
        }

        let stack_page = VirtAddr(probe_page as u64);
        let phys = if let Some(entry) = page_table::resolve_page(target_asid, stack_page) {
            if !validate_entry(entry) {
                let reason = if (entry.0 & PageTableEntry::WRITABLE) == 0 {
                    "not_writable"
                } else {
                    "user_accessible"
                };
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason={}",
                    tid,
                    fake_return_probe,
                    reason
                );
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason={} tid={}",
                    reason,
                    tid
                );
                return Err(KernelError::VmFull);
            }
            entry.addr()
        } else {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_BEGIN tid={} asid={} va=0x{:x}",
                tid,
                target_asid.0,
                probe_page
            );
            let phys = self.alloc_user_data_frame()?;
            page_table::map_page(
                target_asid,
                stack_page,
                PhysAddr(phys),
                PageFlags::KERNEL_RW,
            )
            .map_err(|_| KernelError::VmFull)?;
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DONE tid={} asid={} va=0x{:x}",
                tid,
                target_asid.0,
                probe_page
            );
            phys
        };

        // Stage 128: because `switch_frames` does not switch CR3, an incoming
        // stack may be used while the outgoing task's root is still active.
        // Install the same supervisor-only backing page in every currently
        // existing task root (plus the target root) instead of relying on a
        // target-ASID-only mapping. This is intentionally narrow: one page, not
        // the full kernel-stack arena.
        let mut roots = [None; super::MAX_TASKS];
        roots[0] = Some(target_asid);
        self.with_tcbs(|tcbs| {
            let mut len = 1usize;
            for tcb in tcbs.iter().flatten() {
                let Some(asid) = tcb.asid else {
                    continue;
                };
                if self.with_user_spaces(|spaces| spaces.get(asid).is_none()) {
                    continue;
                }
                if roots[..len].iter().any(|entry| *entry == Some(asid)) {
                    continue;
                }
                if len < roots.len() {
                    roots[len] = Some(asid);
                    len += 1;
                }
            }
        });

        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_MAP_SHARED_BEGIN tid={} va=0x{:x}",
            tid,
            probe_page
        );
        for asid in roots.iter().flatten().copied() {
            let result = match page_table::resolve_page(asid, stack_page) {
                Some(entry) if entry.addr() == phys && validate_entry(entry) => "already_ok",
                Some(_) => {
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_STACK_MAP_SHARED_ROOT tid={} asid={} va=0x{:x} result=conflict",
                        tid,
                        asid.0,
                        probe_page
                    );
                    crate::yarm_log!(
                        "D6_KERNEL_SWITCH_STACK_MAP_SHARED_DEFERRED reason=shared_root_conflict tid={}",
                        tid
                    );
                    return Err(KernelError::VmFull);
                }
                None => {
                    page_table::map_page(asid, stack_page, PhysAddr(phys), PageFlags::KERNEL_RW)
                        .map_err(|_| KernelError::VmFull)?;
                    "mapped"
                }
            };
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_SHARED_ROOT tid={} asid={} va=0x{:x} result={}",
                tid,
                asid.0,
                probe_page,
                result
            );
        }
        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_MAP_SHARED_DONE tid={} va=0x{:x}",
            tid,
            probe_page
        );

        let Some(entry) = page_table::resolve_page(target_asid, stack_page) else {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason=resolve_after_map_failed",
                tid,
                fake_return_probe
            );
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=resolve_after_map_failed tid={}",
                tid
            );
            return Err(KernelError::VmFull);
        };
        if entry.addr() != phys || !validate_entry(entry) {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_CHECK_FAILED tid={} probe=0x{:x} reason=mapped_flags_invalid",
                tid,
                fake_return_probe
            );
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_MAP_DEFERRED reason=mapped_flags_invalid tid={}",
                tid
            );
            return Err(KernelError::VmFull);
        }

        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_CHECK_OK tid={} probe=0x{:x}",
            tid,
            fake_return_probe
        );
        Ok(())
    }

    /// Stage 128/129 proof-time active-root guard with on-demand repair.
    ///
    /// `switch_frames` switches callee-saved registers and the kernel stack; it
    /// does not switch CR3. The Stage 120 proof therefore checks the incoming
    /// switch-stack page against `hal.active_asid()` before dropping the global
    /// lock, proving the stack is visible in the root that is active at the
    /// bridge `callq` return-address push.
    ///
    /// Stage 129: when the active/outgoing ASID does not have the page mapped
    /// (e.g., because it was created after `ensure_kernel_switch_stack_mapped`
    /// ran its shared-root loop), attempt a direct page-table repair using the
    /// physical frame already installed in the target ASID. This bypasses user
    /// VM-region capacity accounting because kernel-half switch-stack pages are
    /// not user-space VM regions.
    #[cfg(all(target_arch = "x86_64", not(test)))]
    pub(crate) fn ensure_active_root_can_use_kernel_switch_stack(
        &mut self,
        tid: u64,
    ) -> Result<(), KernelError> {
        use core::sync::atomic::Ordering;

        use crate::arch::selected_isa::page_table::{self, PageTableEntry, PageTableError};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        // One-shot flag: if a prior repair attempt failed permanently (capacity
        // or invalid-address error), skip the repair on subsequent proof calls to
        // avoid spamming the log.  Success resets nothing — the page stays mapped,
        // so future calls see ACTIVE_CHECK_OK before reaching this flag check.
        #[cfg(all(target_arch = "x86_64", not(test)))]
        static ACTIVE_ROOT_REPAIR_FAILED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);

        let active_asid = self.hal.active_asid();
        let cr3 = active_asid.and_then(page_table::cr3_for_asid).unwrap_or(0);
        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_ACTIVE_ROOT cpu={} active_asid={} cr3=0x{:x}",
            self.current_cpu().0,
            active_asid.map_or(0, |asid| asid.0),
            cr3
        );
        let Some(active_asid) = active_asid else {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid={} active_asid=0 probe=0x0 reason=active_asid_unavailable",
                tid
            );
            return Err(KernelError::UserMemoryFault);
        };
        let (stack_base, stack_top) = self.with_tcbs(|tcbs| {
            let tcb = tcbs
                .iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let stack_base = tcb
                .kernel_context
                .stack_base
                .ok_or(KernelError::WrongObject)?
                .0;
            let stack_top = tcb
                .kernel_context
                .stack_top
                .ok_or(KernelError::WrongObject)?
                .0;
            Ok::<_, KernelError>((stack_base as usize, stack_top as usize))
        })?;
        let aligned_top = stack_top & !0xF;
        let fake_return_probe = aligned_top
            .checked_sub(core::mem::size_of::<usize>())
            .ok_or(KernelError::WrongObject)?;
        let probe_page = fake_return_probe & !(PAGE_SIZE - 1);
        if probe_page < stack_base {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid={} active_asid={} probe=0x{:x} reason=stack_bounds",
                tid,
                active_asid.0,
                fake_return_probe
            );
            return Err(KernelError::WrongObject);
        }
        let stack_page = VirtAddr(probe_page as u64);

        // --- Check whether the page is already correctly mapped. --------------
        match page_table::resolve_page(active_asid, stack_page) {
            Some(entry)
                if (entry.0 & PageTableEntry::WRITABLE) != 0
                    && (entry.0 & PageTableEntry::USER) == 0 =>
            {
                // Already mapped with correct kernel-only writable flags.
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_OK tid={} active_asid={} probe=0x{:x}",
                    tid,
                    active_asid.0,
                    fake_return_probe
                );
                return Ok(());
            }
            Some(entry) => {
                // Page exists but flags are wrong: user-accessible or not writable.
                // Reject — do not overwrite a mapping with unexpected permissions.
                let reason = if (entry.0 & PageTableEntry::USER) != 0 {
                    "user_accessible"
                } else {
                    "not_writable"
                };
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid={} active_asid={} probe=0x{:x} reason={}",
                    tid,
                    active_asid.0,
                    fake_return_probe,
                    reason
                );
                return Err(KernelError::VmFull);
            }
            None => {
                // Not mapped in the active ASID.  Stage 129: attempt repair.
            }
        }

        // --- Stage 129: active-root repair. ----------------------------------
        // The page is missing from ASID `active_asid`.  This happens when ASID
        // `active_asid` was created after `ensure_kernel_switch_stack_mapped`
        // ran its shared-root loop for `tid`, so the loop never included it.
        //
        // Obtain the physical frame address from the target ASID (the incoming
        // task's own root, which was the mapping authority at init time) and
        // install it directly in `active_asid`'s page tables.  This is a direct
        // page-table write — no user VM-region capacity accounting is involved.

        if ACTIVE_ROOT_REPAIR_FAILED.load(Ordering::Relaxed) {
            // A prior repair attempt for this session failed permanently.
            // Return the same error without re-logging to avoid log spam.
            return Err(KernelError::VmFull);
        }

        // Get the target ASID (incoming task's address space).
        let target_asid = match self.task_asid(tid) {
            Some(asid) => asid,
            None => {
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid={} active_asid={} va=0x{:x} reason=target_asid_missing",
                    tid,
                    active_asid.0,
                    probe_page
                );
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED reason=target_asid_missing tid={} active_asid={}",
                    tid,
                    active_asid.0
                );
                ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
                return Err(KernelError::UserMemoryFault);
            }
        };

        // Resolve the physical address from the target ASID's page table.
        let phys = match page_table::resolve_page(target_asid, stack_page) {
            Some(e)
                if (e.0 & PageTableEntry::WRITABLE) != 0 && (e.0 & PageTableEntry::USER) == 0 =>
            {
                e.addr()
            }
            Some(e) => {
                let reason = if (e.0 & PageTableEntry::USER) != 0 {
                    "user_vm_capacity"
                } else {
                    "target_not_writable"
                };
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid={} active_asid={} va=0x{:x} reason={}",
                    tid,
                    active_asid.0,
                    probe_page,
                    reason
                );
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED reason={} tid={} active_asid={}",
                    reason,
                    tid,
                    active_asid.0
                );
                ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
                return Err(KernelError::VmFull);
            }
            None => {
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid={} active_asid={} va=0x{:x} reason=target_not_mapped",
                    tid,
                    active_asid.0,
                    probe_page
                );
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED reason=target_not_mapped tid={} active_asid={}",
                    tid,
                    active_asid.0
                );
                ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
                return Err(KernelError::VmFull);
            }
        };

        // Map the exact page containing stack_top - 8 into the active ASID.
        // Flags: supervisor (kernel-only), writable, not user-accessible.
        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_BEGIN tid={} active_asid={} va=0x{:x}",
            tid,
            active_asid.0,
            probe_page
        );
        match page_table::map_page(
            active_asid,
            stack_page,
            PhysAddr(phys),
            PageFlags::KERNEL_RW,
        ) {
            Ok(_) => {}
            Err(err) => {
                let reason = match err {
                    PageTableError::OutOfMemory => "page_table_capacity",
                    PageTableError::InvalidAddress => "page_table_invalid_addr",
                };
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_FAILED tid={} active_asid={} va=0x{:x} reason={}",
                    tid,
                    active_asid.0,
                    probe_page,
                    reason
                );
                crate::yarm_log!(
                    "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DEFERRED reason={} tid={} active_asid={}",
                    reason,
                    tid,
                    active_asid.0
                );
                ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
                return Err(KernelError::VmFull);
            }
        }
        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_MAP_ACTIVE_DONE tid={} active_asid={} va=0x{:x}",
            tid,
            active_asid.0,
            probe_page
        );

        // Verify the repair: re-resolve and confirm supervisor-only writable flags.
        let Some(entry) = page_table::resolve_page(active_asid, stack_page) else {
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid={} active_asid={} probe=0x{:x} reason=verify_after_map_failed",
                tid,
                active_asid.0,
                fake_return_probe
            );
            ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
            return Err(KernelError::VmFull);
        };
        if (entry.0 & PageTableEntry::WRITABLE) == 0 || (entry.0 & PageTableEntry::USER) != 0 {
            let reason = if (entry.0 & PageTableEntry::USER) != 0 {
                "user_accessible"
            } else {
                "not_writable"
            };
            crate::yarm_log!(
                "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_FAILED tid={} active_asid={} probe=0x{:x} reason={}",
                tid,
                active_asid.0,
                fake_return_probe,
                reason
            );
            ACTIVE_ROOT_REPAIR_FAILED.store(true, Ordering::Relaxed);
            return Err(KernelError::VmFull);
        }

        crate::yarm_log!(
            "D6_KERNEL_SWITCH_STACK_ACTIVE_CHECK_OK tid={} active_asid={} probe=0x{:x}",
            tid,
            active_asid.0,
            fake_return_probe
        );
        Ok(())
    }

    #[cfg(any(not(target_arch = "x86_64"), test))]
    fn ensure_kernel_switch_stack_mapped(
        &mut self,
        _tid: u64,
        _stack_base: usize,
        _stack_top: usize,
    ) -> Result<(), KernelError> {
        Ok(())
    }

    #[cfg(any(not(target_arch = "x86_64"), test))]
    pub(crate) fn ensure_active_root_can_use_kernel_switch_stack(
        &mut self,
        _tid: u64,
    ) -> Result<(), KernelError> {
        Ok(())
    }

    /// Stage 132: map ALL kernel-switch-stack pages (stack_base..stack_top) for
    /// a proof task.  `ensure_kernel_switch_stack_mapped` (Stage 127) maps only
    /// the top page.  After the D6 proof handoff, TSS RSP0 points to stack_top,
    /// and the first kernel trap handler grows ~9 KB deep — well below the single
    /// mapped page — causing a #PF (write to unmapped kernel stack).  This
    /// function closes that gap by allocating and sharing every page in the full
    /// stack range WITHOUT touching `ensure_kernel_switch_stack_mapped` and
    /// without using the region-size constant (preserving Stage 127–129 invariants).
    #[cfg(all(target_arch = "x86_64", not(test)))]
    pub(crate) fn d6_ensure_full_proof_switch_stack_mapped(
        &mut self,
        tid: u64,
    ) -> Result<(), KernelError> {
        use crate::arch::selected_isa::page_table::{self, PageTableEntry};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        fn validate_entry(entry: PageTableEntry) -> bool {
            (entry.0 & PageTableEntry::WRITABLE) != 0 && (entry.0 & PageTableEntry::USER) == 0
        }

        let (stack_base, stack_top) = self.with_tcbs(|tcbs| {
            let tcb = tcbs
                .iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let stack_base = tcb
                .kernel_context
                .stack_base
                .ok_or(KernelError::WrongObject)?
                .0 as usize;
            let stack_top = tcb
                .kernel_context
                .stack_top
                .ok_or(KernelError::WrongObject)?
                .0 as usize;
            Ok::<_, KernelError>((stack_base, stack_top))
        })?;

        if stack_base == 0 || stack_base >= stack_top {
            return Err(KernelError::WrongObject);
        }

        let Some(target_asid) = self.task_asid(tid) else {
            return Err(KernelError::UserMemoryFault);
        };

        // Collect all ASIDs before the allocation loop so &mut self is free for
        // alloc_user_data_frame without nested borrow conflicts.
        let mut roots = [None; super::MAX_TASKS];
        roots[0] = Some(target_asid);
        self.with_tcbs(|tcbs| {
            let mut len = 1usize;
            for tcb in tcbs.iter().flatten() {
                let Some(asid) = tcb.asid else {
                    continue;
                };
                if self.with_user_spaces(|spaces| spaces.get(asid).is_none()) {
                    continue;
                }
                if roots[..len].iter().any(|e| *e == Some(asid)) {
                    continue;
                }
                if len < roots.len() {
                    roots[len] = Some(asid);
                    len += 1;
                }
            }
        });

        crate::yarm_log!(
            "D6_PROOF_FULL_STACK_MAP_BEGIN tid={} base=0x{:x} top=0x{:x}",
            tid,
            stack_base,
            stack_top
        );

        let mut page_addr = stack_base & !(PAGE_SIZE - 1);
        while page_addr < stack_top {
            let stack_page = VirtAddr(page_addr as u64);
            let phys = if let Some(entry) = page_table::resolve_page(target_asid, stack_page) {
                if validate_entry(entry) {
                    crate::yarm_log!(
                        "D6_PROOF_FULL_STACK_MAP_SKIP tid={} va=0x{:x}",
                        tid,
                        page_addr
                    );
                    page_addr = page_addr.saturating_add(PAGE_SIZE);
                    continue;
                }
                return Err(KernelError::VmFull);
            } else {
                let phys = self.alloc_user_data_frame()?;
                page_table::map_page(
                    target_asid,
                    stack_page,
                    PhysAddr(phys),
                    PageFlags::KERNEL_RW,
                )
                .map_err(|_| KernelError::VmFull)?;
                phys
            };
            for asid in roots.iter().flatten().copied() {
                if asid == target_asid {
                    continue;
                }
                match page_table::resolve_page(asid, stack_page) {
                    Some(e) if e.addr() == phys && validate_entry(e) => {}
                    None => {
                        page_table::map_page(
                            asid,
                            stack_page,
                            PhysAddr(phys),
                            PageFlags::KERNEL_RW,
                        )
                        .map_err(|_| KernelError::VmFull)?;
                    }
                    _ => return Err(KernelError::VmFull),
                }
            }
            crate::yarm_log!(
                "D6_PROOF_FULL_STACK_MAP_PAGE_MAPPED tid={} va=0x{:x}",
                tid,
                page_addr
            );
            page_addr = page_addr.saturating_add(PAGE_SIZE);
        }

        crate::yarm_log!("D6_PROOF_FULL_STACK_MAP_DONE tid={}", tid);
        Ok(())
    }

    #[cfg(any(not(target_arch = "x86_64"), test))]
    pub(crate) fn d6_ensure_full_proof_switch_stack_mapped(
        &mut self,
        _tid: u64,
    ) -> Result<(), KernelError> {
        Ok(())
    }

    pub fn initialize_thread_kernel_switch_frame(
        &mut self,
        tid: u64,
        switch_entry: usize,
    ) -> Result<(), KernelError> {
        if switch_entry == 0 {
            return Err(KernelError::WrongObject);
        }
        let (stack_base, stack_top) = self.with_tcbs(|tcbs| {
            let tcb = tcbs
                .iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let stack_base = tcb
                .kernel_context
                .stack_base
                .ok_or(KernelError::WrongObject)?
                .0 as usize;
            let stack_top = tcb
                .kernel_context
                .stack_top
                .ok_or(KernelError::WrongObject)?
                .0 as usize;
            Ok((stack_base, stack_top))
        })?;
        self.ensure_kernel_switch_stack_mapped(tid, stack_base, stack_top)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            // Stage 121 x86_64 first-resume ABI audit: switch_frames enters the
            // initialized frame with a jump, not a call/ret. A normal SysV Rust
            // function entry still expects `rsp % 16 == 8`, so reserve one
            // fake-return-address slot below the 16-byte-aligned stack top on
            // x86_64. Stage 126 additionally requires the page containing the
            // fake slot (`stack_top - 8`) and bridge call-push area
            // (`stack_top - 16` and the observed `stack_top - 24` push to
            // 0xffff800000007fe8 for top 0xffff800000008000) to be backed and
            // supervisor-writable before `initialized = true` is published.
            #[cfg(target_arch = "x86_64")]
            let entry_stack_ptr = (stack_top & !0xF).saturating_sub(core::mem::size_of::<usize>());
            #[cfg(not(target_arch = "x86_64"))]
            let entry_stack_ptr = stack_top & !0xF;
            tcb.kernel_context.frame.set_stack_ptr(entry_stack_ptr);
            tcb.kernel_context.frame.set_instruction_ptr(switch_entry);
            // Stage 131: initialise the fxsave area with a valid FPU state so
            // `fxrstor` on first switch does not load MXCSR=0 (all SSE exceptions
            // unmasked). All-zero fxsave is an invalid state: MXCSR=0 disables every
            // SSE exception mask, causing #XF on the next SSE operation in kernel
            // code (including format-string helpers compiled with SSE intrinsics).
            // `initialize_frame_fpu_state` runs `fninit; fxsave` to capture the
            // current valid state (MXCSR=0x1F80, x87 CW=0x037F).
            #[cfg(target_arch = "x86_64")]
            crate::arch::selected_isa::context_switch::initialize_frame_fpu_state(
                &mut tcb.kernel_context.frame,
            );
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

        // Stage 134: compute region_base separately so the guard page offset
        // (KERNEL_STACK_GUARD_SIZE) can be applied.  The region layout is:
        //   [region_base,  region_base + GUARD)  → unmapped guard page
        //   [region_base + GUARD, region_base + REGION_SIZE)  → mapped stack
        let region_base = KERNEL_STACK_REGION_BASE
            .checked_add(idx.saturating_mul(KERNEL_STACK_REGION_SIZE))
            .ok_or(KernelError::VmFull)?;
        let stack_base = region_base
            .checked_add(KERNEL_STACK_GUARD_SIZE)
            .ok_or(KernelError::VmFull)?;
        let stack_top = region_base
            .checked_add(KERNEL_STACK_REGION_SIZE)
            .ok_or(KernelError::VmFull)?;
        self.set_thread_kernel_stack(tid, stack_base, stack_top)?;
        crate::yarm_log!(
            "KERNEL_STACK_RANGE tid={} base=0x{:x} top=0x{:x}",
            tid,
            stack_base,
            stack_top
        );

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
        // Delegate full cleanup to mark_task_dead: it sets Dead status, revokes
        // reply caps, releases the kernel context, and triggers process-cnode
        // cleanup once all threads in the group are Dead.
        self.mark_task_dead(tid)?;
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

        // All steps below must destroy child_asid on failure to prevent leaking
        // the cloned address space when post-clone task setup fails.
        let result = self.fork_complete_post_clone(parent, parent_class, child_asid, parent_tid);
        if result.is_err() {
            let _ = self.destroy_user_address_space_by_asid(child_asid);
        }
        result
    }

    fn fork_complete_post_clone(
        &mut self,
        parent: ThreadControlBlock,
        parent_class: TaskClass,
        child_asid: Asid,
        parent_tid: u64,
    ) -> Result<u64, KernelError> {
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
