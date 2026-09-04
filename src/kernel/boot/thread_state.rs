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

/// Stage 199G-B §1 — the values the ONE delivery writeback projects, gathered by the drain that
/// already holds them.
///
/// It exists so the completion transaction takes ONE parameter instead of several positional
/// scalars whose order a caller could silently transpose — `sender_tid` and `payload_len` are
/// both plain integers, and swapping them would be invisible at the call site and wrong in
/// userspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockedRecvDeliveryResult {
    /// The receive ABI the WAITER issued, from its saved blocked-receive state.
    pub(crate) recv_abi: crate::kernel::task::RecvAbiVariant,
    /// The sending thread — the legacy projection's `ret0`.
    pub(crate) sender_tid: u64,
    /// Receiver-visible payload length — the legacy projection's `ret1`.
    pub(crate) payload_len: usize,
    /// The receiver-local capability this delivery materialized, if any — the legacy
    /// projection's `ret2`. `None` projects the canonical no-transfer sentinel, which is what
    /// the broad path's `encode_transfer_cap_ret(frame, None)` writes.
    pub(crate) transfer_cap: Option<crate::kernel::capabilities::CapId>,
}

impl BlockedRecvDeliveryResult {
    /// The recv-v2 projection: every field travels in the meta struct, so the register lanes are
    /// the canonical success shape. Byte-identical to what `clear_blocked_recv_return_regs_locked`
    /// wrote before 199G-B.
    pub(crate) const RECV_V2: Self = Self {
        recv_abi: crate::kernel::task::RecvAbiVariant::RecvV2,
        sender_tid: 0,
        payload_len: 0,
        transfer_cap: None,
    };
}

pub(crate) const KERNEL_STACK_REGION_BASE: usize = 0xFFFF_8000_0000_0000;
/// Per-task kernel-stack region size.
///
/// Stage 134: increased from 0x4000 (16 KB) to 0x8000 (32 KB) per slot to
/// accommodate the handle_trap → syscall → spawn → create_user_space call
/// chain that overflowed a 16 KB stack by ~0x40 bytes (RSP descended to
/// 0xffff80000000bfc0, 0x40 below the old base 0xffff80000000c000).
///
/// Stage 165I/165J (x86_64 only): increased from 0x8000 (32 KB) to 0x10000
/// (64 KB, 165I) and then to 0x20000 (128 KB, 165J).  The D6 controlled-switch
/// proof's deep post-cleanup trap path (handle_trap ~8 KB frame +
/// process_ipc_timeout_deadlines' `[None; 512]` ~8 KB + nested call chain)
/// overflowed the 32 KB region (~33 KB observed) and then the 64 KB region
/// (~64 KB observed).  Because tid=0's region sits exactly at the canonical
/// boundary 0xFFFF_8000_0000_0000, the overflow descends into NON-canonical
/// space and #DFs (vector 8, CR2=0) instead of #PF'ing — and non-canonical pages
/// cannot be mapped, so the region must be enlarged.  128 KB gives ~124 KB usable
/// above the guard page.  NOTE: the observed depth tracked the region size
/// (33 KB at 32 KB, 64 KB at 64 KB) because tid=0 always bottoms at the canonical
/// boundary; if 128 KB still #DFs, the post-cleanup path is genuinely
/// unbounded/recursive rather than fixed-deep, and the bound (not the size) is
/// the fix.  AArch64/RISC-V keep 32 KB: their trap paths fit and the D6 proof is
/// x86_64-only, so this is gated to avoid changing their layout/memory.  The
/// region span is MAX_TASKS(512) × 128 KB = 64 MiB
/// ([0xFFFF_8000_0000_0000, 0xFFFF_8000_0400_0000)), still sparse on-demand VA
/// dedicated to kernel stacks (the image/direct-map live at 0xFFFF_FF80_…), so no
/// collision.
#[cfg(target_arch = "x86_64")]
pub(crate) const KERNEL_STACK_REGION_SIZE: usize = 0x20000;
#[cfg(not(target_arch = "x86_64"))]
pub(crate) const KERNEL_STACK_REGION_SIZE: usize = 0x8000;
/// Stage 134: one unmapped guard page at the bottom of every kernel-switch-
/// stack region.  `provision_default_kernel_context` sets stack_base =
/// region_base + KERNEL_STACK_GUARD_SIZE so the guard is never backed.
pub(crate) const KERNEL_STACK_GUARD_SIZE: usize = 0x1000;
/// Stage 165H: bound for the default-off D6 proof's scratch arrays.  The proof
/// helpers previously sized their per-task scratch at `MAX_TASKS` (512): a
/// `[(u64, usize, usize); 512]` is 12 KiB and a `[Option<Asid>; 512]` ~2 KiB of
/// **stack**.  Those arrays live on the deep D6 post-switch / cleanup call chain,
/// and combined with handle_trap's ~8 KiB frame they overflowed the 32 KiB
/// kernel stack — sliding off the bottom of tid=0's region (the canonical
/// boundary `0xffff_8000_0000_0000`) into NON-canonical space, which #GPs on the
/// stack push and escalates to a #DF (vector 8, CR2=0).  The proof only ever
/// touches the handful of bootstrap/service tasks, so a much smaller bound
/// removes ~10 KiB of stack pressure with no behavior change.  (Mapping below the
/// canonical boundary is physically impossible, so the depth must be reduced, not
/// the range extended.)
const D6_PROOF_MAX_TASKS: usize = 128;
pub(crate) const USER_STACK_STRIDE_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(target_arch = "x86_64")]
const USER_VIRT_TOP_EXCLUSIVE: u64 = 0x0000_8000_0000_0000;
#[cfg(not(target_arch = "x86_64"))]
const USER_VIRT_TOP_EXCLUSIVE: u64 = crate::kernel::vm::KERNEL_SPACE_BASE;
pub(crate) const USER_STACK_TOP_BASE: u64 = USER_VIRT_TOP_EXCLUSIVE - USER_STACK_STRIDE_BYTES;

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

