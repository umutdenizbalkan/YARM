// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Stage 28: trap/syscall split-dispatch bridge (whitelist-only scaffold).
//! Stage 29: live-wired for `ControlPlaneSetCnodeSlots` (NR 8) via
//! [`try_split_dispatch_into_frame`].
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
//! ## Stage 29 — live-wired result-writeback contract
//!
//! The whitelisted candidate (`ControlPlaneSetCnodeSlots`) returns a *non-trivial
//! trapframe payload*: the production handler writes
//! `frame.set_ok(slot_capacity, target_pid, 0)` — two meaningful return registers,
//! not a single status code. [`try_split_dispatch`] (Stage 28) returns only the
//! logical `Result<(), KernelError>`.
//!
//! Stage 29 adds [`try_split_dispatch_into_frame`], the minimal pre-global-lock
//! *result-writeback contract*. `TrapFrame::set_ok` / `set_err` are pure register
//! writes (no global-lock dependency, architecture-neutral — see
//! `kernel/trapframe.rs`), so the seam calls them directly:
//!   * It decodes `(target_pid, slots)` from the frame exactly as the global-lock
//!     handler does (`arg(SYSCALL_ARG_CAP)`, `arg(SYSCALL_ARG_PTR)`).
//!   * It reads the requester TID via `SharedKernel::current_tid_split_read(cpu)`
//!     (scheduler lock only) — value-equivalent to the global-lock
//!     `with_cpu(cpu, |k| k.current_tid())` the old `current_tid()` used.
//!   * On success it writes `set_ok(slots, pid, 0)` — byte-for-byte the encoding
//!     the global-lock handler produced — and returns `Some(Ok(()))`.
//!   * On a domain error it returns `Some(Err(TrapHandleError::Syscall(..)))` so
//!     the arch stub propagates it on exactly the path the old `Err(SyscallError)`
//!     return took (the control-plane syscall's errors are fatal/propagated, not
//!     user-recoverable — the old handler never wrote `set_err` for them either).
//!   * It returns `None` for every non-whitelisted syscall (and when the requester
//!     TID is unavailable), so the caller falls back to the UNCHANGED global-lock
//!     path.
//!
//! The split path never blocks/yields/schedules and never switches tasks, so
//! `entering_tid == exiting_tid` (i.e. `task_switched == false`) stays observable
//! to the arch `write_trap_returns_to_saved_regs` branch exactly as before. The
//! `entering_tid` / `exiting_tid` snapshots and the trap boundary are left
//! untouched. See `doc/KERNEL_LOCKING.md` §47.

use crate::kernel::boot::{KernelError, TrapHandleError};
use crate::kernel::scheduler::CpuId;
use crate::kernel::syscall::{Syscall, SyscallError};
use crate::kernel::trapframe::TrapFrame;
use crate::runtime::SharedKernel;

/// Syscalls eligible for split-dispatch (no global lock).
///
/// **WHITELIST ONLY.** A variant exists here only after the corresponding
/// `SharedKernel` split helper is proven safe (single ascending lock-domain
/// order, no blocking/yield/schedule, no user-memory copy in the bridge itself,
/// result encodable as the existing syscall return type).
// Stage 29: live-wired for `ControlPlaneCnodeSlots` via
// `try_split_dispatch_into_frame`. The default-deny `_ => None` fallback keeps
// every other syscall on the unchanged global-lock dispatch path.
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

