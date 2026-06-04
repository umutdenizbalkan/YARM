// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 28: trap/syscall split-dispatch bridge (whitelist-only scaffold).
//!
//! This module hosts the minimal, **whitelist-only** mechanism that classifies
//! a decoded `Syscall` as eligible for *split-dispatch* — i.e. servicing it via
//! per-domain split-mut/split-read helpers on [`SharedKernel`] WITHOUT taking the
//! global `SpinLock<KernelState>` and WITHOUT calling `with` / `with_cpu`.
//!
//! ## Default-deny contract
//!
//! [`try_split_dispatch`] returns `Some(result)` ONLY for syscalls on the
//! explicit whitelist. Every other syscall — including all IPC, Spawn/fork/exec,
//! VM, and futex paths — falls through the `_ => None` arm and MUST be handled by
//! the unchanged global-lock dispatch path (`SharedKernel::with_cpu` →
//! `KernelState::handle_trap` → `syscall::dispatch`). This guarantees that adding
//! the bridge can never silently change the behavior of any non-whitelisted
//! syscall: the fallback is the existing, fully-tested global-lock path.
//!
//! ## Why this is helper-only this stage (NOT live-wired)
//!
//! The whitelisted candidate (`ControlPlaneSetCnodeSlots`) returns a *non-trivial
//! trapframe payload*: the production handler writes
//! `frame.set_ok(slot_capacity, target_pid, 0)` — two meaningful return registers,
//! not a single status code. The bridge here returns only the logical
//! `Result<(), KernelError>` of the domain mutation; it deliberately does NOT
//! touch the `TrapFrame`.
//!
//! Live-wiring would require, at the x86_64 arch trap entry
//! (`descriptor_tables.rs` Stage-2N shared path):
//!   1. A pre-global-lock seam that has `&mut TrapFrame` AND the decoded args,
//!      able to write `set_ok(..)` itself (the existing
//!      `handle_trap_entry_shared` staging seam stages only diagnostic fault /
//!      recv-timeout data and does not own a result-writeback contract).
//!   2. Preservation of the `entering_tid` / `exiting_tid` / `task_switched`
//!      snapshots that today bracket `dispatch_trap_entry_with_shared_kernel` via
//!      `with_cpu(cpu, |k| k.current_tid())`. The control-plane syscall never
//!      switches tasks, so a split path must still make `task_switched == false`
//!      observable to the `write_trap_returns_to_saved_regs` branch.
//!
//! Both are arch-sensitive (hard invariant: do not touch x86_64 entering/exiting
//! TID logic, do not touch the trap boundary). Until that result-writeback
//! abstraction exists at the arch seam, the bridge stays helper-only and is proven
//! by unit tests. See `doc/KERNEL_LOCKING.md` §46.

use crate::kernel::boot::KernelError;
use crate::kernel::syscall::Syscall;
use crate::runtime::SharedKernel;

/// Syscalls eligible for split-dispatch (no global lock).
///
/// **WHITELIST ONLY.** A variant exists here only after the corresponding
/// `SharedKernel` split helper is proven safe (single ascending lock-domain
/// order, no blocking/yield/schedule, no user-memory copy in the bridge itself,
/// result encodable as the existing syscall return type).
// Helper-only this stage (NOT live-wired). The default-deny `_ => None`
// fallback means the live trap path keeps using the global-lock dispatch; these
// items are exercised by the Stage 28 tests and are ready to become live once
// the arch result-writeback seam exists (see module docs / §46).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitEligibleSyscall {
    /// `Syscall::ControlPlaneSetCnodeSlots` (NR 8). Serviced by
    /// `SharedKernel::control_plane_set_process_cnode_slots_split_mut`
    /// (task read rank 2 → boot-config read → capability mutate rank 4).
    ControlPlaneCnodeSlots {
        requester_tid: u64,
        target_pid: u64,
        slots: usize,
    },
    // Add others ONLY when the per-domain helper is proven safe.
}

/// Classify a decoded syscall + raw args into a split-eligible descriptor.
///
/// Returns `None` for every non-whitelisted syscall (default-deny). For the
/// whitelisted control-plane syscall it also validates the same argument
/// preconditions the global-lock handler enforces (`target_pid != 0`,
/// `slots != 0`); on a precondition miss it returns `None` so the caller falls
/// back to the global-lock path, which will produce the canonical
/// `InvalidArgs` error and the correct trapframe encoding.
#[allow(dead_code)]
pub(crate) fn classify_split_eligible(
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<SplitEligibleSyscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => {
            // args[0] = target_pid (SYSCALL_ARG_CAP), args[1] = slots (SYSCALL_ARG_PTR).
            let target_pid = args[0];
            let slots = args[1] as usize;
            if target_pid == 0 || slots == 0 {
                // Defer the InvalidArgs encoding to the global-lock path.
                return None;
            }
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid,
                target_pid,
                slots,
            })
        }
        // Default-deny: every other syscall falls back to the global-lock path.
        _ => None,
    }
}