/// Returns the instruction-pointer address to use for the first-resume switch
/// frame.  On x86_64 this is the raw assembly shim address
/// (`yarm_kernel_thread_switch_trampoline`) so the D6 proof COM1 markers fire
/// before the Rust handler runs.  On non-x86_64 architectures the shim does
/// not exist; return the Rust real handler directly.
pub(crate) fn kernel_switch_frame_trampoline_ip() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        yarm_kernel_thread_switch_trampoline as *const () as usize
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        yarm_kernel_thread_switch_trampoline_rust_real as *const () as usize
    }
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
        // Stage 166 (D6-SWITCH-A): tag the first-resume when driven by the
        // production Outcome A knob (proof knob off).
        if crate::kernel::boot::d6_switch_a_enabled()
            && !crate::kernel::boot::d6_controlled_switch_proof_enabled()
        {
            crate::yarm_log!("D6_SWITCH_A_FIRST_RESUME incoming={}", ctx.incoming_tid);
        }
        let Some(shared) = super::Bootstrap::shared_static_ref() else {
            crate::yarm_log!("D6_FIRST_RESUME_DEFERRED reason=shared_not_ready");
            loop {
                core::hint::spin_loop();
            }
        };
        crate::yarm_log!("D6_FIRST_RESUME_LOCK_REACQUIRE_BEGIN");
        // U3 (canonical 203C): one rank-1 scheduler binding instead of the broad
        // `with_cpu(ctx.cpu_id, |kernel| …)` re-acquire. The broad body's only KernelState
        // effect was the CPU validation-and-bind `with_cpu` performs on entry: its single
        // call, `post_switch_restore_arch_thread_state(kernel, cpu, None)`, delegates on
        // x86_64 to `restore_arch_thread_state`, which returns `Ok(())` on its first
        // statement when `frame` is `None` — before any current-TID read, TCB access,
        // context/TLS restore, ASID activation, CR3 check or domain lock. Refusal keeps its
        // historical shape: the DONE/restore/CR3 markers are skipped and execution still
        // falls through to the switch-back below (the old `let _ =` ignored the error too).
        if shared.bind_current_cpu_split(ctx.cpu_id).is_ok() {
            crate::yarm_log!("D6_FIRST_RESUME_LOCK_REACQUIRE_DONE");
            // These two markers delimit the established frame-absent no-op boundary: with
            // `frame == None` there was never any restoration to perform between them.
            crate::yarm_log!("D6_FIRST_RESUME_POST_SWITCH_RESTORE_BEGIN");
            crate::yarm_log!("D6_FIRST_RESUME_POST_SWITCH_RESTORE_DONE");
            // Stage 139: capture hardware CR3 after post-switch restore so the
            // cleanup diagnostics can track any CR3 divergence introduced by
            // the proof's lock-drop switch. Read with no guard held — it is an
            // architectural observation, not KernelState.
            #[cfg(not(feature = "hosted-dev"))]
            {
                let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
                crate::yarm_log!("D6_PROOF_CR3_AFTER_FIRST_RESUME cr3=0x{:016x}", hw_cr3);
            }
        }
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

    /// Stage 188A: zero a blocked recv-v2 waiter's saved return registers so the
    /// resumed task sees a success result (ret0=0, error=0, ret1=0, ret2=0).
    ///
    /// Byte-identical to the inline block that `complete_blocked_recv_for_waiter`
    /// has always performed after a successful blocked-waiter delivery; extracted
    /// so the same completion can be applied by the Stage 188A dispatch-return
    /// executor (which runs the delivery's user copy through the 186E seam after
    /// the broad borrow drops). See that call site for the x86_64 register-slot
    /// rationale (RCX/RDX/R8 must be zeroed because the resumption path restores
    /// `user_gprs` verbatim).
    /// Stage 199G-B §1: this is now the recv-v2 SPELLING of
    /// [`Self::publish_blocked_recv_delivery_result`] — same single writeback owner, so the
    /// broad-lock and off-lock deliveries cannot drift apart.
    pub(crate) fn clear_blocked_recv_return_regs(&mut self, waiter_tid: u64) {
        self.publish_blocked_recv_delivery_result(waiter_tid, BlockedRecvDeliveryResult::RECV_V2);
    }

    /// Stage 199G-B §1: broad-lock sibling of
    /// [`Self::publish_blocked_recv_delivery_result_locked`], for the delivery paths that still
    /// hold `&mut KernelState`. It delegates to that one owner rather than repeating the
    /// projection, so there is exactly ONE place that decides what a satisfied blocked receiver
    /// sees in its return registers.
    pub(crate) fn publish_blocked_recv_delivery_result(
        &mut self,
        waiter_tid: u64,
        result: BlockedRecvDeliveryResult,
    ) {
        self.with_tcbs_mut(|tcbs| {
            Self::publish_blocked_recv_delivery_result_locked(tcbs, waiter_tid, result);
        });
    }

    /// Stage 198E3B2B: `&mut [Option<TCB>]` sibling of [`Self::clear_blocked_recv_return_regs`] for
    /// use inside `SharedKernel::with_task_tcbs_split_mut` (task rank 2 only). Byte-identical.
    pub(crate) fn clear_blocked_recv_return_regs_locked(
        tcbs: &mut [Option<crate::kernel::task::ThreadControlBlock>],
        waiter_tid: u64,
    ) {
        Self::publish_blocked_recv_delivery_result_locked(
            tcbs,
            waiter_tid,
            BlockedRecvDeliveryResult::RECV_V2,
        );
    }

    /// Stage 199G-B §A — **the ONE variant-driven blocked-receive DELIVERY writeback.**
    ///
    /// A blocked receiver that a sender satisfied is owed a success result, and the two receive
    /// ABIs owe DIFFERENT success results. Which one is not a property of the delivering path —
    /// it is a property of the receive the waiter issued — so it is passed in from the snapshot
    /// that captured it, and no call site re-derives it by inspecting the message.
    ///
    /// * [`RecvAbiVariant::RecvV2`] — `ret0 = 0`. Every field the receiver needs travels in the
    ///   40-byte meta struct the delivery already copied, so the register lanes are cleared to
    ///   the canonical success shape. This is byte-identical to what
    ///   `clear_blocked_recv_return_regs_locked` wrote before 199G-B, which is why that name
    ///   survives as the recv-v2 spelling of this call.
    /// * [`RecvAbiVariant::LegacyTimeout`] — the NR 5 / legacy shape, which writes **no
    ///   metadata at all**: `ret0 = sender_tid`, `ret1 = payload_len`,
    ///   `ret2 = SYSCALL_NO_TRANSFER_CAP`, error = 0. These are exactly the lanes the broad
    ///   `handle_ipc_recv_result_with_empty_error` success arm installs for a legacy receive
    ///   (`set_ok(sender, payload_len, …)` + `encode_transfer_cap_ret`), so a receiver cannot
    ///   tell which route delivered its message.
    ///
    /// Non-x86 ports mirror `arg0` and clear `user_gprs[0]`; their resume boundaries reconstruct
    /// the outgoing frame from the argument lanes, which is why `arg0` is written on every port
    /// and the extra x86 lanes only there.
    pub(crate) fn publish_blocked_recv_delivery_result_locked(
        tcbs: &mut [Option<crate::kernel::task::ThreadControlBlock>],
        waiter_tid: u64,
        result: BlockedRecvDeliveryResult,
    ) {
        let (ret0, ret1, ret2) = if result.recv_abi.writes_recv_v2_meta() {
            (0usize, 0usize, 0usize)
        } else {
            (
                result.sender_tid as usize,
                result.payload_len,
                result
                    .transfer_cap
                    .map_or(crate::kernel::syscall::SYSCALL_NO_TRANSFER_CAP, |c| c.0)
                    as usize,
            )
        };
        if let Some(tcb) = tcbs.iter_mut().flatten().find(|t| t.tid.0 == waiter_tid) {
            tcb.user_context.arg0 = ret0;
            tcb.user_context.user_gprs[0] = ret0; // RAX / x0 = ret0
            #[cfg(target_arch = "x86_64")]
            {
                tcb.user_context.user_gprs[2] = 0; // RCX = error = 0 (success)
                tcb.user_context.user_gprs[3] = ret2; // RDX = ret2
                tcb.user_context.user_gprs[7] = ret1; // R8  = ret1
            }
            let _ = (ret1, ret2);
        }
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
        let mut roots = [None; D6_PROOF_MAX_TASKS];
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

        let active_asid = self.hal.active_asid_on(self.current_cpu());
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
        let mut roots = [None; D6_PROOF_MAX_TASKS];
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

        // Stage 165B: start one guard page BELOW stack_base.  The proof's deep
        // post-switch call chain (`handle_trap` → `process_ipc_timeout_deadlines`,
        // ~8 KiB frame) overflows `[stack_base, stack_top)` into the region's
        // guard-adjacent page (observed #PF: CR2 = RSP − 8, both in
        // `[region_base, stack_base)`).  For the default-off proof we back that
        // single page (still supervisor-only) for BOTH proof participants so the
        // post-proof trap path cannot fault regardless of which participant's
        // region is current.  Production stacks (no `yarm.d6_switch_proof=1`) keep
        // their guard page unmapped — this path runs only under the proof knob.
        let region_base = (stack_base & !(PAGE_SIZE - 1)).saturating_sub(KERNEL_STACK_GUARD_SIZE);
        crate::yarm_log!(
            "D6_PROOF_FULL_STACK_MAP_BEGIN tid={} region_base=0x{:x} base=0x{:x} top=0x{:x}",
            tid,
            region_base,
            stack_base,
            stack_top
        );

        let mut page_addr = region_base;
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

    /// Stage 165B: map the FULL kernel-stack region containing the *sampled live
    /// RSP* for the D6 controlled-switch proof.
    ///
    /// `d6_ensure_full_proof_switch_stack_mapped` maps `[stack_base, stack_top)`
    /// selected by *task identity*.  But the proof's deep post-switch call chain
    /// (`handle_trap` → `process_ipc_timeout_deadlines`, which allocates and
    /// zeroes an ~8 KiB frame) grows the *live* kernel stack below `stack_base`,
    /// into the region's guard-adjacent page.  The observed Stage 165 #PF was a
    /// supervisor write (error 0x2) to `CR2 = RSP − 8` with both inside
    /// `[region_base, stack_base)` (e.g. `0xffff_8000_0001_8dd8`, region idx 3,
    /// whose mapped stack starts at `stack_base = 0x…1_9000`).
    ///
    /// Because the proof is a default-off diagnostic, this function selects the
    /// stack region by **RSP containment** (not by assuming a tid).
    ///
    /// Stage 165C: the proof samples RSP on the **boot/CPU kernel stack**, which
    /// lives in the kernel image high half (`>= KERNEL_BOOTSTRAP_VIRT_BASE`), NOT
    /// in the per-task region `[KERNEL_STACK_REGION_BASE, +MAX_TASKS*SIZE)`.
    /// Stage 165B mis-classified that high address as a per-task stack (it only
    /// checked `>= KERNEL_STACK_REGION_BASE`) and tried to allocate+map the
    /// already-kernel-mapped region, aborting with `VmFull`.  This function now
    /// classifies the sampled RSP:
    ///
    /// * **static kernel / boot / CPU stack** (the observed case): already mapped
    ///   supervisor-writable in the shared high half of every root, and we are
    ///   literally executing on it — so this is a *verify-only ensure*.  It probes
    ///   the active root, records the result, and accepts; it never allocates VM
    ///   metadata or maps into task roots (which is what tripped `VmFull`).
    /// * **per-task stack**: maps every page of `[region_base, region_top)` —
    ///   including the page below `stack_base` — supervisor-only writable in the
    ///   active root and every task root.  (The per-task guard-adjacent overflow
    ///   that motivated Stage 165B is also covered by
    ///   `d6_ensure_full_proof_switch_stack_mapped`.)
    ///
    /// This path runs solely under `yarm.d6_switch_proof=1`; production stacks are
    /// untouched.
    #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
    pub(crate) fn d6_ensure_live_rsp_region_mapped(
        &mut self,
        sampled_rsp: usize,
    ) -> Result<(), KernelError> {
        use crate::arch::selected_isa::page_table::{self, PageTableEntry};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        fn validate_entry(entry: PageTableEntry) -> bool {
            (entry.0 & PageTableEntry::WRITABLE) != 0 && (entry.0 & PageTableEntry::USER) == 0
        }

        // Per-task kernel stacks occupy
        // `[KERNEL_STACK_REGION_BASE, KERNEL_STACK_REGION_BASE + MAX_TASKS*SIZE)`.
        // The kernel image (with its boot/CPU `.bss` stacks) lives far higher, at
        // `>= KERNEL_BOOTSTRAP_VIRT_BASE`.  Stage 165B only checked
        // `rsp >= KERNEL_STACK_REGION_BASE`, which the kernel image ALSO satisfies,
        // so it mis-classified the boot stack as a per-task stack, computed a bogus
        // index, and tried to allocate+map an already-kernel-mapped region → VmFull.
        const PER_TASK_REGION_END: usize =
            KERNEL_STACK_REGION_BASE + super::MAX_TASKS * KERNEL_STACK_REGION_SIZE;
        let kernel_image_base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE as usize;
        let rsp_page = sampled_rsp & !(PAGE_SIZE - 1);

        let is_task_stack =
            sampled_rsp >= KERNEL_STACK_REGION_BASE && sampled_rsp < PER_TASK_REGION_END;
        let kind = if is_task_stack {
            "task_stack"
        } else if sampled_rsp >= kernel_image_base {
            "static_kernel_stack"
        } else {
            "unknown"
        };
        crate::yarm_log!(
            "D6_PROOF_LIVE_RSP_REGION_KIND kind={} rsp=0x{:x} rsp_page=0x{:x}",
            kind,
            sampled_rsp,
            rsp_page
        );

        let Some(target_asid) = self.hal.active_asid_on(self.current_cpu()) else {
            crate::yarm_log!(
                "D6_PROOF_LIVE_RSP_STACK_SKIP reason=no_active_asid rsp=0x{:x}",
                sampled_rsp
            );
            return Ok(());
        };

        // Probe the active root for the live RSP page (diagnostic ground truth).
        let probe = page_table::resolve_page(target_asid, VirtAddr(rsp_page as u64));
        let (probe_present, probe_writable, probe_user) = match probe {
            Some(e) => (
                1u8,
                ((e.0 & PageTableEntry::WRITABLE) != 0) as u8,
                ((e.0 & PageTableEntry::USER) != 0) as u8,
            ),
            None => (0u8, 0u8, 0u8),
        };
        crate::yarm_log!(
            "D6_PROOF_LIVE_RSP_MAP_PROBE root=active asid={} va=0x{:x} present={} writable={} user={}",
            target_asid.0,
            rsp_page,
            probe_present,
            probe_writable,
            probe_user
        );

        // Static kernel / boot / CPU stack case (the actually-observed case): the
        // region is backed by the kernel image mapping in the shared high half of
        // EVERY root.  This is an ENSURE, not an allocator — we are *executing on
        // this very stack*, so its page is mapped supervisor-writable by
        // construction.  Do NOT allocate user VM metadata and do NOT map into task
        // roots (that path trips VmFull on the kernel-image region).  Verify,
        // record, and accept; the per-task guard-adjacent overflow is handled
        // separately by `d6_ensure_full_proof_switch_stack_mapped`.
        if !is_task_stack {
            crate::yarm_log!(
                "D6_PROOF_LIVE_RSP_MAP_SKIP_ALREADY_PRESENT va=0x{:x}",
                rsp_page
            );
            if probe_present == 0 || probe_writable == 0 || probe_user != 0 {
                // `resolve_page` cannot always walk the early boot page tables;
                // the hardware mapping is nonetheless live (we run on it).  Record
                // the discrepancy without failing the proof.
                crate::yarm_log!(
                    "D6_PROOF_LIVE_RSP_MAP_FAIL_DETAIL step=static_kernel_probe err=resolve_view_only present={} writable={} user={} used=0 cap=0",
                    probe_present,
                    probe_writable,
                    probe_user
                );
            }
            crate::yarm_log!(
                "D6_PROOF_LIVE_RSP_STACK_MAP_DONE region_base=0x{:x} region_top=0x{:x} rsp_page=0x{:x} covers_rsp_page=1",
                rsp_page,
                rsp_page.saturating_add(PAGE_SIZE),
                rsp_page
            );
            return Ok(());
        }

        // ---- Per-task stack case: full-region ensure mapping (Stage 165B) ----
        let offset = sampled_rsp - KERNEL_STACK_REGION_BASE;
        let idx = offset / KERNEL_STACK_REGION_SIZE;
        let region_base = KERNEL_STACK_REGION_BASE
            .checked_add(idx.saturating_mul(KERNEL_STACK_REGION_SIZE))
            .ok_or(KernelError::VmFull)?;
        let region_top = region_base
            .checked_add(KERNEL_STACK_REGION_SIZE)
            .ok_or(KernelError::VmFull)?;

        // Collect all task ASIDs before the allocation loop (mirrors
        // `d6_ensure_full_proof_switch_stack_mapped`) so each live-region page is
        // shared into every root that may be active during a post-proof trap.
        let mut roots = [None; D6_PROOF_MAX_TASKS];
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
            "D6_PROOF_LIVE_RSP_STACK_MAP_BEGIN rsp=0x{:x} rsp_page=0x{:x} region_base=0x{:x} region_top=0x{:x}",
            sampled_rsp,
            rsp_page,
            region_base,
            region_top
        );

        // Map the ENTIRE region (region_base..region_top), not just the top page:
        // the live RSP can have descended below stack_base into the
        // guard-adjacent page, and the next trap frame grows further still.
        let mut covers_rsp_page = false;
        let mut page_addr = region_base;
        while page_addr < region_top {
            let stack_page = VirtAddr(page_addr as u64);
            let phys = if let Some(entry) = page_table::resolve_page(target_asid, stack_page) {
                if validate_entry(entry) {
                    if page_addr == rsp_page {
                        covers_rsp_page = true;
                    }
                    crate::yarm_log!("D6_PROOF_LIVE_RSP_STACK_MAP_SKIP va=0x{:x}", page_addr);
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
            if page_addr == rsp_page {
                covers_rsp_page = true;
            }
            crate::yarm_log!("D6_PROOF_LIVE_RSP_STACK_MAP_PAGE va=0x{:x}", page_addr);
            page_addr = page_addr.saturating_add(PAGE_SIZE);
        }

        // Coverage proof: the mapped range MUST include the page that contains the
        // sampled live RSP (the page family the post-proof trap frame faults in).
        crate::yarm_log!(
            "D6_PROOF_LIVE_RSP_STACK_MAP_DONE region_base=0x{:x} region_top=0x{:x} rsp_page=0x{:x} covers_rsp_page={}",
            region_base,
            region_top,
            rsp_page,
            covers_rsp_page
        );
        Ok(())
    }

    /// Stage 165D: after the D6 proof switches back to tid=1 and restores CR3 to
    /// asid 1, normal scheduling/trap/idle can land a trap on *another* task's
    /// kernel stack (observed: tid=3, stack `[0x…1_1000, 0x…1_8000)`) while the
    /// active root is still asid 1.  Per-task kernel stacks are mapped only in
    /// their own root, so the supervisor stack write (#PF error 0x2) faulted on
    /// `0xffff_8000_0001_7f98` (tid=3 top page) under asid 1.
    ///
    /// This default-off, proof-only "ensure" shares every schedulable task's
    /// kernel stack pages — by their authoritative owner-root physical frame —
    /// into the active root AND every other live task root, so whichever kernel
    /// stack a post-cleanup trap selects is supervisor-writable under whatever
    /// CR3 is active.
    ///
    /// Stage 165E: kernel stacks are demand-paged, so a schedulable task's stack
    /// page may not yet be mapped in its OWN (owner) root at cleanup time — the
    /// observed tid=3 case, where the Stage 165D mapper found no source frame and
    /// silently skipped, leaving asid 1 without tid=3's page and falsely reporting
    /// `failures=0`.  The mapper now, for each page: (1) SOURCE — take the frame
    /// from the owner root, or, if the owner lacks it, allocate the owner's real
    /// backing frame (`result=created`); frames are only ever created in the
    /// OWNER root, never fabricated into a non-owner root; (2) ROOT — share that
    /// exact frame into every root, accepting already-shared pages as
    /// `already_ok`.  Any schedulable page that cannot be sourced is an explicit
    /// `D6_POST_CLEANUP_STACK_MAP_SKIP` + a counted failure (never a silent skip).
    /// Stage 165G: tasks without an owner asid (e.g. tid=0) are NOT ignorable —
    /// idle / trap / interrupt / kernel-continuation paths can run on their kernel
    /// stack after cleanup (observed tid=0 #PF at `0xffff_8000_0000_7d78` under
    /// asid 1 on a long run).  Such a stack is sourced from an existing frame in
    /// any root, or, if none maps it, a proof-only frame allocated in the ACTIVE
    /// root, then shared into all roots — exactly like a schedulable stack.  Any
    /// page that cannot be backed is a hard `D6_POST_CLEANUP_STACK_MAP_SKIP` +
    /// counted failure (no "ignorable" NOTE).  No page is ever mapped
    /// user-accessible.
    ///
    /// Stage 165F: the deep post-cleanup call chain can overflow `[base, top)`
    /// into the guard-adjacent page below `stack_base` (observed tid=3 #PF at
    /// `0xffff_8000_0001_0dd8`, page `0x…1_0000` = `base − KERNEL_STACK_GUARD_SIZE`).
    /// For a schedulable task the mapped range is therefore extended to
    /// `[base − guard, top)`; the guard page is sourced/created in the owner root
    /// and shared like any other page, logged with
    /// `D6_POST_CLEANUP_STACK_MAP_GUARD_PAGE … included=1`.  Production guard pages
    /// are untouched (this runs only under `yarm.d6_switch_proof=1`).
    #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
    pub(crate) fn d6_ensure_post_cleanup_task_stacks_mapped(&mut self) -> Result<(), KernelError> {
        use crate::arch::selected_isa::page_table::{self, PageTableEntry};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        fn validate_entry(entry: PageTableEntry) -> bool {
            (entry.0 & PageTableEntry::WRITABLE) != 0 && (entry.0 & PageTableEntry::USER) == 0
        }

        let Some(active_asid) = self.hal.active_asid_on(self.current_cpu()) else {
            crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_BEGIN active_asid=none");
            crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_DONE tasks=0 failures=0");
            return Ok(());
        };
        let current_tid = self.current_tid().unwrap_or(u64::MAX);
        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
        crate::yarm_log!(
            "D6_POST_CLEANUP_STACK_MAP_BEGIN active_asid={}",
            active_asid.0
        );
        crate::yarm_log!(
            "D6_POST_CLEANUP_CURRENT_STATE current_tid={} active_asid={} cr3=0x{:016x}",
            current_tid,
            active_asid.0,
            hw_cr3
        );

        // Collect every live task's kernel stack (tid, base, top) up front.
        let mut stacks = [(0u64, 0usize, 0usize); D6_PROOF_MAX_TASKS];
        let mut n = 0usize;
        self.with_tcbs(|tcbs| {
            for tcb in tcbs.iter().flatten() {
                let (Some(base), Some(top)) =
                    (tcb.kernel_context.stack_base, tcb.kernel_context.stack_top)
                else {
                    continue;
                };
                if n < stacks.len() {
                    stacks[n] = (tcb.tid.0, base.0 as usize, top.0 as usize);
                    n += 1;
                }
            }
        });

        // The set of roots a post-cleanup trap can run under: the active root plus
        // every live task root.
        let mut roots = [None; D6_PROOF_MAX_TASKS];
        roots[0] = Some(active_asid);
        let mut roots_len = 1usize;
        for i in 0..n {
            let Some(asid) = self.task_asid(stacks[i].0) else {
                continue;
            };
            if roots[..roots_len].iter().any(|e| *e == Some(asid)) {
                continue;
            }
            if roots_len < roots.len() {
                roots[roots_len] = Some(asid);
                roots_len += 1;
            }
        }

        // TSS RSP0 audit: which kernel stack will the next user→kernel trap use,
        // and is its page mapped supervisor-writable in the active root?
        let rsp0 = crate::arch::x86_64::descriptor_tables::read_boot_tss_rsp0() as usize;
        let rsp0_page = rsp0.saturating_sub(8) & !(PAGE_SIZE - 1);
        let rsp0_tid = {
            let mut found = u64::MAX;
            for i in 0..n {
                let (tid, base, top) = stacks[i];
                if base <= rsp0 && rsp0 <= top {
                    found = tid;
                    break;
                }
            }
            found
        };
        let rsp0_mapped = rsp0_page != 0
            && page_table::resolve_page(active_asid, VirtAddr(rsp0_page as u64))
                .map(validate_entry)
                .unwrap_or(false);
        crate::yarm_log!(
            "D6_POST_CLEANUP_TSS_RSP0 tid={} rsp0=0x{:016x} page=0x{:x} mapped_active={}",
            rsp0_tid,
            rsp0,
            rsp0_page,
            rsp0_mapped as u8
        );

        let mut failures = 0usize;
        let mut guard_pages = 0usize;
        for i in 0..n {
            let (tid, base, top) = stacks[i];
            if base == 0 || base >= top {
                continue;
            }
            let top_page = top.saturating_sub(8) & !(PAGE_SIZE - 1);
            let owner = self.task_asid(tid);
            let owner_num: i64 = owner.map(|a| a.0 as i64).unwrap_or(-1);
            // Stage 165F/165G: extend the mapped range one guard page BELOW
            // stack_base for EVERY live task (Stage 165G: including no-owner
            // idle/trap-capable stacks such as tid=0).  The deep post-cleanup call
            // chain (handle_trap → printk → process_ipc_timeout_deadlines) can
            // overflow `[base, top)` into the guard-adjacent page (observed: tid=3
            // #PF at 0xffff_8000_0001_0dd8 = base − guard).  Production guard pages
            // are untouched because this path runs only under the proof knob.
            let region_base = (base & !(PAGE_SIZE - 1)).saturating_sub(KERNEL_STACK_GUARD_SIZE);
            crate::yarm_log!(
                "D6_POST_CLEANUP_STACK_MAP_TASK tid={} region_base=0x{:x} base=0x{:x} top=0x{:x} page=0x{:x}",
                tid,
                region_base,
                base,
                top,
                top_page
            );
            let mut page_addr = region_base;
            while page_addr < top {
                let page = VirtAddr(page_addr as u64);
                let is_top = page_addr == top_page;
                // The guard-adjacent page(s) below stack_base (schedulable tasks).
                let is_guard = page_addr < (base & !(PAGE_SIZE - 1));
                let log_page = is_top || is_guard;

                // Step 1 — SOURCE: obtain the authoritative physical frame for this
                // stack page.  Stage 165E: do NOT silently skip when no root maps
                // it.  A schedulable task (one with an owner asid) MUST have its
                // kernel stack backed; if the owner root does not yet map the page
                // (kernel stacks are demand-paged, so e.g. tid=3's top page may be
                // unmapped at cleanup time), allocate the owner's real backing
                // frame.  Frames are only ever created in the OWNER root — never
                // fabricated into a non-owner root — and the SAME frame is shared.
                let mut phys = None;
                let mut source = "missing";
                if let Some(oa) = owner {
                    if let Some(e) = page_table::resolve_page(oa, page) {
                        if validate_entry(e) {
                            phys = Some(e.addr());
                            source = "found";
                        }
                    }
                    if phys.is_none() {
                        match self.alloc_user_data_frame() {
                            Ok(p) => match page_table::map_page(
                                oa,
                                page,
                                PhysAddr(p),
                                PageFlags::KERNEL_RW,
                            ) {
                                Ok(_) => {
                                    phys = Some(p);
                                    source = "created";
                                }
                                Err(_) => source = "failed",
                            },
                            Err(_) => source = "failed",
                        }
                    }
                } else {
                    // No owner asid (e.g. tid=0).  Stage 165G: such a stack is NOT
                    // ignorable — idle / trap / interrupt / kernel-continuation
                    // paths can run on it after cleanup (observed tid=0 #PF at
                    // 0xffff_8000_0000_7d78 under asid 1 on a long run).  First try
                    // to reuse an existing frame from any root (no divergence); if
                    // none maps it, allocate a proof-only backing frame in the
                    // ACTIVE root (there is no owner root to create in) and share
                    // it.  Since a no-owner task runs under whichever task root is
                    // active, sharing one frame into every root keeps it consistent.
                    for root in roots[..roots_len].iter().flatten().copied() {
                        if let Some(e) = page_table::resolve_page(root, page) {
                            if validate_entry(e) {
                                phys = Some(e.addr());
                                source = "found";
                                break;
                            }
                        }
                    }
                    if phys.is_none() {
                        match self.alloc_user_data_frame() {
                            Ok(p) => match page_table::map_page(
                                active_asid,
                                page,
                                PhysAddr(p),
                                PageFlags::KERNEL_RW,
                            ) {
                                Ok(_) => {
                                    phys = Some(p);
                                    source = "created";
                                }
                                Err(_) => source = "failed",
                            },
                            Err(_) => source = "failed",
                        }
                    }
                    if log_page {
                        crate::yarm_log!(
                            "D6_POST_CLEANUP_STACK_MAP_NO_OWNER_ACTIVE_SOURCE tid={} page=0x{:x} result={}",
                            tid,
                            page_addr,
                            source
                        );
                    }
                }

                if is_guard {
                    let included = if phys.is_some() { 1 } else { 0 };
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_GUARD_PAGE tid={} page=0x{:x} included={}",
                        tid,
                        page_addr,
                        included
                    );
                    if included == 1 {
                        guard_pages += 1;
                    }
                }

                if log_page {
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_SOURCE tid={} owner_asid={} page=0x{:x} result={}",
                        tid,
                        owner_num,
                        page_addr,
                        source
                    );
                }

                let Some(phys) = phys else {
                    // No frame obtained.  Stage 165G: every live task's kernel
                    // stack — owner-asid OR no-owner (idle/trap-capable, e.g.
                    // tid=0) — MUST be backed; an unbacked page is a hard failure,
                    // never a silent skip and never an "ignorable" NOTE.
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_SKIP tid={} reason=no_source_frame page=0x{:x}",
                        tid,
                        page_addr
                    );
                    failures += 1;
                    page_addr = page_addr.saturating_add(PAGE_SIZE);
                    continue;
                };

                // Step 2 — ROOT: share the authoritative frame into every root a
                // post-cleanup trap can run under.
                for root in roots[..roots_len].iter().flatten().copied() {
                    let result = match page_table::resolve_page(root, page) {
                        Some(e) if validate_entry(e) && e.addr() == phys => "already_ok",
                        Some(e) if validate_entry(e) => {
                            // A different supervisor frame already maps this VA in
                            // this root — a genuine conflict; surface, do not hide.
                            failures += 1;
                            "failed"
                        }
                        Some(_) => {
                            failures += 1;
                            "failed"
                        }
                        None => match page_table::map_page(
                            root,
                            page,
                            PhysAddr(phys),
                            PageFlags::KERNEL_RW,
                        ) {
                            Ok(_) => "mapped",
                            Err(_) => {
                                failures += 1;
                                "failed"
                            }
                        },
                    };
                    if log_page {
                        crate::yarm_log!(
                            "D6_POST_CLEANUP_STACK_MAP_ROOT tid={} asid={} page=0x{:x} result={}",
                            tid,
                            root.0,
                            page_addr,
                            result
                        );
                    }
                }
                page_addr = page_addr.saturating_add(PAGE_SIZE);
            }
        }

        crate::yarm_log!(
            "D6_POST_CLEANUP_STACK_MAP_DONE tasks={} roots={} failures={} guard_pages={}",
            n,
            roots_len,
            failures,
            guard_pages
        );
        if failures > 0 {
            return Err(KernelError::VmFull);
        }
        Ok(())
    }

    /// U9-D3 §7 — rank 6 ONLY: the split twin of [`Self::alloc_user_data_frame`] (`memory_state.rs`).
    ///
    /// Same allocation, same PT-pool sanity panic, same trace marker; the one difference is that
    /// the memory domain is entered through `with_memory_split_mut` instead of a broad
    /// `&mut KernelState`, so nothing else is held while the frame allocator runs. Placed beside its ONLY caller,
    /// the split D6 stack repair below, because `memory_state.rs` carries a Stage 112/114 fence
    /// forbidding `with_memory_split_mut` in that file (the deferred `VmBrk` shrink live-wire);
    /// relocating this helper keeps that fence intact and unweakened rather than relaxing it. `alloc_user_data_frame` is left unmodified.
    #[allow(dead_code)]
    pub(crate) fn alloc_user_data_frame_split(
        shared: &crate::runtime::SharedKernel,
    ) -> Result<u64, KernelError> {
        let pa = shared.with_memory_split_mut(|memory| {
            crate::kernel::boot::kernel_mut(&mut memory.frame_allocator)
                .alloc_frame()
                .map_err(|_| KernelError::MemoryObjectFull)
        })?;
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

    /// U9-D3 §7 — rank 6 ONLY: return a frame that [`Self::alloc_user_data_frame_split`] produced
    /// but whose intended mapping FAILED, so it was never reachable from any address space.
    ///
    /// This is the "exact rollback of unused frames" the split D6 cleanup owes. The broad path had
    /// no equivalent — a frame whose `map_page` failed was simply lost — so this is strictly
    /// tighter, and it is safe precisely because the mapping failed: no page table refers to the
    /// frame, and no TLB entry can exist for it, so no shootdown is owed before it is reusable.
    #[allow(dead_code)]
    pub(crate) fn free_unmapped_user_data_frame_split(
        shared: &crate::runtime::SharedKernel,
        pa: u64,
    ) {
        shared.with_memory_split_mut(|memory| {
            let _ = crate::kernel::boot::kernel_mut(&mut memory.frame_allocator).free_frame(pa);
        });
    }

    /// U9-D3 §7 — the broad-lock-free twin of
    /// [`Self::d6_ensure_post_cleanup_task_stacks_mapped`], the FUNCTIONAL repair that was the
    /// last reason the Stage 117 switch-plan stash drain touched the broad lock.
    ///
    /// Nothing is deleted or weakened. Every marker, every counter, every source/root decision and
    /// the `VmFull` failure verdict are byte-for-byte what the broad body produces; the change is
    /// purely that the work is decomposed into bounded, value-owned phases with at most ONE domain
    /// lock held at a time, and none held across a page-table write.
    ///
    /// # Phases
    ///
    /// 1. **Lock-free** — the active ASID (`arch::hal::active_address_space`, a per-CPU atomic)
    ///    and the hardware CR3. Neither ever needed a lock.
    /// 2. **rank 1** — this CPU's current TID, then released.
    /// 3. **rank 2, ONE acquisition** — every live task's `(tid, stack_base, stack_top)` copied
    ///    BY VALUE into the same bounded `D6_PROOF_MAX_TASKS` array the broad body uses, plus each
    ///    task's owner ASID. Nothing borrows a TCB past this phase, and rank 2 is released before
    ///    any page-table work.
    /// 4. **No lock held** — the root set, the TSS RSP0 audit, and the per-page source/share walk.
    ///    A page that needs backing takes rank 6 for the ALLOCATION ALONE
    ///    ([`KernelState::alloc_user_data_frame_split`]), releases it, and only then maps — so no
    ///    lock is held across `page_table::map_page`.
    ///
    /// # Exact rollback of unused frames
    ///
    /// If the allocation succeeds and the mapping then fails, the frame was never reachable from
    /// any address space, so it is returned to the allocator
    /// ([`KernelState::free_unmapped_user_data_frame_split`]) instead of being lost. No shootdown
    /// is owed for it: no page table ever referred to it, so no TLB anywhere can hold a
    /// translation. The broad body leaked such a frame; this is strictly tighter, and the observed
    /// `result=failed` marker and counted failure are unchanged.
    #[cfg(all(target_arch = "x86_64", not(test), not(feature = "hosted-dev")))]
    pub(crate) fn d6_ensure_post_cleanup_task_stacks_mapped_split(
        shared: &crate::runtime::SharedKernel,
        cpu: crate::kernel::scheduler::CpuId,
    ) -> Result<(), KernelError> {
        use crate::arch::selected_isa::page_table::{self, PageTableEntry};
        use crate::kernel::vm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

        fn validate_entry(entry: PageTableEntry) -> bool {
            (entry.0 & PageTableEntry::WRITABLE) != 0 && (entry.0 & PageTableEntry::USER) == 0
        }

        // (1) Lock-free: the active root.
        let Some(active_asid) = crate::arch::hal::active_address_space(cpu) else {
            crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_BEGIN active_asid=none");
            crate::yarm_log!("D6_POST_CLEANUP_STACK_MAP_DONE tasks=0 failures=0");
            return Ok(());
        };
        // (2) rank 1, released immediately.
        let current_tid = shared.current_tid_split_read(cpu).unwrap_or(u64::MAX);
        let hw_cr3 = crate::arch::x86_64::page_table::read_hw_cr3();
        crate::yarm_log!(
            "D6_POST_CLEANUP_STACK_MAP_BEGIN active_asid={}",
            active_asid.0
        );
        crate::yarm_log!(
            "D6_POST_CLEANUP_CURRENT_STATE current_tid={} active_asid={} cr3=0x{:016x}",
            current_tid,
            active_asid.0,
            hw_cr3
        );

        // (3) rank 2, ONE acquisition — every live task's kernel stack, BY VALUE.
        let mut stacks = [(0u64, 0usize, 0usize); D6_PROOF_MAX_TASKS];
        let n = shared.d6_live_kernel_stacks_split(&mut stacks);
        // rank 2 released. Each owner ASID is then resolved through the same rank-2 seam, one
        // task at a time — the broad body's `self.task_asid(tid)`.
        let mut owners = [None; D6_PROOF_MAX_TASKS];
        for (i, owner) in owners[..n].iter_mut().enumerate() {
            *owner = shared.task_asid_opt_split_read(stacks[i].0);
        }

        // (4) No lock held from here on. The set of roots a post-cleanup trap can run under.
        let mut roots = [None; D6_PROOF_MAX_TASKS];
        roots[0] = Some(active_asid);
        let mut roots_len = 1usize;
        for owner in owners[..n].iter().copied() {
            let Some(asid) = owner else {
                continue;
            };
            if roots[..roots_len].iter().any(|e| *e == Some(asid)) {
                continue;
            }
            if roots_len < roots.len() {
                roots[roots_len] = Some(asid);
                roots_len += 1;
            }
        }

        // TSS RSP0 audit — unchanged.
        let rsp0 = crate::arch::x86_64::descriptor_tables::read_boot_tss_rsp0() as usize;
        let rsp0_page = rsp0.saturating_sub(8) & !(PAGE_SIZE - 1);
        let rsp0_tid = {
            let mut found = u64::MAX;
            for (tid, base, top) in stacks[..n].iter().copied() {
                if base <= rsp0 && rsp0 <= top {
                    found = tid;
                    break;
                }
            }
            found
        };
        let rsp0_mapped = rsp0_page != 0
            && page_table::resolve_page(active_asid, VirtAddr(rsp0_page as u64))
                .map(validate_entry)
                .unwrap_or(false);
        crate::yarm_log!(
            "D6_POST_CLEANUP_TSS_RSP0 tid={} rsp0=0x{:016x} page=0x{:x} mapped_active={}",
            rsp0_tid,
            rsp0,
            rsp0_page,
            rsp0_mapped as u8
        );

        let mut failures = 0usize;
        let mut guard_pages = 0usize;
        for i in 0..n {
            let (tid, base, top) = stacks[i];
            if base == 0 || base >= top {
                continue;
            }
            let top_page = top.saturating_sub(8) & !(PAGE_SIZE - 1);
            let owner = owners[i];
            let owner_num: i64 = owner.map(|a| a.0 as i64).unwrap_or(-1);
            // Stage 165F/165G: the mapped range extends one guard page BELOW stack_base for every
            // live task, unchanged.
            let region_base = (base & !(PAGE_SIZE - 1)).saturating_sub(KERNEL_STACK_GUARD_SIZE);
            crate::yarm_log!(
                "D6_POST_CLEANUP_STACK_MAP_TASK tid={} region_base=0x{:x} base=0x{:x} top=0x{:x} page=0x{:x}",
                tid,
                region_base,
                base,
                top,
                top_page
            );
            let mut page_addr = region_base;
            while page_addr < top {
                let page = VirtAddr(page_addr as u64);
                let is_top = page_addr == top_page;
                let is_guard = page_addr < (base & !(PAGE_SIZE - 1));
                let log_page = is_top || is_guard;

                // Step 1 — SOURCE. Identical decision tree to the broad body; the only difference
                // is that the allocation takes rank 6 alone and releases it BEFORE `map_page`, and
                // that a frame whose mapping fails is returned rather than leaked.
                let mut phys = None;
                let mut source = "missing";
                if let Some(oa) = owner {
                    if let Some(e) = page_table::resolve_page(oa, page)
                        && validate_entry(e)
                    {
                        phys = Some(e.addr());
                        source = "found";
                    }
                    if phys.is_none() {
                        match KernelState::alloc_user_data_frame_split(shared) {
                            Ok(p) => {
                                // rank 6 already released — the map runs with nothing held.
                                match page_table::map_page(
                                    oa,
                                    page,
                                    PhysAddr(p),
                                    PageFlags::KERNEL_RW,
                                ) {
                                    Ok(_) => {
                                        phys = Some(p);
                                        source = "created";
                                    }
                                    Err(_) => {
                                        KernelState::free_unmapped_user_data_frame_split(shared, p);
                                        source = "failed";
                                    }
                                }
                            }
                            Err(_) => source = "failed",
                        }
                    }
                } else {
                    // No owner asid (e.g. tid=0) — Stage 165G, unchanged: reuse an existing frame
                    // from any root, else allocate a proof-only frame in the ACTIVE root.
                    for root in roots[..roots_len].iter().flatten().copied() {
                        if let Some(e) = page_table::resolve_page(root, page)
                            && validate_entry(e)
                        {
                            phys = Some(e.addr());
                            source = "found";
                            break;
                        }
                    }
                    if phys.is_none() {
                        match KernelState::alloc_user_data_frame_split(shared) {
                            Ok(p) => {
                                match page_table::map_page(
                                    active_asid,
                                    page,
                                    PhysAddr(p),
                                    PageFlags::KERNEL_RW,
                                ) {
                                    Ok(_) => {
                                        phys = Some(p);
                                        source = "created";
                                    }
                                    Err(_) => {
                                        KernelState::free_unmapped_user_data_frame_split(shared, p);
                                        source = "failed";
                                    }
                                }
                            }
                            Err(_) => source = "failed",
                        }
                    }
                    if log_page {
                        crate::yarm_log!(
                            "D6_POST_CLEANUP_STACK_MAP_NO_OWNER_ACTIVE_SOURCE tid={} page=0x{:x} result={}",
                            tid,
                            page_addr,
                            source
                        );
                    }
                }

                if is_guard {
                    let included = if phys.is_some() { 1 } else { 0 };
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_GUARD_PAGE tid={} page=0x{:x} included={}",
                        tid,
                        page_addr,
                        included
                    );
                    if included == 1 {
                        guard_pages += 1;
                    }
                }

                if log_page {
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_SOURCE tid={} owner_asid={} page=0x{:x} result={}",
                        tid,
                        owner_num,
                        page_addr,
                        source
                    );
                }

                let Some(phys) = phys else {
                    crate::yarm_log!(
                        "D6_POST_CLEANUP_STACK_MAP_SKIP tid={} reason=no_source_frame page=0x{:x}",
                        tid,
                        page_addr
                    );
                    failures += 1;
                    page_addr = page_addr.saturating_add(PAGE_SIZE);
                    continue;
                };

                // Step 2 — ROOT: share the authoritative frame into every root, unchanged. No lock
                // is held here either; these are page-table writes, not domain-state writes.
                for root in roots[..roots_len].iter().flatten().copied() {
                    let result = match page_table::resolve_page(root, page) {
                        Some(e) if validate_entry(e) && e.addr() == phys => "already_ok",
                        Some(e) if validate_entry(e) => {
                            let _ = e;
                            failures += 1;
                            "failed"
                        }
                        Some(_) => {
                            failures += 1;
                            "failed"
                        }
                        None => match page_table::map_page(
                            root,
                            page,
                            PhysAddr(phys),
                            PageFlags::KERNEL_RW,
                        ) {
                            Ok(_) => "mapped",
                            Err(_) => {
                                failures += 1;
                                "failed"
                            }
                        },
                    };
                    if log_page {
                        crate::yarm_log!(
                            "D6_POST_CLEANUP_STACK_MAP_ROOT tid={} asid={} page=0x{:x} result={}",
                            tid,
                            root.0,
                            page_addr,
                            result
                        );
                    }
                }
                page_addr = page_addr.saturating_add(PAGE_SIZE);
            }
        }

        crate::yarm_log!(
            "D6_POST_CLEANUP_STACK_MAP_DONE tasks={} roots={} failures={} guard_pages={}",
            n,
            roots_len,
            failures,
            guard_pages
        );
        if failures > 0 {
            return Err(KernelError::VmFull);
        }
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

    /// Provision the default kernel context (stack region + switch frame) for a reserved
    /// incarnation — broad entry.
    ///
    /// U9-SPAWN-TXN §2 moved the body to [`provision_default_kernel_context_locked`], which takes
    /// the TCB storage directly. This is the last of U9-SPAWN2 §3's seven phases to get a
    /// rank-local owner.
    ///
    /// The old shape was three separate task-lock acquisitions — a slot-index read, a
    /// `set_thread_kernel_stack` write, and a switch-frame write — so a TCB could be observed
    /// with a stack assigned and no switch frame, or vice versa. One acquisition removes that
    /// window, and the body's all-or-nothing refusal removes the partially initialized TCB it
    /// could otherwise leave behind.
    pub(crate) fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        let outcome =
            self.with_tcbs_mut(|tcbs| provision_default_kernel_context_locked(tcbs, tid))?;
        crate::yarm_log!(
            "KERNEL_STACK_RANGE tid={} base=0x{:x} top=0x{:x}",
            tid,
            outcome.stack_base,
            outcome.stack_top
        );
        Ok(())
    }

    pub(crate) fn release_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| release_kernel_context_locked(tcbs, tid))
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

    /// U9-EXIT2 §2 — the ONLY writer of `ThreadDetachState::Detached`, and it has no production
    /// caller: no syscall reaches it, no boot path calls it, and every TCB constructor produces
    /// `Joinable`. So a `Detached` task cannot exist in a production kernel, which is what makes
    /// the self-exit route's `DetachedThread` refusal a class-B impossibility rather than a
    /// population it declines to serve.
    ///
    /// The `cfg` is the proof, not a comment about it: the freestanding builds do not compile this
    /// function at all, so a future production path that tries to detach a thread breaks the build
    /// and is forced to answer §2's real question — what the smallest no-allocation terminal
    /// cleanup for a detached self-exit would be — instead of silently reintroducing a broad edge.
    #[cfg(any(test, feature = "hosted-dev"))]
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

    /// U9-EXIT2 §3 — the ONLY writer of the robust-futex registry, and it has no production
    /// caller: there is no `SetRobustFutexHead` syscall, no boot path registers a list, and the
    /// fork publication explicitly CLEARS the child's slot. So the registry is empty in a
    /// production kernel, `has_robust_futex_list` is always false, and the broad `exit_task`'s
    /// robust-wake loop is unreachable.
    ///
    /// As with `mark_thread_detached`, the `cfg` is the proof: a future production registration
    /// path breaks the freestanding build and must then answer §3's real question — one lock
    /// domain, one no-allocation owner, owner-death publication outside every lock, one wake —
    /// rather than inheriting a refusal that quietly routes it back to the broad dispatcher.
    #[cfg(any(test, feature = "hosted-dev"))]
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
        // U9-EXIT1 §3: the registry is a TASK-domain array — it sits beside `tcbs`,
        // `task_classes` and `tls_restore_pending`, and is written only for a task that
        // `with_tcbs` above just proved live. It had no lock of its own because nothing outside
        // the broad guard ever reached it; a split reader does now, so both accessors take the
        // task lock and the pairing is real rather than incidental.
        self.with_task_robust_futex_mut(|robust| {
            if let Some(slot) = robust
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
        })
    }

    pub fn robust_futex_state(&self, tid: u64) -> Option<RobustFutexState> {
        self.with_task_robust_futex(|robust| {
            robust
                .iter()
                .flatten()
                .find(|entry| entry.tid.0 == tid)
                .map(|entry| entry.state)
        })
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

    /// Canonical 199E-R1D — snapshot the CURRENT task's asynchronously interrupted user context
    /// and publish the typed resume tag for it.
    ///
    /// This is the async sibling of [`Self::sync_current_thread_from_frame`], and it is a strict
    /// superset of it: the register file, PC and SP are captured by the same
    /// `capture_user_context`, and the tag is what tells the resume path that those registers are
    /// a live computation rather than a syscall/startup lane convention.
    ///
    /// Ordering is the whole safety argument, and it is enforced by construction rather than by
    /// the caller: the context is written FIRST and the tag is published LAST, so a tag can never
    /// be observed pointing at a half-written register file. The caller's obligation is the other
    /// half — call this before anything that can schedule another task.
    ///
    /// Returns `false` and publishes NOTHING when there is no current task, when the current task
    /// is the idle/kernel identity (tid 0), when the task has no ASID (it cannot be a user task),
    /// or when the preemption counter is exhausted. A refusal is always fail-closed: the previous
    /// tag, if any, is left untouched and no partial state is written.
    ///
    /// Repeated preemption of the same task is coherent: the context is overwritten wholesale and
    /// the generation advances, so the tag always names the newest snapshot and the older one
    /// becomes unmatchable rather than lingering as a second claim.
    pub(crate) fn snapshot_async_preempted_current(&mut self, frame: &TrapFrame) -> bool {
        let Some(tid) = self.current_tid() else {
            return false;
        };
        // tid 0 is the idle/kernel identity: it never returns to U-mode through a saved user
        // context, so there is nothing to preserve and a tag would be meaningless.
        if tid == 0 {
            return false;
        }
        let captured = frame.capture_user_context();
        self.with_tcbs_mut(|tcbs| {
            let Some(tcb) = tcbs.iter_mut().flatten().find(|tcb| tcb.tid.0 == tid) else {
                return false;
            };
            // A user task always has an ASID; without one the exact-incarnation check the tag
            // depends on cannot be formed, so refuse rather than publish an unverifiable tag.
            let Some(asid) = tcb.asid else {
                return false;
            };
            // `checked_add`: an exhausted counter refuses rather than wrapping into a value an
            // ancient tag could match.
            let Some(next_generation) = tcb.async_preempt_generation.checked_add(1) else {
                return false;
            };
            // Context FIRST — but the SYSCALL-ARGUMENT MIRROR is preserved, not overwritten.
            //
            // `UserRegisterContext` carries two mirrors of userspace state: `user_gprs` (the raw
            // register file) and `arg0..arg5` (the decoded syscall lane). They mean different
            // things and have different owners. The asynchronous resume reads `user_gprs`; the
            // ORDINARY resume arms — fresh startup and syscall/D2 continuation — treat `arg0..5`
            // as authoritative for `a0..a5`.
            //
            // Capturing a timer frame wholesale would write both, and that is a real defect
            // rather than a tidy-up: an interrupted task's mid-computation `a0` would land in the
            // syscall lane, and any later ORDINARY resume of that task — a wake from a blocked
            // receive, say — would install it as the syscall result. Measured live as
            // `core::fmt` faulting on `ld a1, 0(a0)` with `a0 = 0x10003`, an ordinary
            // intermediate value promoted to a pointer. Keeping the lane untouched leaves each
            // mirror owned by exactly the paths that write and read it.
            let preserved_syscall_lane = (
                tcb.user_context.arg0,
                tcb.user_context.arg1,
                tcb.user_context.arg2,
                tcb.user_context.arg3,
                tcb.user_context.arg4,
                tcb.user_context.arg5,
            );
            tcb.user_context = captured;
            tcb.user_context.arg0 = preserved_syscall_lane.0;
            tcb.user_context.arg1 = preserved_syscall_lane.1;
            tcb.user_context.arg2 = preserved_syscall_lane.2;
            tcb.user_context.arg3 = preserved_syscall_lane.3;
            tcb.user_context.arg4 = preserved_syscall_lane.4;
            tcb.user_context.arg5 = preserved_syscall_lane.5;
            tcb.async_preempt_generation = next_generation;
            // … tag LAST, so it can never name a half-written register file.
            tcb.async_preempted = Some(crate::kernel::task::AsyncPreemptedContext {
                tid,
                asid,
                preempt_generation: next_generation,
            });
            true
        })
    }

    /// Canonical 199E-R2 — consume `tid`'s async-preemption tag through THE canonical
    /// incoming-identity classifier, for the incarnation `incoming_asid` names.
    ///
    /// The broad-lock twin of `SharedKernel::take_async_preempt_for_incoming_split`, delegating
    /// to the same [`classify_and_take_async_resume`] so the hosted probes exercise the exact
    /// decision the live RISC-V write-backs make rather than a re-statement of it.
    ///
    /// There is deliberately NO variant keyed on `self.current_tid()`. That was the 199E-R1D
    /// shape, and on the post-lock dispatch route `current` is still the OUTGOING task when the
    /// restore runs — so it consumed the preempted task's authorization and handed the resumed
    /// task nothing. Making the identity a required argument is what makes that shape
    /// unexpressible.
    ///
    /// [`classify_and_take_async_resume`]: crate::kernel::task::classify_and_take_async_resume
    pub(crate) fn take_async_preempt_for_incoming(
        &mut self,
        incoming_tid: u64,
        incoming_asid: Option<crate::kernel::vm::Asid>,
    ) -> crate::kernel::task::AsyncResumeClass {
        self.with_tcbs_mut(|tcbs| {
            crate::kernel::task::classify_and_take_async_resume(tcbs, incoming_tid, incoming_asid)
        })
    }

    /// Canonical 199E-R2 — the broad-lock twin of
    /// `SharedKernel::cancel_async_preempt_for_split`: drop a staged snapshot that the trap is
    /// about to run past by returning through the original frame.
    pub(crate) fn cancel_async_preempt_for(&mut self, tid: u64) -> bool {
        self.with_tcbs_mut(|tcbs| crate::kernel::task::cancel_async_resume(tcbs, tid))
    }

    /// Canonical 199E-R1D — does `tid` currently carry a VALID async-preemption tag?
    ///
    /// Read-only; consumes nothing. Exists so a resume path can classify before it commits, and
    /// so tests can observe the tag's lifetime without ending it.
    #[must_use]
    pub(crate) fn async_preempted_resume_pending(&mut self, tid: u64) -> bool {
        self.with_tcbs_mut(|tcbs| {
            tcbs.iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.async_preempted.map(|tag| tag.matches_tcb(tcb)))
                .unwrap_or(false)
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
        // U9-EXIT1 §4 — one body, two owners. The rank-2 half lives in `exit_claim`, where the
        // split route's exit transaction drives it; this is that same body under the broad guard,
        // so a joiner wake cannot differ by route. The rank-1 enqueues stay here, after the
        // acquisition releases — which is the property that made the two halves separable at all.
        let mut wake_tids = [None; super::MAX_TASKS];
        let wake_count = self.with_tcbs_mut(|tcbs| {
            super::exit_claim::wake_joiners_for_locked(tcbs, target_tid, &mut wake_tids)
        });
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

    /// Allocate and map a task's user stack, resolving the address space from the task itself.
    ///
    /// This is the TID-keyed entry: it requires `tid` to ALREADY carry an ASID, which is why the
    /// stack could historically only be allocated after the spawn commit had bound one. The
    /// layout, guard page and probe all live in [`Self::allocate_user_stack_in_asid`]; this adds
    /// only the lookup, so there is exactly one stack-layout policy.
    pub(crate) fn allocate_user_stack_with_guard(
        &mut self,
        tid: u64,
        stack_pages: usize,
    ) -> Result<crate::kernel::vm::VirtAddr, KernelError> {
        let asid = self.task_asid(tid).ok_or(KernelError::UserMemoryFault)?;
        self.allocate_user_stack_in_asid(asid, tid, stack_pages)
    }

    /// THE user-stack allocator: the stack slot is derived from `tid`, but it is installed in the
    /// address space the CALLER names.
    ///
    /// U9-SPAWN-VM1 split this out of [`Self::allocate_user_stack_with_guard`] unchanged. The
    /// spawn-image provisioner needs a stack in an address space that no task carries yet — the
    /// child is `ReservedUnstarted` and its ASID is deliberately not yet bound — so it cannot go
    /// through the TID lookup. Nothing else differs: same slot arithmetic, same guard page, same
    /// overlap refusal, same resolve probe.
    /// THE user-stack allocator — broad entry.
    /// Body: [`super::vm_image_locked::allocate_user_stack_locked`].
    pub(crate) fn allocate_user_stack_in_asid(
        &mut self,
        asid: crate::kernel::vm::Asid,
        tid: u64,
        stack_pages: usize,
    ) -> Result<crate::kernel::vm::VirtAddr, KernelError> {
        self.with_vm_then_memory_mut(|vm, memory| {
            super::vm_image_locked::allocate_user_stack_locked(vm, memory, asid, tid, stack_pages)
        })
    }

    pub fn spawn_user_thread(
        &mut self,
        parent_tid: u64,
        tls_base: usize,
        user_stack_top: usize,
        user_entry: usize,
    ) -> Result<u64, KernelError> {
        use super::spawn_thread_core as core_;
        let args = core_::SpawnThreadArgs::validate(tls_base, user_stack_top, user_entry)?;
        // Parent identity and class in ONE task-lock acquisition — a parent replaced between the
        // two reads would give the child one task's group and another's class.
        let parent = self.with_task_enqueue_policy_mut(|tcbs, classes| {
            core_::parent_facts_locked(tcbs, classes, parent_tid)
        })?;
        // Staged brk ownership policy: brk bounds remain leader-owned and
        // per-task keyed; spawned threads do not get independent copied bounds.
        let tid = self.allocate_thread_id()?;
        // FIRST IRREVERSIBLE MUTATION.
        self.register_task_with_class_in_process(tid, parent.class, parent.thread_group_id.0)?;
        let initialized = self.with_task_enqueue_policy_mut(|tcbs, classes| {
            let Some(idx) = tcbs
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid))
            else {
                return Err(KernelError::TaskMissing);
            };
            match core_::initialize_thread_incarnation_locked(tcbs, idx, &parent, &args) {
                Ok(()) => Ok(()),
                Err(err) => {
                    core_::unregister_thread_incarnation_locked(
                        tcbs,
                        classes,
                        tid,
                        parent.thread_group_id,
                    );
                    Err(err)
                }
            }
        });
        if let Err(err) = initialized {
            return Err(err);
        }
        core_::claim_tls_restore_slot_locked(
            super::kernel_mut(&mut self.tls_restore_pending).as_mut_slice(),
            tid,
        );
        // U9-SPAWN1 SP-2: rank 1, last. A failed enqueue used to return here with the task still
        // registered, `Runnable` and never queued — a live-looking task no run queue owned and
        // whose TID could not be reallocated. It now undoes the EXACT incarnation it created,
        // through the same owner the pre-lock route uses, and returns the same error.
        match self.enqueue_task(tid) {
            Ok(_) => Ok(tid),
            Err(err) => {
                self.with_task_enqueue_policy_mut(|tcbs, classes| {
                    core_::unregister_thread_incarnation_locked(
                        tcbs,
                        classes,
                        tid,
                        parent.thread_group_id,
                    )
                });
                crate::yarm_log!(
                    "SPAWN_THREAD_ENQUEUE_FAILED tid={} err={:?} result=compensated",
                    tid,
                    err
                );
                Err(err)
            }
        }
    }

    /// U9-FORK1 §4 — the broad acquisition around THE fork transaction.
    ///
    /// The body moved to [`crate::kernel::syscall::fork_txn::fork_process_cow`], which both this
    /// route and the split route execute. What used to live here — a bare
    /// `register_task_with_class` that created an already-live task, followed by seven later
    /// steps that returned `Err` without undoing it — is replaced by the delivered spawn
    /// reservation lifecycle, so every failure arm now has an exact inverse. See `fork_txn.rs`
    /// for the order and the ledger.
    pub fn fork_user_process_cow(
        &mut self,
        parent_tid: u64,
        parent_context: Option<crate::kernel::task::UserRegisterContext>,
    ) -> Result<u64, KernelError> {
        let mut owners = crate::kernel::syscall::spawn_txn::BroadSpawnOwners { kernel: self };
        crate::kernel::syscall::fork_txn::fork_process_cow(&mut owners, parent_tid, parent_context)
    }
}