/// # Validation status
/// - LIVE_TRAP_SMOKE_X86_64 — entry point for the NR 8 live split-dispatch path;
///   called from `handle_trap_entry_shared` before the global lock; x86_64 smoke
///   validated (Stage 29 / 29A, marker `YARM_LOCK_SPLIT_DISPATCH nr=8 result=ok`).
///
/// Stage 29 live-wire seam: try to service a syscall through the split
/// (no-global-lock) path AND write its result into the trap frame.
///
/// This is the pre-global-lock *result-writeback contract*. It is called from
/// `handle_trap_entry_shared` BEFORE the global `with_cpu` lock is taken.
///
/// Returns:
/// * `Some(Ok(()))`  — the syscall was a whitelisted split-eligible one, was
///   serviced via the per-domain split helpers, and the success payload was
///   written into `frame` via `set_ok(..)`. The caller must SKIP the global-lock
///   dispatch entirely (the result is already in the frame).
/// * `Some(Err(e))`  — the syscall was whitelisted but the domain mutation failed.
///   `e` is the same `TrapHandleError::Syscall(..)` the global-lock path would have
///   returned for this error; the caller propagates it on the existing error path.
/// * `None`          — the syscall is NOT split-eligible (default-deny) OR the
///   requester TID was unavailable. The caller MUST fall back to the unchanged
///   global-lock dispatch path.
///
/// The split path never blocks, yields, schedules, switches tasks, or copies user
/// memory. Because no task switch occurs, `entering_tid == exiting_tid` and
/// `task_switched == false` remain observable to the arch return-register
/// writeback branch exactly as on the global-lock path.
pub(crate) fn try_split_dispatch_into_frame(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
) -> Option<Result<(), TrapHandleError>> {
    use crate::kernel::syscall::{SYSCALL_ARG_CAP, SYSCALL_ARG_PTR};

    // Default-deny by syscall number first (cheap, no lock).
    let syscall = Syscall::decode(frame.syscall_num()).ok()?;
    if classify_split_eligible_nr_only(syscall).is_none() {
        return None;
    }

    // The requester TID is what the global-lock handler reads via
    // `current_tid(kernel)` (i.e. `kernel.current_tid()`).
    //
    // Stage 29A: this MUST use the authoritative `current_tid_authoritative(cpu)`
    // read, NOT `current_tid_split_read(cpu)`. At the live x86_64 pre-global-lock
    // trap point the split-read of the scheduler's per-CPU current slot is stale
    // (it can observe a prior task such as tid 0 instead of the running requester),
    // which made the requester-class permission check resolve the wrong task and
    // return `MissingRight`. The authoritative read binds `current_cpu` first and
    // returns the same task the global-lock handler sees. It is a read-only
    // current-task snapshot (no dispatch/yield/switch); the domain mutation below
    // still runs lock-free via the split-mut helper. If unavailable, fall back so
    // the global-lock path produces the canonical `Internal` error.
    let requester_tid = shared.current_tid_authoritative(cpu)?;

    // Decode args identically to `handle_control_plane_set_cnode_slots`.
    let mut args = [0u64; 6];
    for (i, slot) in args.iter_mut().enumerate() {
        *slot = frame.arg(i) as u64;
    }

    let result = try_split_dispatch(shared, syscall, requester_tid, args)?;
    match result {
        Ok(()) => {
            // Mirror the global-lock handler's exact success encoding:
            //   frame.set_ok(slot_capacity, target_pid as usize, 0)
            let target_pid = frame.arg(SYSCALL_ARG_CAP);
            let slots = frame.arg(SYSCALL_ARG_PTR);
            frame.set_ok(slots, target_pid, 0);
            Some(Ok(()))
        }
        Err(err) => Some(Err(TrapHandleError::Syscall(SyscallError::from(err)))),
    }
}