/// Try to dispatch a syscall through the split (no-global-lock) path.
///
/// Returns `Some(result)` if the syscall is on the whitelist and was serviced via
/// per-domain split helpers; returns `None` to signal the caller to fall back to
/// the unchanged global-lock dispatch path. This function itself never blocks,
/// yields, schedules, or copies user memory.
#[allow(dead_code)]
pub(crate) fn try_split_dispatch(
    shared: &SharedKernel,
    syscall: Syscall,
    requester_tid: u64,
    args: [u64; 6],
) -> Option<Result<(), KernelError>> {
    let eligible = classify_split_eligible(syscall, requester_tid, args)?;
    match eligible {
        SplitEligibleSyscall::ControlPlaneCnodeSlots {
            requester_tid,
            target_pid,
            slots,
        } => Some(shared.control_plane_set_process_cnode_slots_split_mut(
            requester_tid,
            target_pid,
            slots,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::scheduler::CpuId;
    use crate::kernel::syscall::{
        SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR, SYSCALL_COUNT, SYSCALL_IPC_RECV_NR,
        SYSCALL_IPC_SEND_NR, SYSCALL_SPAWN_PROCESS_NR, SYSCALL_VM_MAP_NR,
    };
    use crate::kernel::task::TaskClass;

    fn decode(nr: usize) -> Syscall {
        Syscall::decode(nr).expect("decode syscall nr")
    }

    /// Boot a SharedKernel with a SystemServer requester (900) and an App target
    /// (901), with the requester dispatched as the current task — the same setup
    /// the Stage 27 control-plane helper test uses.
    fn shared_with_control_plane_requester() -> (SharedKernel, u64, u64) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(900, TaskClass::SystemServer)
                .expect("system server");
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("target app");
            state.enqueue_current_cpu(900).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(900) {
                state.yield_current().expect("switch");
            }
        });
        let _ = CpuId(0);
        (kernel, 900, 901)
    }

    #[test]
    fn stage28_split_dispatch_whitelist_accepts_cnode_slots_syscall() {
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before capacity");
        let requested = before.saturating_add(4);

        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, requested as u64, 0, 0, 0, 0];

        // Must be classified eligible.
        assert_eq!(
            classify_split_eligible(syscall, requester, args),
            Some(SplitEligibleSyscall::ControlPlaneCnodeSlots {
                requester_tid: requester,
                target_pid: target,
                slots: requested,
            }),
            "control-plane cnode-slots must be split-eligible"
        );

        // Must dispatch through the split path and mutate the capability domain.
        let result = try_split_dispatch(&kernel, syscall, requester, args);
        assert_eq!(result, Some(Ok(())), "split dispatch must service the syscall");

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "split path must resize the target cnode");
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_send() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_SEND_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC send must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_ipc_recv() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_IPC_RECV_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "IPC recv must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_spawnv5() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_SPAWN_PROCESS_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "SpawnV5 must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_whitelist_rejects_vm_map() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let syscall = decode(SYSCALL_VM_MAP_NR);
        let args = [1, 2, 3, 4, 5, 6];
        assert_eq!(classify_split_eligible(syscall, 1, args), None);
        assert_eq!(
            try_split_dispatch(&kernel, syscall, 1, args),
            None,
            "VM map must fall back to the global-lock path"
        );
    }

    #[test]
    fn stage28_split_dispatch_fallback_preserved_for_unwhitelisted() {
        // Every non-whitelisted syscall number must classify as None — the
        // default-deny contract. We exhaustively walk every decodable syscall and
        // assert that only ControlPlaneSetCnodeSlots is ever eligible.
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        let args = [0u64; 6]; // zero args → even cnode-slots fails preconditions → None
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue; // gaps in the NR space are not valid syscalls
            };
            // With zero args, NOTHING (including cnode-slots) is eligible.
            assert_eq!(
                classify_split_eligible(syscall, 1, args),
                None,
                "syscall nr {} must default-deny with zero args",
                nr
            );
            assert_eq!(
                try_split_dispatch(&kernel, syscall, 1, args),
                None,
                "syscall nr {} must fall back to global-lock path with zero args",
                nr
            );
        }
        // And the control-plane syscall with valid args IS the sole eligible one.
        let cp = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        assert!(
            classify_split_eligible(cp, 1, [5, 8, 0, 0, 0, 0]).is_some(),
            "control-plane cnode-slots with valid args must be eligible"
        );
    }

    #[test]
    fn stage28_syscall_count_unchanged() {
        // ABI guard: the split-dispatch scaffold is pure additive infrastructure
        // and must not alter the syscall ABI.
        assert_eq!(SYSCALL_COUNT, 30, "Stage 28 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage28_stage27_split_mut_helper_still_works() {
        // Regression: the Stage 27 split-mut helper the bridge delegates to must
        // still behave identically when invoked directly.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(8);
        kernel
            .control_plane_set_process_cnode_slots_split_mut(requester, target, requested)
            .expect("split-mut helper");
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "Stage 27 helper must still resize");

        // Absent requester still yields the stable TaskMissing error.
        let err = kernel
            .control_plane_set_process_cnode_slots_split_mut(123_456, target, 8)
            .expect_err("absent requester must fail");
        assert_eq!(err, KernelError::TaskMissing);
    }
}