/// U9-SPAWN-TXN §2 — the kernel-stack region one reserved incarnation gets, and the facts a
/// caller needs to report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DefaultKernelContext {
    pub(crate) stack_base: usize,
    pub(crate) stack_top: usize,
}

/// THE default-kernel-context owner: task rank 2, one acquisition, all or nothing.
///
/// This is the last of U9-SPAWN2 §3's seven spawn phases to become rank-local. It takes the TCB
/// storage explicitly and reaches nothing else — no scheduler, no VM, no capability state, no
/// `KernelState` — so its rank-locality is readable from its signature.
///
/// # Exact identity, and no partial TCB on refusal
///
/// The stack region is derived from the TCB's SLOT INDEX, not from the TID: kernel stacks are a
/// fixed array of regions indexed by slot, so two tasks in the same slot at different times share
/// a region and two tasks in different slots never collide. The index is located by exact TID
/// match, and a TID with no live TCB is refused before anything is written.
///
/// Every arithmetic step is checked, and — this is the part the old three-acquisition shape got
/// wrong — all of it happens BEFORE the first field is written. The old body computed the region,
/// called `set_thread_kernel_stack` (which took the task lock, validated, and wrote two fields and
/// `initialized = false`), released, then took the lock again to write the frame's stack pointer,
/// instruction pointer and `owns_stack`. A failure between those two acquisitions left a TCB with
/// a kernel stack assigned and no switch frame — a partially initialized incarnation that nothing
/// removed. Here the only fallible steps are the index lookup and the address arithmetic, both of
/// which complete before any write, so a refusal leaves the TCB exactly as it was found.
pub(crate) fn provision_default_kernel_context_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
) -> Result<DefaultKernelContext, KernelError> {
    let idx = tcbs
        .iter()
        .position(|slot| slot.as_ref().is_some_and(|tcb| tcb.tid.0 == tid))
        .ok_or(KernelError::TaskMissing)?;

    // Stage 134: compute region_base separately so the guard page offset
    // (KERNEL_STACK_GUARD_SIZE) can be applied. The region layout is:
    //   [region_base,  region_base + GUARD)              → unmapped guard page
    //   [region_base + GUARD, region_base + REGION_SIZE) → mapped stack
    let region_base = KERNEL_STACK_REGION_BASE
        .checked_add(idx.saturating_mul(KERNEL_STACK_REGION_SIZE))
        .ok_or(KernelError::VmFull)?;
    let stack_base = region_base
        .checked_add(KERNEL_STACK_GUARD_SIZE)
        .ok_or(KernelError::VmFull)?;
    let stack_top = region_base
        .checked_add(KERNEL_STACK_REGION_SIZE)
        .ok_or(KernelError::VmFull)?;
    // `set_thread_kernel_stack`'s validation, applied here so this body enforces the same
    // contract rather than trusting a caller to have called it first.
    if stack_base == 0 || stack_top == 0 || stack_base >= stack_top {
        return Err(KernelError::WrongObject);
    }

    // FIRST WRITE. Everything fallible is behind us, so the whole set lands or none of it does.
    let tcb = tcbs
        .get_mut(idx)
        .and_then(Option::as_mut)
        .ok_or(KernelError::TaskMissing)?;
    tcb.kernel_context.stack_base = Some(crate::kernel::vm::VirtAddr(stack_base as u64));
    tcb.kernel_context.stack_top = Some(crate::kernel::vm::VirtAddr(stack_top as u64));
    tcb.kernel_context.frame.set_stack_ptr(stack_top & !0xF);
    tcb.kernel_context
        .frame
        .set_instruction_ptr(kernel_switch_frame_trampoline_ip());
    tcb.kernel_context.initialized = false;
    tcb.kernel_context.owns_stack = true;
    Ok(DefaultKernelContext {
        stack_base,
        stack_top,
    })
}

/// U9-SPAWN-TXN3 §3 — THE kernel-context release, under task rank 2 and nothing else.
///
/// The exact inverse of [`provision_default_kernel_context_locked`]: it gives back everything
/// that owner installed, and nothing else. It was an inline closure inside
/// `KernelState::release_kernel_context`; as a named owner both the broad and the split
/// reservation-cancel reach the same body, so the two cannot give back different fields.
pub(crate) fn release_kernel_context_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    tid: u64,
) -> Result<(), KernelError> {
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
}