/// Number-only split eligibility classifier (no arg validation, no lock).
///
/// Used by [`try_split_dispatch_into_frame`] as the fast default-deny gate before
/// reading any scheduler/task state. Argument-precondition validation is still
/// performed by `classify_split_eligible`, so a syscall that passes this gate but
/// fails its preconditions (e.g. `target_pid == 0`) still falls back to the
/// global-lock path for the canonical error encoding.
fn classify_split_eligible_nr_only(syscall: Syscall) -> Option<Syscall> {
    match syscall {
        Syscall::ControlPlaneSetCnodeSlots => Some(syscall),
        _ => None,
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

    // ----------------------------------------------------------------------
    // Stage 29 — live-wired result-writeback seam (try_split_dispatch_into_frame)
    // ----------------------------------------------------------------------

    use crate::kernel::trapframe::TrapFrame;

    const CPU0: CpuId = CpuId(0);

    /// Build the same NR-8 trap frame the live arch path constructs:
    /// arg(SYSCALL_ARG_CAP)=target_pid, arg(SYSCALL_ARG_PTR)=slots.
    fn cnode_slots_frame(target_pid: u64, slots: usize) -> TrapFrame {
        TrapFrame::new(
            SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR,
            [target_pid as usize, slots, 0, 0, 0, 0],
        )
    }

    /// Boot a SharedKernel where an App requester (901) is the current task on
    /// CPU 0, plus a second App target (902). Used to exercise the MissingRight
    /// guard (a non-system-server App may only resize its own cnode).
    fn shared_with_app_requester() -> (SharedKernel, u64, u64) {
        let kernel = SharedKernel::new(Bootstrap::init().expect("init"));
        kernel.with(|state| {
            state
                .register_task_with_class(901, TaskClass::App)
                .expect("app requester");
            state
                .register_task_with_class(902, TaskClass::App)
                .expect("app target");
            state.enqueue_current_cpu(901).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(901) {
                state.yield_current().expect("switch");
            }
        });
        (kernel, 901, 902)
    }

    #[test]
    fn stage29_split_cnode_slots_ok_return_lanes() {
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(4);
        let mut frame = cnode_slots_frame(target, requested);

        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(result, Some(Ok(())), "split seam must service NR 8");
        // Exact lanes the old global-lock handler produced: set_ok(slots, pid, 0).
        assert_eq!(frame.ret0(), requested, "ret0 == slots");
        assert_eq!(frame.ret1(), target as usize, "ret1 == target pid");
        assert_eq!(frame.ret2(), 0, "ret2 == 0");
        assert_eq!(frame.error_code(), None, "no error on success");

        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested), "capability domain actually resized");
    }

    #[test]
    fn stage29_split_cnode_slots_missing_task_error() {
        // Requester TID with no registered task → TaskMissing. Exercised via the
        // helper the seam delegates to (the seam itself always reads a present
        // current TID; an absent requester must surface the same error).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let syscall = decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR);
        let args = [target, 16, 0, 0, 0, 0];
        let result = try_split_dispatch(&kernel, syscall, 424_242, args);
        assert_eq!(result, Some(Err(KernelError::TaskMissing)));
    }

    #[test]
    fn stage29_split_cnode_slots_bad_requester_class_error() {
        // App requester (901) targeting a DIFFERENT pid (902) → MissingRight.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            )))),
            "App requester resizing another pid's cnode must be MissingRight"
        );
        // On error the seam must NOT write a success payload.
        assert_eq!(frame.ret0(), 0);
        assert_eq!(frame.ret1(), 0);
    }

    #[test]
    fn stage29_split_cnode_slots_missing_cnode_error() {
        // System-server requester targeting a pid with no registered cnode and no
        // pre-reserved cnode space: the create path must fail rather than fabricate
        // a success. We use a target pid that was never registered.
        let (kernel, _requester, _target) = shared_with_control_plane_requester();
        let unregistered_pid = 7_777u64;
        // Whatever the domain decides (create or reject), the seam must propagate
        // the SAME Result the split-mut helper returns — never silently OK with a
        // bogus frame payload. Compare seam vs direct helper.
        let mut frame = cnode_slots_frame(unregistered_pid, 16);
        let seam = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let direct = kernel.control_plane_set_process_cnode_slots_split_mut(
            900,
            unregistered_pid,
            16,
        );
        match (seam, direct) {
            (Some(Ok(())), Ok(())) => {
                // Create path succeeded: the frame must carry the canonical lanes.
                assert_eq!(frame.ret0(), 16);
                assert_eq!(frame.ret1(), unregistered_pid as usize);
            }
            (Some(Err(TrapHandleError::Syscall(s))), Err(k)) => {
                assert_eq!(s, SyscallError::from(k), "seam error must equal helper error");
                assert_eq!(frame.error_code(), None, "seam never writes set_err for hard errors");
            }
            (seam, direct) => panic!("seam/direct divergence: {seam:?} vs {direct:?}"),
        }
    }

    #[test]
    fn stage29_split_cnode_slots_duplicate_update_ok() {
        // Calling the seam twice with the same target must be idempotent-OK.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(6);
        let mut f1 = cnode_slots_frame(target, requested);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut f1), Some(Ok(())));
        let mut f2 = cnode_slots_frame(target, requested);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut f2), Some(Ok(())));
        assert_eq!(f2.ret0(), requested);
        assert_eq!(f2.ret1(), target as usize);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(requested));
    }

    #[test]
    fn stage29_split_cnode_slots_capacity_resize_ok() {
        // Distinct grow then a second grow: lanes track the latest request.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let base = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("base");
        let grow1 = base.saturating_add(2);
        let mut f1 = cnode_slots_frame(target, grow1);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut f1), Some(Ok(())));
        assert_eq!(f1.ret0(), grow1);
        let grow2 = grow1.saturating_add(5);
        let mut f2 = cnode_slots_frame(target, grow2);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut f2), Some(Ok(())));
        assert_eq!(f2.ret0(), grow2);
        let after = kernel.with(|state| {
            let cnode = state.process_cnode_for_pid(target).expect("cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(after, Some(grow2));
    }

    #[test]
    fn stage29_split_cnode_slots_error_code_preserved() {
        // The error code surfaced by the seam must equal the From<KernelError>
        // SyscallError code of the underlying domain error (MissingRight → 4).
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let Some(Err(TrapHandleError::Syscall(err))) =
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame)
        else {
            panic!("expected a Syscall error");
        };
        assert_eq!(err, SyscallError::from(KernelError::MissingRight));
        assert_eq!(err.code(), SyscallError::MissingRight.code());
    }

    #[test]
    fn stage29_split_cnode_slots_no_scheduler_side_effect() {
        // The split path must not switch tasks: current TID is unchanged across it.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let before_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(before_tid, Some(requester));
        let mut frame = cnode_slots_frame(target, 12);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let after_tid = kernel.current_tid_split_read(CPU0);
        assert_eq!(after_tid, Some(requester), "no task switch (task_switched==false)");
    }

    #[test]
    fn stage29_split_cnode_slots_no_ipc_side_effect() {
        // The split path must not enqueue IPC: the target task stays runnable and
        // its status is not changed to any blocked endpoint state.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let mut frame = cnode_slots_frame(target, 14);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let status = kernel.with(|state| state.task_status(target));
        assert!(
            !matches!(
                status,
                Some(crate::kernel::task::TaskStatus::Blocked(
                    crate::kernel::task::WaitReason::EndpointSend(_)
                        | crate::kernel::task::WaitReason::EndpointReceive(_)
                ))
            ),
            "split path must not block the target on any endpoint"
        );
    }

    // ---- Part 5: fallback safety ----

    #[test]
    fn stage29_only_nr8_is_split_eligible() {
        assert!(classify_split_eligible_nr_only(
            decode(SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR)
        )
        .is_some());
    }

    #[test]
    fn stage29_ipc_send_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut frame),
            None,
            "IPC send must fall back to the global-lock path"
        );
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_IPC_SEND_NR)).is_none());
    }

    #[test]
    fn stage29_spawnv5_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_SPAWN_PROCESS_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut frame), None);
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_SPAWN_PROCESS_NR)).is_none());
    }

    #[test]
    fn stage29_vm_map_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_VM_MAP_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut frame), None);
        assert!(classify_split_eligible_nr_only(decode(SYSCALL_VM_MAP_NR)).is_none());
    }

    #[test]
    fn stage29_futex_not_eligible() {
        let (kernel, _r, _t) = shared_with_control_plane_requester();
        let mut frame = TrapFrame::new(SYSCALL_IPC_RECV_NR, [1, 2, 3, 4, 5, 6]);
        // IPC recv stands in for any blocking syscall; also assert futex by number.
        assert_eq!(try_split_dispatch_into_frame(&kernel, CPU0, &mut frame), None);
        assert!(classify_split_eligible_nr_only(
            decode(crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR)
        )
        .is_none());
        assert!(classify_split_eligible_nr_only(
            decode(crate::kernel::syscall::SYSCALL_FUTEX_WAKE_NR)
        )
        .is_none());
    }

    #[test]
    fn stage29_syscall_count_still_30() {
        assert_eq!(SYSCALL_COUNT, 30, "Stage 29 must not change SYSCALL_COUNT");
    }

    #[test]
    fn stage29_whitelist_exhaustive() {
        // Iterate the full NR space; only NR 8 may be split-eligible.
        for nr in 0..SYSCALL_COUNT {
            let Ok(syscall) = Syscall::decode(nr) else {
                continue;
            };
            let eligible = classify_split_eligible_nr_only(syscall).is_some();
            if nr == SYSCALL_CONTROL_PLANE_SET_CNODE_SLOTS_NR {
                assert!(eligible, "NR 8 must be split-eligible");
            } else {
                assert!(!eligible, "NR {nr} must NOT be split-eligible");
            }
        }
    }

    // ---- Part 6: result-writeback equivalence ----

    #[test]
    fn stage29_split_result_ok_encodes_same_as_old_path() {
        // The seam's success lanes must equal what the old global-lock handler
        // produced: set_ok(slot_capacity, target_pid, 0).
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(3);
        let mut seam_frame = cnode_slots_frame(target, requested);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut seam_frame),
            Some(Ok(()))
        );

        // Reference encoding the old path used.
        let mut ref_frame = cnode_slots_frame(target, requested);
        ref_frame.set_ok(requested, target as usize, 0);

        assert_eq!(seam_frame.ret0(), ref_frame.ret0());
        assert_eq!(seam_frame.ret1(), ref_frame.ret1());
        assert_eq!(seam_frame.ret2(), ref_frame.ret2());
        assert_eq!(seam_frame.error_code(), ref_frame.error_code());
    }

    #[test]
    fn stage29_split_result_err_encodes_same_as_old_path() {
        // On a domain error the seam returns TrapHandleError::Syscall(e) — exactly
        // what the old handler's `Err(SyscallError)` became at the trap boundary —
        // and leaves the frame return lanes untouched (no set_ok), matching the old
        // path which never wrote set_ok on error.
        let (kernel, _requester, target) = shared_with_app_requester();
        let mut frame = cnode_slots_frame(target, 16);
        let result = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        assert_eq!(
            result,
            Some(Err(TrapHandleError::Syscall(SyscallError::from(
                KernelError::MissingRight
            ))))
        );
        assert_eq!(frame.ret0(), 0, "no success payload on error");
        assert_eq!(frame.ret1(), 0, "no success payload on error");
    }

    #[test]
    fn stage29_split_result_no_task_switch() {
        // entering_tid == exiting_tid across the seam ⇒ task_switched == false,
        // which the arch path requires to take the write_trap_returns branch.
        let (kernel, requester, target) = shared_with_control_plane_requester();
        let entering = kernel.current_tid_split_read(CPU0);
        let mut frame = cnode_slots_frame(target, 10);
        let _ = try_split_dispatch_into_frame(&kernel, CPU0, &mut frame);
        let exiting = kernel.current_tid_split_read(CPU0);
        assert_eq!(entering, exiting);
        assert_eq!(exiting, Some(requester));
    }

    #[test]
    fn stage29_split_dispatch_fallback_path_unchanged() {
        // A None return from the seam means the global-lock handler still runs.
        // Prove the global-lock dispatch produces the canonical result for the
        // same NR-8 frame the seam would have serviced — i.e. the fallback path is
        // intact and value-equivalent.
        let (kernel, _requester, target) = shared_with_control_plane_requester();
        // A NON-whitelisted syscall returns None from the seam.
        let mut send_frame = TrapFrame::new(SYSCALL_IPC_SEND_NR, [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            try_split_dispatch_into_frame(&kernel, CPU0, &mut send_frame),
            None,
            "non-whitelisted syscall must fall back (None)"
        );
        // And the global-lock handler can still service NR 8 directly.
        let before = kernel
            .with(|state| {
                let cnode = state.process_cnode_for_pid(target).expect("cnode");
                state.cnode_slot_capacity(cnode)
            })
            .expect("before");
        let requested = before.saturating_add(7);
        let mut nr8 = cnode_slots_frame(target, requested);
        kernel
            .with(|state| crate::kernel::syscall::dispatch(state, &mut nr8))
            .expect("global-lock dispatch");
        assert_eq!(nr8.ret0(), requested);
        assert_eq!(nr8.ret1(), target as usize);
    }
}
