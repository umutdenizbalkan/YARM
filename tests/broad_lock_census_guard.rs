// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Broad-lock census guard — keeps canonical Stage 204A from silently going stale.
//!
//! Stage 204A ("broad-lock callsite census") is the one canonical stage recorded COMPLETE.
//! Its deliverable is an authoritative, classified list of every runtime acquisition of the
//! broad `SpinLock<KernelState>`, in `doc/KERNEL_UNLOCK_AUDIT.md` §1. A census is only
//! complete while it matches the tree, so the moment someone adds or deletes a production
//! `SharedKernel::with` / `with_cpu` callsite without re-classifying it, 204A quietly stops
//! being true and every downstream count (§1.2, §1.4a, `doc/KERNEL_LOCKING.md` §0.2,
//! `doc/STATUS.md` §0) becomes wrong.
//!
//! This guard recomputes the census from source with the same method the audit used and
//! fails if it drifts from the pinned expectation.
//!
//! **Adding a broad-lock callsite is not forbidden** — the retirement work legitimately
//! moves them around. What is forbidden is doing so without updating the census. When this
//! test fails, update [`EXPECTED_WITH_CPU`] / [`EXPECTED_WITH_BROAD`] /
//! [`EXPECTED_STATE_LOCK`] here **and** the classification and totals in the documents
//! named above, in the same change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-file count of production `SharedKernel::with_cpu` callsites.
const EXPECTED_WITH_CPU: &[(&str, usize)] = &[
    // U3 (203C): 8 -> 7 -> 6 -> 3. Two read-only post-lock drains moved to the rank-1
    // scheduler seam (`current_tid_split_read`), then the three homologous switch/restore
    // drains (queue-switch foundation, FutexWait switch-success, Yield switch-success) moved
    // to the exact-token rank-2 transaction: `direct_dispatch_activate_asid_split` (which
    // performs the real RISC-V map/root/`write_satp`+`sfence.vma` on this target) plus
    // `direct_dispatch_restore_context_split` and `direct_dispatch_take_completion_split`.
    // U3 (203C): 3 -> 2. The post-lock `CurrentTaskExited` validation snapshot moved to
    // `post_lock_exit_validation_split`, one coherent rank-1 (scheduler) transaction with the
    // rank-2 (task) acquisition nested inside it. The two remaining acquisitions are the
    // canonical broad trap phase and the deferred terminal-idle predicate.
    // U3 (203C): 2 -> 1. The post-lock blocked-syscall TERMINAL-IDLE predicate moved to
    // `terminal_idle_on_cpu_split`, ONE coherent rank-1 scheduler snapshot: validate + bind
    // `current_cpu` (as `with_cpu` did), then `current_tid_on(cpu)` and `runnable_count_on(cpu)`
    // under that single guard, so the predicate can no longer tear between two separately
    // locked reads. **This file is now fully drained of reacquisitions**: its ONE remaining
    // production `with_cpu` is the canonical broad Phase-2 trap handler itself.
    // U9-TERMINAL-FINAL §5: 1 -> 0. That last one — the canonical broad Phase-2 trap handler —
    // is DELETED. Every supported trap outcome on this port is settled by a pre-lock owner, and
    // a trap that reaches the end of the bridge with none set is settled per event class by
    // `arch::trap_entry::settle_unowned_trap`, driven here through
    // `settle_unowned_trap_at_riscv_bridge`, which composes `bind_current_cpu_split` for the
    // CPU admission the deleted `with_cpu` performed on entry.
    // **`src/arch/riscv64/trap.rs` now has NO production broad acquisition of any kind.**
    // Stage 199D: 12 -> 11. The AArch64 handled-split return path no longer reacquires the
    // broad lock to finalize a syscall; it uses two bounded rank-2 task-domain transactions
    // (exact-incarnation TLS take, exact-incarnation context commit) instead.
    // U3 (203C): 11 -> 9. The AArch64 FutexWait and Yield switch-success restores now run the
    // neutral exact-token resume core (`direct_dispatch_resume_incoming_core`). The FutexWait
    // no-incoming idle check was briefly retired too and then RESTORED: its only live gate (the
    // Stage 195F idle oracle) is unreachable behind the pre-existing SpawnV5 stall, so that one
    // substitution could not be live-proven and was not shipped.
    // U3 (203C): 9 -> 7. The two homologous x86_64 D2 switch-success restores (blocking send
    // and blocking receive) were byte-identical broad re-acquires; both now run one neutral
    // exact-token transaction, `x86_post_lock_resume_marked_incoming`.
    // U3 (203C): 7 -> 5. The x86_64 FutexWait and Yield switch-success restores were the same
    // body again; both now reuse that transaction unchanged. The five that remain are the
    // canonical broad trap phase, the AArch64 FutexWait no-incoming idle read, the D6
    // controlled-proof restore, and the two AArch64 ExitCurrentTask acquisitions.
    // U3 (203C): 5 -> 4. The AArch64 `CurrentTaskExited` VALIDATION reacquisition became
    // `SharedKernel::post_lock_exit_validation_split`, the same rank-1 -> rank-2 snapshot the
    // RISC-V exit consumer already used.
    // U3 (203C): 4 -> 3. The AArch64 `CurrentTaskExited` REPLACEMENT RESTORE reacquisition became
    // `SharedKernel::post_exit_replacement_restore_split` (one coherent rank-1 -> rank-2
    // transaction taking the replacement's ASID, saved context, TLS request and parked
    // blocked-syscall completion) plus `aarch64::trap::post_exit_restore_replacement`, which does
    // the TTBR0/frame work with every domain lock released through the SAME single frame writer
    // the in-lock restore uses. The three that remain are the canonical broad trap phase, the
    // AArch64 FutexWait no-incoming idle read, and the production post-switch
    // architectural-state restore site — whose
    // acquisition is DELIBERATELY retained: its body also runs the D6 proof cleanup
    // (`d6_ensure_post_cleanup_task_stacks_mapped` maps kernel stack pages into the active root
    // and every live task root), which is D3-fenced by AI_AGENT_RULES §14.4 and has no split form,
    // so splitting it would leave a broad drain behind at census delta 0.
    // U3 (203C): 3 -> 2. The AArch64 deferred-FutexWait NO-INCOMING idle read
    // (`with_cpu(cpu, |kernel| matches!(kernel.current_tid(), None | Some(0)))`) became the
    // EXISTING authoritative rank-1 transaction `SharedKernel::current_tid_authoritative(cpu)` —
    // no new seam. That helper applies the same `validate_online_cpu` predicate
    // `set_current_cpu` applies, performs the same `current_cpu` binding, and reads the same
    // `current_tid_on(current_cpu())` (MPIDR-derived on freestanding AArch64), all under ONE
    // rank-1 acquisition instead of the broad guard's two separate scheduler acquisitions. The
    // predicate and the refusal policy are outcome-for-outcome unchanged: `Some(0)` is idle, and
    // both "no current task" and "CPU refused" arrive as `None` and are mapped to `true` by the
    // same `unwrap_or(true)`.
    // U9-D3 §7: 2 -> 1. The retained broad tail at the production post-switch restore site is
    // RETIRED. Its D3 fence is lifted (0xF1 is the sole generation-matched ACK producer, proven
    // live from CPL0 and CPL3), so the functional D6 stack repair is now
    // `d6_ensure_post_cleanup_task_stacks_mapped_split`: rank 1 for the current TID, rank 2 for
    // every live task's kernel stack BY VALUE, then a page-table walk with NO lock held that takes
    // rank 6 for each frame ALLOCATION alone and releases it before mapping — plus an exact
    // rollback of any frame whose mapping failed, which the broad body leaked. Every marker,
    // counter and the `VmFull` verdict are unchanged, and `D6_SWITCH_A_DONE` is still emitted
    // here. The RISC-V in-lock restore that shared that tail went with it: the drain is
    // statically unreachable on RISC-V (`maybe_switch_kernel_context` gates the stash on
    // `!cfg!(target_arch = "riscv64")`), so no reachable behaviour was removed and the
    // cross-arch FOUNDATION `post_switch_restore_arch_thread_state` stays defined.
    // The ONE that remains is the canonical broad Phase-2 trap dispatch — a terminal dispatcher.
    // U9-RECV-BLOCK1 §6 took this 1 -> 2 for `drive_pending_x86_smp_unlock_audit`, and
    // U9-RECV-BLOCK2 §3 took it BACK to 1 without giving up the provisioning it restored.
    //
    // `maybe_run_x86_smp_unlock_audit` clears an AP's wake-only bit and drives
    // `live_ap_user_dispatch` -> `build_ap_workload`. Both of its historical call sites sit in
    // `KernelState::handle_trap`'s BROAD arms, so it ran only on traps the split routes declined —
    // an incidental dependency that closing the receive family broke. §6 repaired it from the trap
    // path, which cost an acquisition. §3 drives the SAME one-shot body from `run_scheduler_loop`,
    // immediately after `dispatch_ready_task()`, where `&mut KernelState` is already owned by the
    // boot loop: every gate the audit applies holds there (APs released, graduated proof
    // completed, a real user task current) and no lock is taken to reach it.
    //
    // U9-TERMINAL-FINAL §5: 1 -> 0. That last one — the canonical broad Phase-2 trap handler
    // shared by x86_64 and AArch64 — is DELETED. Every supported trap outcome is settled by a
    // pre-lock owner, and a trap that reaches the end of the bridge with none of the six
    // settlement flags set is settled PER EVENT CLASS by `settle_unowned_trap`, whose `Syscall`
    // arm reproduces the broad arm on both axes: `InvalidNumber` written into the caller frame
    // and `Ok(())`, so the errno returns to userspace rather than halting the kernel.
    // **`src/arch/trap_entry.rs` now has NO production broad acquisition of any kind.**
    // U3 (203C): 4 -> 3. The AP saved-resume placement's `with_cpu(cpu, |k| { enqueue; dispatch })`
    // became one authoritative rank-1 -> rank-2 transaction,
    // `SharedKernel::enqueue_then_dispatch_on_cpu_split`: rank 1 acquired once, CPU validated with
    // the same predicate `set_current_cpu` uses, `current_cpu` bound, then rank 2 nested (ascending)
    // to resolve the whole enqueue policy — reservation refusal, existence, class -> priority — in
    // ONE acquisition instead of the two `with_tcbs` reads `enqueue_on_cpu` takes before touching
    // the queue. The homologous next-task placement in `ap_sched_next_or_idle` is NOT converted: no
    // existing workload reaches its success body, so it keeps its acquisition byte-for-byte. The
    // three that remain are that one, the AP return-to-idle `block_current_on_cpu`, and the BSP
    // `on_preempt_prefer_on_cpu`.
    // U3 (203C): 3 -> 2. The AP return-to-idle `with_cpu(cpu, |k| k.block_current_on_cpu(cpu))`
    // became `SharedKernel::block_current_on_cpu_split`, one rank-1 scheduler transaction: the
    // whole acquisition existed to do a single scheduler-domain thing — take this CPU's `current`
    // and drop its membership entry — so rank 1 is acquired once, the CPU is validated with the
    // same predicate `set_current_cpu` uses, `current_cpu` is bound, and the existing
    // `block_current_on` primitive runs inside that guard. Rank 2 is never taken and no task
    // status changes. The two that remain are the BSP `on_preempt_prefer_on_cpu` and the
    // unreached ED-2 next-task placement, neither of which is live-reached.
    // U3 (203C): 2 -> 1. The BSP saved-resume `with_cpu(cpu, |k| k.on_preempt_prefer_on_cpu(cpu,
    // client_tid))` became `SharedKernel::on_preempt_prefer_on_cpu_split`, ONE rank-1 scheduler
    // transaction for the EXPLICIT trapping CPU that runs the SAME `Scheduler::on_preempt_prefer_on`
    // primitive. The retired `KernelState::on_preempt_prefer_on_cpu` wrapper was a thin re-entry
    // into that same rank-1 lock and has been deleted now that it is caller-free; the scheduler
    // policy keeps a single implementation. Online validation, the previous-current re-enqueue, the
    // preferred-task selection and the legacy fallback (another runnable task may be selected when
    // the preferred one is not queued) are all unchanged; the consumer still aborts unless the
    // returned TID is its own.
    // U3 (203C): 1 -> 0. **`src/arch/x86_64/smp.rs` is now FULLY DRAINED** — it holds no broad
    // acquisition of any form. The last one was the ED-2 next-task placement in
    // `ap_sched_next_or_idle`, `with_cpu(cpu, |k| match k.enqueue_on_cpu(cpu, next_tid) { Ok(()) =>
    // k.dispatch_next_on_cpu(cpu), Err(_) => None }).unwrap_or(None)`. It was retained while its
    // success body was believed unreachable; the existing SELECTOR-OFF two-task AP workload
    // (`yarm.ap_user_dispatch=1` with NO direct-IpcCall oracle, where `ap_workload_task_count()`
    // returns `AP_WORKLOAD_TASKS` = 2) was then proven to reach it live — `X86_AP_ADMIT_PLACED
    // cpu=1 tid=20206`, `X86_AP_NEXT_TASK_DISPATCH_BEGIN cpu=1`, both TIDs issuing a real ring-3
    // syscall, `X86_AP_REPEATED_DISPATCH_OK cpu=1 count=2` — so it was retired onto the EXISTING
    // `enqueue_then_dispatch_on_cpu_split` with `EnqueueRefusalPolicy::Decline`, which is this
    // site's historical `Err(_) => None` policy verbatim. No new seam was added; `DispatchAnyway`
    // stays the saved-resume policy and the two remain distinct.
    // U3 (203C): 1 -> 0. `src/kernel/boot/thread_state.rs` is now FULLY DRAINED. The x86_64 D6
    // first-resume trampoline's `with_cpu(ctx.cpu_id, |kernel| …)` existed to call
    // `post_switch_restore_arch_thread_state(kernel, cpu, None)` — and with `frame == None` the
    // x86_64 `restore_arch_thread_state` returns `Ok(())` on its first statement, before any
    // current-TID read, TCB access, context/TLS restore, ASID activation, CR3 check or domain
    // lock. The acquisition's only KernelState effect was therefore the CPU validate-and-bind
    // `with_cpu` performs on entry, which is exactly what `SharedKernel::bind_current_cpu_split`
    // now does under one rank-1 acquisition. The REAL D6 restore/cleanup acquisition in
    // `trap_entry.rs` passes a live frame and is untouched.
    // U1: 13 -> 12. The obsolete `SharedKernel::handle_trap_with_cpu` wrapper had no
    // in-tree caller at all and was deleted.
    // U3 (203C): 12 -> 11. `current_tid_authoritative` became the authoritative rank-1
    // scheduler transaction. It still VALIDATES the CPU and BINDS `current_cpu` before
    // reading — only the broad lock went away — which is what distinguishes it from the
    // reverted Stage 4T+6 substitution onto the non-binding `current_tid_split_read`.
    // U3 (203C): 11 -> 8. The three homologous blocked-waiter Phase-C completions
    // (`execute_dispatch_post_work`, `execute_blocked_waiter_reply_cap_delivery`,
    // `execute_blocked_waiter_ordinary_cap_delivery`) each re-entered the broad lock with a
    // byte-identical body: clear the waiter's return registers, clear the endpoint waiter, wake.
    // All three now call ONE shared, class-neutral, rank-ordered transaction,
    // `complete_blocked_waiter_delivery_split` — rank 1 (validate + bind), rank 2 (return-register
    // clear, wake-TID ASID read), rank 3 (identity-keyed `clear_endpoint_waiter_if_identity`),
    // then the rank-2/rank-1 wake — so no seam is entered while a rank >= its own is held.
    // U3 (203C): 8 -> 7. `revalidate_idle_owner_after_drains` became a CPU-local rank-1/rank-2
    // transaction: rank 1 (`validate_online_cpu` + bind `current_cpu` + the same single
    // `dispatch_next_on`) is fully released before the rank-2 snapshot (user context, the
    // pending TLS-restore request, and the task's OPTIONAL asid in ONE acquisition, no
    // `TaskStatus` write), which is fully released before the frame / FS-base / CR3 work.
    // Rollback re-takes rank 1 alone. The proven-infallible legacy restore is why the
    // `restorable` requeue arm stays unreachable there, exactly as it was under the broad lock.
    // U3 (203C): 7 -> 5. The two homologous recv copy-fault completions in
    // `complete_recv_boundary_user_copy` — the plain `CopyFault` arm and the recv-v2
    // `PayloadCopyFault` arm — each re-entered the broad lock with the byte-identical body
    // `recv_boundary_record_user_fault(k, frame, user_ptr)`. Both now call ONE class-neutral
    // transaction, `record_recv_boundary_user_fault_split`: rank 1 (the existing
    // `bind_current_cpu_split`, promoted to architecture-neutral) then rank 8 (the existing
    // `record_fault_split_mut`), then the frame error with no lock held. No new seam. The
    // capability-rollback re-entry in the same method is dependency-blocked and untouched.
    // U3 (203C): 5 -> 4. The ordinary-cap DEFERRED SENDER WAKE in
    // `complete_recv_boundary_ordinary_cap` re-entered the broad lock to run a two-line
    // composition: the `IPC_RECV_SPLIT_REFILL_WAKE_APPLY` marker, then
    // `apply_scheduler_wake_plan(Wake(tid))` — which is `wake_tid_to_runnable`. It now calls
    // `apply_split_sender_wake_plan_split`: rank 1 (`bind_current_cpu_split`, the same
    // validate-and-bind `with_cpu` performed on entry) released, the marker, then the SHARED
    // `wake_tid_to_runnable_split` (rank 2 then rank 1). No wake logic is duplicated — that
    // body is the one the blocked-waiter completion already used, renamed from
    // `wake_blocked_waiter_split` now that it has a second production caller.
    // U9-C (200B reply-cap production transaction): 4 -> 3. The recv Phase-A acquisition is
    // retired. Its body now runs off the broad lock as `SharedKernel::recv_queued_split_phase_a_split`,
    // composing ordered split seams plus one new exact transaction —
    // `materialize_reply_cap_split` (rank 3 consume -> rank 3 liveness -> rank 2/4 CNode ->
    // rank 4 mint -> rank 3 record), which wires the production caller Stage 188D built the
    // seams for and deliberately left unwired. The three survivors are the ordinary
    // `rollback_materialized_recv_cap` callers; they reach `revoke_capability_in_cnode`
    // (cross-address-space unmap + TLB shootdown, rank-6 reclaim, notification destroy + wake)
    // and stay D3/cap-teardown fenced. See doc/KERNEL_UNLOCK_AUDIT.md §1.4a.
    // U9-D3 §6: 3 -> 0. The D3 fence is lifted — vector 0xF1 is the sole target-side
    // invalidation and generation-matched ACK producer, earning real ACKs from CPL0 and CPL3, and
    // `unmap_range_two_phase_split` completes a real cross-CPU shootdown with NO lock held. The
    // U9-F phased composition therefore now serves the memory-backed cohort too: it performs the
    // active-transfer-mapping unmap, the supervisor report, the rank-6 refcount drop and a
    // reclaim that happens only after every target acknowledged. All three broad fallbacks are
    // retired; **`src/runtime.rs` now has NO production broad acquisition of any kind.**
];

/// Per-file count of production broad `SharedKernel::with(|state| …)` callsites.
///
/// `src/kernel/boot/orchestrator_state.rs` is listed because it matches the same textual
/// pattern, but its single hit is `LOCK_ORDER_LAST_RANK.with(|last| …)` — a `thread_local!`
/// accessor, **not** a broad-lock acquisition. It is counted here so the guard stays purely
/// mechanical, and subtracted by [`THREAD_LOCAL_FALSE_POSITIVES`] to reach the audited
/// figure.
const EXPECTED_WITH_BROAD: &[(&str, usize)] = &[
    // U3 (203C): 2 -> 1. The AP saved-frame resume's broad read became one authoritative rank-2
    // snapshot transaction (`SharedKernel::ap_saved_resume_context_split`): a single task-domain
    // acquisition copies ASID + status + full `UserRegisterContext` + TLS by value, and the
    // ASID -> CR3 resolution runs only after that guard is released. The BSP counterpart was NOT
    // converted at that point: its path was not live-reached, and an unreached site is never
    // retired merely because it is homologous to a reached one.
    // U3 (203C): 1 -> 0. Two census-neutral prerequisites made that BSP path live-reachable
    // (explicit-CPU authority for the current-task read and both direct-IpcCall remote-wake
    // decisions, then an order-independent request-OK proof rendezvous), and the BSP saved-resume
    // read is now retired onto the SAME `ap_saved_resume_context_split` its AP twin already used.
    // `KernelState::ap_saved_resume_context` was deleted with it, being caller-free.
    // **There is no production broad `SharedKernel::with` callsite left in the tree**; the only
    // remaining match is the thread-local false positive counted below.
    ("src/kernel/boot/orchestrator_state.rs", 1),
    // U1: 8 -> 7. The obsolete `SharedKernel::run_reply_timeout_completion` wrapper had no
    // production caller and was deleted; the single completion body
    // (`KernelState::run_reply_timeout_completion_locked`) is unchanged.
    // U2: 7 -> 4. The two test-only helpers were relocated into test-only modules:
    // `ipc_recv_with_deadline_split_bridge` (2 acquisitions) and the
    // `SharedKernel::control_plane_set_process_cnode_slots_via_syscall` wrapper (1).
    // U3 (203C): 4 -> 0. The last four broad acquisitions in this file moved onto existing
    // rank-domain seams: the two home-CPU wrappers onto the rank-2 task seam, and the reply-win
    // deadline disarm onto rank 2 (handle read) followed by rank 3 (exact token disarm), taken
    // sequentially and never nested. `src/runtime.rs` now has NO production broad acquisition.
];

/// Per-file count of raw `self.state.lock()` sites.
///
/// BROAD-LOCK-RETIREMENT1: 3 -> 0. The three were the bodies of `SharedKernel::lock` / `with` /
/// `with_cpu`. The production wrappers are deleted, the `SpinLock<KernelState>` they locked is
/// deleted, and the hosted-test-only helpers that keep the names serialize on a separate
/// `#[cfg(test)]` lock — so no raw broad-lock body exists in any build.
const EXPECTED_STATE_LOCK: &[(&str, usize)] = &[];

/// Textual matches in [`EXPECTED_WITH_BROAD`] that are not broad-lock acquisitions.
const THREAD_LOCAL_FALSE_POSITIVES: usize = 1;

/// The audited totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.2.
///
/// The acquisition total is `with_cpu + with_broad` only. The three raw `self.state.lock()`
/// sites are the **bodies** of `SharedKernel::lock` / `with` / `with_cpu` — the
/// implementations that every callsite goes through, not callsites themselves. Adding them
/// would double-count the lock.
const AUDITED_WITH_CPU_TOTAL: usize = 0; // U9-TERMINAL-FINAL §5: 2 -> 0 (both terminal broad trap dispatchers deleted; ZERO production `with_cpu` callsites remain). U9-RECV-BLOCK2 §3: 3 -> 2 (the SMP-unlock audit driver moved to the boot ownership point, which already owns `&mut KernelState`). U9-RECV-BLOCK1 §6: 2 -> 3. U9-D3 §7: 3 -> 2 (the D6 functional broad tail retired; only the two terminal dispatchers remain). U9-D3 §6: 6 -> 3 (all three ordinary rollback fallbacks retired). U9-C: 7 -> 6 (recv Phase-A retired). U3: 38 -> 33 -> 31 -> 30 -> 28 -> 26 -> 23 -> 22 -> 21 -> 20 -> 17 -> 16 -> 14 -> 13 -> 12 -> 11 -> 10 -> 9 -> 8 -> 7 (… the x86_64 BSP saved-resume preempt-and-prefer reacquisition, then the x86_64 ED-2 next-task placement)
const AUDITED_WITH_BROAD_TOTAL: usize = 0; // U3: 6 -> 2 -> 1 -> 0 (runtime.rs fully drained, then both x86 SMP saved-resume reads retired). ZERO production broad `SharedKernel::with` callsites remain.
const AUDITED_STATE_LOCK_TOTAL: usize = 0; // BLR1: 3 -> 0 (the broad lock and its wrappers are deleted)
const AUDITED_ACQUISITION_TOTAL: usize = AUDITED_WITH_CPU_TOTAL + AUDITED_WITH_BROAD_TOTAL;

/// Stage 204A classification totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a.
// U9-RECV-BLOCK2 §3: back to 0. The SMP-unlock audit driver §6 classified here takes no lock at
// all now — it runs from `run_scheduler_loop`, which already owns `&mut KernelState` — so it is
// not an acquisition site to classify.
const CLASS_BOOT_ONLY: usize = 0;
const CLASS_TEST_ONLY: usize = 0; // U2: 3 -> 0 (test-only helpers left the production census)
const CLASS_OBSOLETE: usize = 0; // U1: 2 -> 0 (both obsolete acquisitions deleted)
const CLASS_RUNTIME_REQUIRED: usize = 0; // U9-TERMINAL-FINAL §5: 2 -> 0 (both terminal broad trap dispatchers deleted). U9-D3 §7: 3 -> 2 (the D6 functional broad tail retired). U9-D3 §6: 6 -> 3 (all three ordinary rollback fallbacks retired). U9-C: 7 -> 6 (recv Phase-A retired). U3: 44 -> 39 -> 37 -> 36 -> 34 -> 32 -> 28 -> 25 -> 24 -> 23 -> 22 -> 21 -> 18 -> 17 -> 15 -> 14 -> 13 -> 12 -> 11 -> 10 -> 8 -> 7 (thirty-seven retired onto their seams)

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every production `.rs` file: everything under `src/` and `crates/`, minus whole-file
/// test modules (`tests.rs`).
fn production_rs_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    walk(&root.join("crates"), &mut out);
    out.sort();
    out
}

/// Index of the `mod tests {` line that follows a `#[cfg(test)]` attribute, i.e. where
/// production code ends in a file that carries an inline test module.
fn test_module_cutoff(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if (trimmed.starts_with("mod tests") || trimmed.starts_with("pub mod tests"))
            && trimmed.ends_with('{')
            && i > 0
            && lines[i - 1].contains("#[cfg(test)]")
        {
            return i;
        }
    }
    lines.len()
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with('*') || t.starts_with("/*")
}

/// Count production occurrences of `needle_pred` per file, relative to the repo root.
fn census(needle_pred: fn(&str) -> bool) -> BTreeMap<String, usize> {
    let root = repo_root();
    let mut counts = BTreeMap::new();
    for path in production_rs_files() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        // BLR1: production text is the file minus EVERY `#[cfg(test)]` item, found on
        // comment- and literal-blind code. The previous cut-off at the first `#[cfg(test)] mod
        // tests` hid production code that follows a test module (`src/runtime.rs` has some).
        let (production, _) = split_cfg_test(&text);
        let mut n = 0usize;
        for line in production.lines() {
            if !is_comment(line) && needle_pred(line) {
                n += 1;
            }
        }
        if n > 0 {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            counts.insert(rel, n);
        }
    }
    counts
}

fn with_cpu_line(line: &str) -> bool {
    line.contains(".with_cpu(")
}

/// `.with(` immediately followed by a closure — the broad `&mut KernelState` form.
fn with_broad_line(line: &str) -> bool {
    let Some(idx) = line.find(".with(") else {
        return false;
    };
    line[idx + ".with(".len()..].trim_start().starts_with('|')
}

fn state_lock_line(line: &str) -> bool {
    line.contains(".state.lock()")
}

fn expected_map(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(f, n)| ((*f).to_string(), *n)).collect()
}

fn diff_report(
    kind: &str,
    actual: &BTreeMap<String, usize>,
    expected: &BTreeMap<String, usize>,
) -> String {
    let mut out = format!("\n{kind} census drift:\n");
    for (file, n) in actual {
        match expected.get(file) {
            Some(e) if e == n => {}
            Some(e) => out.push_str(&format!("  CHANGED {file}: expected {e}, found {n}\n")),
            None => out.push_str(&format!(
                "  NEW     {file}: found {n} (not in the census)\n"
            )),
        }
    }
    for (file, e) in expected {
        if !actual.contains_key(file) {
            out.push_str(&format!("  GONE    {file}: expected {e}, found 0\n"));
        }
    }
    out.push_str(
        "\nA broad-lock callsite was added or removed without updating the Stage 204A census.\n\
         Adding one is allowed; leaving the census stale is not — Stage 204A is recorded\n\
         COMPLETE and stops being true the moment this drifts.\n\n\
         Update, in the same change:\n\
           * the pinned tables in tests/broad_lock_census_guard.rs\n\
           * doc/KERNEL_UNLOCK_AUDIT.md §1.2, §1.3, §1.4 and the §1.4a classification\n\
           * doc/KERNEL_LOCKING.md §0.2\n\
           * doc/STATUS.md §0 (broad-lock position table)\n",
    );
    out
}

#[test]
fn with_cpu_callsites_match_the_census() {
    let actual = census(with_cpu_line);
    let expected = expected_map(EXPECTED_WITH_CPU);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("SharedKernel::with_cpu", &actual, &expected)
    );
}

#[test]
fn broad_with_callsites_match_the_census() {
    let actual = census(with_broad_line);
    let expected = expected_map(EXPECTED_WITH_BROAD);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("broad SharedKernel::with", &actual, &expected)
    );
}

#[test]
fn raw_state_lock_sites_match_the_census() {
    let actual = census(state_lock_line);
    let expected = expected_map(EXPECTED_STATE_LOCK);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("raw state.lock()", &actual, &expected)
    );
}

#[test]
fn census_totals_are_internally_consistent() {
    let with_cpu: usize = census(with_cpu_line).values().sum();
    let with_broad_raw: usize = census(with_broad_line).values().sum();
    let state_lock: usize = census(state_lock_line).values().sum();
    let with_broad = with_broad_raw - THREAD_LOCAL_FALSE_POSITIVES;

    assert_eq!(with_cpu, AUDITED_WITH_CPU_TOTAL, "with_cpu total drifted");
    assert_eq!(
        with_broad, AUDITED_WITH_BROAD_TOTAL,
        "broad `with` total drifted (after subtracting {THREAD_LOCAL_FALSE_POSITIVES} \
         thread_local false positive)"
    );
    assert_eq!(
        state_lock, AUDITED_STATE_LOCK_TOTAL,
        "raw state.lock() total drifted — BLR1 deleted the broad lock; any raw `state.lock()` \
         means it, or an equivalent, came back"
    );
    assert_eq!(
        with_cpu + with_broad,
        AUDITED_ACQUISITION_TOTAL,
        "total broad-lock acquisition sites drifted (state.lock() bodies are deliberately \
         excluded — counting them would double-count the lock)"
    );
}

#[test]
fn stage_204a_classification_covers_every_runtime_required_site() {
    assert_eq!(
        CLASS_BOOT_ONLY + CLASS_TEST_ONLY + CLASS_OBSOLETE + CLASS_RUNTIME_REQUIRED,
        AUDITED_ACQUISITION_TOTAL,
        "the Stage 204A classification must account for every acquisition site; \
         boot-only + test-only + obsolete + runtime-required must equal the census total"
    );
}

#[test]
fn documents_publish_the_same_census_totals() {
    let doc = |name: &str| {
        fs::read_to_string(repo_root().join("doc").join(name))
            .unwrap_or_else(|_| panic!("doc/{name} must exist"))
    };
    let audit = doc("KERNEL_UNLOCK_AUDIT.md");
    let locking = doc("KERNEL_LOCKING.md");
    let status = doc("STATUS.md");

    let total = AUDITED_ACQUISITION_TOTAL.to_string();
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
        ("STATUS.md", &status),
    ] {
        assert!(
            text.contains(&format!("**{total}**")),
            "doc/{name} must publish the broad-lock acquisition total ({total}); \
             the census and the documents must not drift apart"
        );
    }

    // The classification counts are published only by the audit and the locking doc.
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
    ] {
        assert!(
            text.contains(&format!("**{CLASS_RUNTIME_REQUIRED}**")),
            "doc/{name} must publish the runtime-required count ({CLASS_RUNTIME_REQUIRED})"
        );
    }
}

/// Stage 199D (AArch64 blocker 3): the post-lock direct dispatch drain must not have added a
/// broad-lock acquisition site. The requirement is that the census stays at 50 or DECREASES —
/// a drain that reached for `with_cpu` to do its ASID activation or frame restore, as the
/// FutexWait and Yield drains legitimately do, would push it to 51 and fail here.
#[test]
fn stage199d_blocker3_added_no_broad_lock_acquisition_site() {
    assert!(
        AUDITED_ACQUISITION_TOTAL <= 50,
        "the broad-lock census must stay at 50 or decrease; it is {AUDITED_ACQUISITION_TOTAL}"
    );
    // The drain and the arch resume primitive it calls contain no acquisition of their own.
    let trap_entry = std::fs::read_to_string("src/arch/trap_entry.rs").expect("trap_entry.rs");
    let drain = trap_entry
        .split("if crate::kernel::direct_dispatch::is_pending(cpu_idx) {")
        .nth(1)
        .and_then(|s| s.split("\n    // Stage 192A").next())
        .expect("the direct dispatch drain");
    assert!(
        !drain.contains("with_cpu(") && !drain.contains("shared.with("),
        "the post-lock direct dispatch drain must acquire no broad lock"
    );
}

/// U9-TERMINAL-FINAL §5 — **the ratchet.** The census above is a per-file expectation, so it
/// catches a drift in a file it already lists. This catches the case the census cannot: a
/// production broad acquisition appearing in a file that was never on the list.
///
/// It is stated as an absolute rather than as a delta because the absolute is now available and
/// a delta would not be: with the two terminal dispatchers deleted, the correct number of
/// production acquisitions of the broad `SpinLock<KernelState>` — in any form, in any file — is
/// ZERO. Any nonzero measurement is a regression, and the failure names the files.
///
/// The three raw `self.state.lock()` sites are deliberately NOT included, and are reported
/// separately by [`raw_state_lock_sites_match_the_census`]. They are the BODIES of
/// `SharedKernel::lock` / `with` / `with_cpu` — the wrapper definitions every callsite goes
/// through. Deleting the last callsite does not delete the wrappers, and counting their bodies
/// as acquisitions would make the figure above un-reachable by construction.
#[test]
fn no_production_broad_acquisition_exists_anywhere() {
    let with_cpu = census(with_cpu_line);
    let with_broad = census(with_broad_line);

    assert!(
        with_cpu.is_empty(),
        "a production `SharedKernel::with_cpu` callsite was reintroduced: {with_cpu:?}. \
         U9-TERMINAL-FINAL §5 took this to zero by settling every supported trap outcome \
         pre-lock; a new acquisition must be justified and re-classified in \
         doc/KERNEL_UNLOCK_AUDIT.md §1.4a, not added silently"
    );

    // `with_broad` was already zero before this pass, and its one match is the documented
    // `thread_local!` false positive, which is not an acquisition.
    let real_broad: usize = with_broad.values().sum::<usize>() - THREAD_LOCAL_FALSE_POSITIVES;
    assert_eq!(
        real_broad, 0,
        "a production broad `SharedKernel::with` callsite was reintroduced: {with_broad:?}"
    );

    // Stated on the totals as well, so a census entry edited to match a reintroduced callsite
    // still fails here.
    assert_eq!(
        AUDITED_ACQUISITION_TOTAL, 0,
        "the audited acquisition total must stay at zero"
    );
}

/// U9-TERMINAL-FINAL §5 — the two bridges must not grow a *replacement* for the acquisition
/// they lost, which is the shape a closure this large is most likely to regress into.
///
/// Three distinct evasions are rejected by name, because each would satisfy the census above
/// while undoing what the census measures:
///
///  * a **raw lock** taken directly on `SharedKernel::state`, bypassing the wrappers the census
///    counts;
///  * a **hidden wrapper** — a local helper that performs the acquisition on the bridge's
///    behalf, which is how a deleted `with_cpu` comes back under another name;
///  * the settlement being replaced by a **blanket** error for every class at once, which is
///    what makes a trap outcome undiagnosable and is forbidden by §5 in the same sentence as
///    the deletion.
#[test]
fn neither_bridge_regrew_a_replacement_for_the_deleted_acquisition() {
    for file in ["src/arch/trap_entry.rs", "src/arch/riscv64/trap.rs"] {
        let src = fs::read_to_string(repo_root().join(file)).expect(file);
        let code: String = src
            .lines()
            .take(test_module_cutoff(&src.lines().collect::<Vec<_>>()))
            .filter(|line| !is_comment(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(".state.lock()"),
            "{file} must not take the broad lock directly — that is the same acquisition \
             under a different spelling, and it would not be counted by the census"
        );
        for hidden in ["with_cpu(", "shared.with(", "kernel.with("] {
            assert!(
                !code.contains(hidden),
                "{file} must not reach the broad lock through `{hidden}` — the terminal \
                 acquisition is deleted, not renamed"
            );
        }
    }

    // The settlement is PER CLASS. All five arms present means no class was collapsed into a
    // shared catch-all to manufacture the closure.
    let shared =
        fs::read_to_string(repo_root().join("src/arch/trap_entry.rs")).expect("trap_entry.rs");
    let settle = shared
        .split("pub(crate) fn settle_unowned_trap(")
        .nth(1)
        .and_then(|s| s.split("\n}\n").next())
        .expect("settle_unowned_trap");
    for arm in [
        "TrapEvent::Syscall",
        "TrapEvent::PageFault(_)",
        "TrapEvent::TimerInterrupt",
        "TrapEvent::ExternalInterrupt(_)",
        "TrapEvent::Unknown { .. }",
    ] {
        assert!(
            settle.contains(arm),
            "the unowned-trap settlement must keep a named arm for `{arm}` — a single error \
             for all five is the catch-all §5 forbids"
        );
    }
    // The `Syscall` arm settles on the USERSPACE channel. This is the half that is easiest to
    // lose: `KernelState::handle_trap` wrote `dispatch_syscall`'s error into the frame and
    // returned `Ok(())`, because every arch entry point treats `Err(TrapHandleError)` as a
    // fatal kernel halt. A settlement that returns the errno on the `Err` channel would have
    // the right VALUE and the wrong OUTCOME.
    assert!(
        settle.contains("frame.set_err(SyscallError::InvalidNumber.code())")
            && settle.contains("return Ok(());"),
        "the unowned `Syscall` settlement must write `InvalidNumber` into the caller's frame \
         and return `Ok(())`, exactly as the deleted broad arm did — not halt the kernel"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// BROAD-LOCK-RETIREMENT1 — the mechanism is deleted, and its reintroduction fails here.
//
// The census above counts ACQUISITIONS by their historical spelling. That stopped being enough
// once the acquisitions reached zero: the remaining risk is the MECHANISM coming back — a
// wrapper under another name, storage that wraps the whole state in a lock again, or the
// hosted-test helpers leaking into a production build. These checks separate four things, and
// the first three must be absent:
//
//   1. production broad ACQUISITIONS — any production expression that forms a whole-state
//      reference from the `SharedKernel` storage, by any spelling;
//   2. production broad-wrapper DEFINITIONS — any production function that lends, returns or
//      takes a closure over `&mut KernelState` out of shared storage (only the boot token may);
//   3. production broad-lock STORAGE — any lock type, cell or static holding a `KernelState`
//      other than the one lock-free cell;
//   4. explicitly TEST-ONLY support — present, and present only under exactly `#[cfg(test)]`.
//
// They read production text with every `#[cfg(test)]` item removed (comment- and literal-blind),
// and the compile probe at the bottom checks the hosted non-test BUILD, not the source.
// ═══════════════════════════════════════════════════════════════════════════════════════════

/// Same-length copy of `text` with comments and string/char literals blanked, so structural
/// scans see only code (and line numbers stay true).
fn code_mask(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let n = b.len();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for x in out.iter_mut().take(to.min(n)).skip(from) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    let mut i = 0;
    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            let j = text[i..].find('\n').map_or(n, |j| i + j);
            blank(&mut out, i, j);
            i = j;
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                if b[j] == b'/' && j + 1 < n && b[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && j + 1 < n && b[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if b[i] == b'r'
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
            && i + 1 < n
            && (b[i + 1] == b'"' || b[i + 1] == b'#')
        {
            let mut h = i + 1;
            while h < n && b[h] == b'#' {
                h += 1;
            }
            if h < n && b[h] == b'"' {
                let hashes = h - (i + 1);
                let close = format!("\"{}", "#".repeat(hashes));
                let end = text[h + 1..]
                    .find(&close)
                    .map_or(n, |e| h + 1 + e + close.len());
                blank(&mut out, i, end);
                i = end;
            } else {
                i += 1;
            }
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            blank(&mut out, i, j + 1);
            i = j + 1;
        } else if b[i] == b'\'' {
            // A char literal, or a lifetime.
            if i + 1 < n && b[i + 1] == b'\\' {
                let j = text[i + 2..].find('\'').map_or(n, |e| i + 2 + e);
                blank(&mut out, i, j + 1);
                i = j + 1;
            } else {
                let ch_len = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                if i + 1 + ch_len < n && b[i + 1 + ch_len] == b'\'' {
                    blank(&mut out, i, i + 2 + ch_len);
                    i += 2 + ch_len;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking preserves UTF-8")
}

/// `(production, test)`: every item, field, statement or expression attributed with exactly
/// `#[cfg(test)]` is test; everything else is production (newlines kept, so lines stay true).
/// A different cfg — `cfg(any(test, ..))`, a feature — is production, deliberately.
fn split_cfg_test(text: &str) -> (String, String) {
    let code = code_mask(text);
    let cb = code.as_bytes();
    let (mut prod, mut test, mut i) = (String::new(), String::new(), 0usize);
    let mut search = 0usize;
    while let Some(off) = code[search..].find("#[cfg(test)]") {
        let start = search + off;
        let mut depth = 0i32;
        let mut k = start + "#[cfg(test)]".len();
        while k < cb.len() {
            match cb[k] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break; // the enclosing construct closes; the attributed one ended
                    }
                    depth -= 1;
                    if depth == 0 && cb[k] == b'}' {
                        k += 1;
                        break;
                    }
                }
                b';' | b',' if depth == 0 => {
                    k += 1;
                    break;
                }
                _ => {}
            }
            k += 1;
        }
        prod.push_str(&text[i..start]);
        prod.push_str(&"\n".repeat(text[start..k].matches('\n').count()));
        test.push_str(&text[start..k]);
        i = k;
        search = k;
    }
    prod.push_str(&text[i..]);
    (prod, test)
}

/// Production code of every kernel-crate file (`src/`), comment- and literal-blind.
fn kernel_production_code() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    for path in production_rs_files() {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if !rel.starts_with("src/") || rel.contains("/overtaken_tests/") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("readable");
        out.push((rel, code_mask(&split_cfg_test(&text).0)));
    }
    out
}

/// Every production match of `pattern` (a small regex-free predicate over code), as `file:line`.
fn find_all(code: &[(String, String)], pred: impl Fn(&str) -> Vec<usize>) -> Vec<String> {
    let mut hits = Vec::new();
    for (rel, text) in code {
        for at in pred(text) {
            hits.push(format!("{rel}:{}", text[..at].matches('\n').count() + 1));
        }
    }
    hits
}

/// Offsets where `needle` occurs with arbitrary whitespace between its space-separated tokens.
fn token_seq(text: &str, needle: &str) -> Vec<usize> {
    let toks: Vec<&str> = needle.split(' ').collect();
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(off) = text[from..].find(toks[0]) {
        let start = from + off;
        let mut at = start + toks[0].len();
        let mut ok = true;
        for t in &toks[1..] {
            let rest = &text[at..];
            let trimmed = rest.trim_start();
            at += rest.len() - trimmed.len();
            if trimmed.starts_with(t) {
                at += t.len();
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            hits.push(start);
        }
        from = start + toks[0].len();
    }
    hits
}

const KS: [&str; 2] = ["KernelState", "crate::kernel::boot::KernelState"];

/// KIND 1 — no production expression forms a whole-state reference from shared storage.
#[test]
fn blr1_kind1_no_production_broad_acquisition_by_any_spelling() {
    let code = kernel_production_code();
    // The historical spellings, now on precise production text (multi-line forms included).
    let with_cpu = find_all(&code, |t| token_seq(t, ". with_cpu ("));
    assert!(with_cpu.is_empty(), "production `.with_cpu(`: {with_cpu:?}");
    let with_closure: Vec<String> = find_all(&code, |t| {
        let mut v = token_seq(t, ". with ( |");
        v.extend(token_seq(t, ". with ( move |"));
        v
    })
    .into_iter()
    .filter(|h| !h.starts_with("src/kernel/boot/orchestrator_state.rs:"))
    .collect();
    assert!(
        with_closure.is_empty(),
        "production `.with(|…|)`: {with_closure:?}"
    );
    // The spelling-independent form: the cell is opened in exactly ONE place (`state_ptr`), and
    // a whole-state reference is formed from it in exactly ONE place (the boot token's loan).
    let opens = find_all(&code, |t| token_seq(t, ". state . 0 . get ("));
    assert!(
        opens.len() == 1 && opens[0].starts_with("src/runtime.rs:"),
        "the state cell is opened only by `state_ptr`: {opens:?}"
    );
    let loans: Vec<String> = find_all(&code, |t| {
        let mut v = token_seq(t, "& mut * self . shared . state_ptr ( )");
        v.extend(token_seq(t, "& mut * self . state_ptr ( )"));
        v.extend(token_seq(t, "& * self . state_ptr ( )"));
        v.extend(token_seq(t, "& * self . shared . state_ptr ( )"));
        v
    });
    assert_eq!(
        loans.len(),
        1,
        "exactly one production whole-state loan may exist — `BootExclusive::state`: {loans:?}"
    );
    let runtime = &code
        .iter()
        .find(|(r, _)| r == "src/runtime.rs")
        .expect("runtime")
        .1;
    let loan_at = token_seq(runtime, "& mut * self . shared . state_ptr ( )")[0];
    let owner = &runtime[..loan_at];
    assert!(
        owner.rfind("pub fn state(&mut self) -> &mut KernelState")
            > owner.rfind("impl BootExclusive {"),
        "the one loan is `BootExclusive::state`"
    );
    // Projectors never reborrow the raw state pointer as a whole.
    let reborrow = find_all(&code, |t| {
        let mut v = token_seq(t, "& * state )");
        v.extend(token_seq(t, "& * state ;"));
        v.extend(token_seq(t, "& mut * state )"));
        v.extend(token_seq(t, "& mut * state ;"));
        v.extend(token_seq(t, "& * state ,"));
        v.extend(token_seq(t, "& mut * state ,"));
        v
    });
    assert!(
        reborrow.is_empty(),
        "a whole-state reborrow of a raw state pointer: {reborrow:?}"
    );
}

/// KIND 2 — no production function lends, returns or closes over the whole state, except the
/// boot token's `state(&mut self)`.
#[test]
fn blr1_kind2_no_production_broad_wrapper_definition() {
    let code = kernel_production_code();
    let mut closures = Vec::new();
    let mut returns = Vec::new();
    for k in KS {
        for f in ["FnOnce", "FnMut", "Fn"] {
            closures.extend(find_all(&code, |t| {
                token_seq(t, &format!("{f} ( & mut {k}"))
            }));
            closures.extend(find_all(&code, |t| token_seq(t, &format!("{f} ( & {k}"))));
        }
        closures.extend(find_all(&code, |t| {
            token_seq(t, &format!("fn ( & mut {k} )"))
        }));
        for r in [
            format!("-> & mut {k}"),
            format!("-> & {k}"),
            format!("-> & 'static mut {k}"),
            format!("-> & 'a mut {k}"),
            format!("-> Option < & mut {k}"),
            format!("-> Option < & 'static mut {k}"),
            format!("Guard < '_ , {k} >"),
            format!("Guard < 'a , {k} >"),
        ] {
            returns.extend(find_all(&code, |t| token_seq(t, &r)));
        }
    }
    assert!(
        closures.is_empty(),
        "a production closure / fn-pointer over the whole state is a broad wrapper under another \
         name: {closures:?}"
    );
    returns.sort();
    returns.dedup();
    assert_eq!(
        returns.len(),
        1,
        "exactly one production function may return a whole-state reference — \
         `BootExclusive::state`: {returns:?}"
    );
    // And the historical names are not defined in production on `SharedKernel`.
    let runtime = &code
        .iter()
        .find(|(r, _)| r == "src/runtime.rs")
        .expect("runtime")
        .1;
    for name in [
        "pub fn with<R>(",
        "pub fn with_cpu<R>(",
        "pub fn lock(&self)",
        "fn borrow_kernel_for_boot",
    ] {
        assert!(!runtime.contains(name), "production `{name}` is back");
    }
}

/// KIND 3 — no production storage wraps the whole state in a lock, a second cell or a static.
#[test]
fn blr1_kind3_no_production_broad_lock_storage() {
    let code = kernel_production_code();
    let mut locks = Vec::new();
    let mut cells = Vec::new();
    for k in KS {
        for l in ["SpinLock", "SpinLockIrq", "Mutex", "RwLock", "TicketLock"] {
            locks.extend(find_all(&code, |t| token_seq(t, &format!("{l} < {k} >"))));
        }
        cells.extend(find_all(&code, |t| {
            token_seq(t, &format!("UnsafeCell < {k} >"))
        }));
        cells.extend(find_all(&code, |t| {
            token_seq(t, &format!("UnsafeCell < core::mem::MaybeUninit < {k} >"))
        }));
        cells.extend(find_all(&code, |t| {
            token_seq(t, &format!("MaybeUninit < {k} >"))
        }));
        cells.extend(find_all(&code, |t| {
            token_seq(t, &format!("AtomicPtr < {k} >"))
        }));
    }
    assert!(
        locks.is_empty(),
        "a lock around the whole state is back: {locks:?}"
    );
    assert_eq!(
        cells.len(),
        1,
        "exactly one production cell may hold the state — `KernelStateCell`: {cells:?}"
    );
    assert!(cells[0].starts_with("src/runtime.rs:"), "{cells:?}");
    let runtime = &code
        .iter()
        .find(|(r, _)| r == "src/runtime.rs")
        .expect("runtime")
        .1;
    assert!(runtime.contains("struct KernelStateCell(core::cell::UnsafeCell<KernelState>);"));
    assert!(
        runtime.contains("pub struct SharedKernel {\n    state: KernelStateCell,"),
        "the shared kernel's storage is the lock-free cell"
    );
    // No static holds a whole state (the boot singleton is a `MaybeUninit<SharedKernel>`).
    let statics: Vec<String> = find_all(&code, |t| {
        let mut v = Vec::new();
        for (i, line) in t.lines().scan(0usize, |o, l| {
            let s = *o;
            *o += l.len() + 1;
            Some((s, l))
        }) {
            let l = line.trim_start();
            if (l.starts_with("static ")
                || l.starts_with("pub static ")
                || l.starts_with("pub(crate) static "))
                && l.split('=').next().is_some_and(|ty| {
                    ty.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .any(|w| w == "KernelState")
                })
            {
                v.push(i);
            }
        }
        v
    });
    assert!(
        statics.is_empty(),
        "a static holding the whole state: {statics:?}"
    );
}

/// KIND 4 — the hosted reference helpers exist, and only under exactly `#[cfg(test)]`.
#[test]
fn blr1_kind4_test_only_support_is_exactly_cfg_test() {
    let text = fs::read_to_string(repo_root().join("src/runtime.rs")).expect("runtime.rs");
    let (production, test) = split_cfg_test(&text);
    let (production, test) = (code_mask(&production), code_mask(&test));
    for item in [
        "pub fn with<R>(&self, f: impl FnOnce(&mut KernelState) -> R) -> R {",
        "pub fn with_cpu<R>(",
        "pub fn lock(&self) -> TestExclusiveGuard<'_> {",
        "pub struct TestExclusiveGuard<'a> {",
        "test_exclusive: SpinLock<()>,",
    ] {
        assert!(
            test.contains(item),
            "test-only support `{item}` must exist for the hosted suite"
        );
        assert!(
            !production.contains(item),
            "`{item}` leaked into a production build"
        );
    }
    // Attached to exactly `#[cfg(test)]` — not `cfg(any(test, ..))`, not a feature.
    assert!(text.contains("#[cfg(test)]\nimpl SharedKernel {\n    /// Test-only: run `f`"));
    // Informational record of what the hosted suite still drives through them.
    let mut callers = 0usize;
    for path in [
        "src/kernel/boot/tests.rs",
        "src/kernel/boot/overtaken_tests/tests.rs",
        "src/runtime.rs",
        "src/kernel/syscall_split.rs",
        "src/kernel/syscall.rs",
        "src/kernel/recv_waiter_split.rs",
    ] {
        let t = fs::read_to_string(repo_root().join(path)).expect(path);
        let t = code_mask(&split_cfg_test(&t).1);
        callers += token_seq(&t, ". with ( |").len()
            + token_seq(&t, ". with ( move |").len()
            + token_seq(&t, ". with_cpu (").len();
    }
    assert!(
        callers > 0,
        "the hosted reference tests still use the test-only helpers"
    );
}

/// The only pre-publication handle on the shared kernel is the trap-entry installation, one per
/// port, and every port's trap entry refuses it until publication.
#[test]
fn blr1_trap_installation_is_the_only_pre_publication_handle() {
    let code = kernel_production_code();
    let installs: Vec<String> =
        find_all(&code, |t| token_seq(t, ". shared_for_trap_installation ("));
    let mut files: Vec<&str> = installs
        .iter()
        .map(|h| h.split(':').next().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "src/arch/aarch64/boot.rs",
            "src/arch/riscv64/boot.rs",
            "src/arch/x86_64/boot.rs"
        ],
        "one trap-target installation per port, nowhere else: {installs:?}"
    );
    let refusals: Vec<String> = find_all(&code, |t| {
        token_seq(t, "refuse_shared_entry_during_boot_exclusive (")
    })
    .into_iter()
    .filter(|h| !h.starts_with("src/runtime.rs:"))
    .collect();
    let mut rfiles: Vec<&str> = refusals
        .iter()
        .map(|h| h.split(':').next().unwrap())
        .collect();
    rfiles.sort();
    assert_eq!(
        rfiles,
        [
            "src/arch/aarch64/boot.rs",
            "src/arch/riscv64/boot.rs",
            "src/arch/x86_64/descriptor_tables.rs"
        ],
        "every port's trap entry checks publication: {refusals:?}"
    );
    // `shared_static_ref` — callable by anyone — answers only after publication.
    let bootstrap =
        fs::read_to_string(repo_root().join("src/kernel/boot/bootstrap_state.rs")).unwrap();
    let ssr = bootstrap
        .split("pub fn shared_static_ref()")
        .nth(1)
        .expect("shared_static_ref")
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(ssr.contains("!crate::runtime::boot_state_published()"));
    // Secondaries are released only after publication, on every port.
    let x86 = fs::read_to_string(repo_root().join("src/arch/x86_64/boot.rs")).unwrap();
    let a64 = fs::read_to_string(repo_root().join("src/arch/aarch64/boot.rs")).unwrap();
    let rv = fs::read_to_string(repo_root().join("src/arch/riscv64/boot.rs")).unwrap();
    for (arch, src, release) in [
        (
            "x86_64",
            &x86,
            "release_deferred_ap_dispatch_at_publication()",
        ),
        ("aarch64", &a64, "note_bsp_boot_borrow_ended()"),
        ("riscv64", &rv, "release_secondaries_at_boot_borrow_end("),
    ] {
        let enter = src
            .split("pub fn enter_dispatched_user_task_if_available(\n    mut boot: crate::runtime::BootExclusive,")
            .nth(1)
            .unwrap_or_else(|| panic!("{arch}: first entry consumes the token"));
        let p = enter
            .find("boot.publish();")
            .unwrap_or_else(|| panic!("{arch}: publish"));
        let r = enter
            .find(release)
            .unwrap_or_else(|| panic!("{arch}: release"));
        assert!(
            p < r,
            "{arch}: secondaries are released only after publication"
        );
    }
    let smp = fs::read_to_string(repo_root().join("src/arch/x86_64/smp.rs")).unwrap();
    let live = smp
        .split("fn live_ap_user_dispatch(kernel: &mut KernelState, cpu: CpuId) {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    assert!(
        !live.contains("set_ap_dispatch_request(cpu, 1)") && !live.contains("write_icr("),
        "x86_64: the in-phase AP dispatcher must not wake the AP; the request is owed until \
         publication"
    );
}

/// Authority of every `KernelState` field (`doc/KERNEL_UNLOCKING.md` §BLR1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Authority {
    /// Mutable only under the named lock field of the same state.
    Lock(&'static str),
    /// A lock: an atomic protocol in its own right.
    IsLock,
    /// A lock that contains its data (`SpinLockIrq<SchedulerState>`).
    SelfLocked,
    /// Written during construction, read-only afterwards.
    ImmutableAfterConstruction,
    /// Touched only by whole-state code, which runs only in the boot exclusive phase (and in
    /// hosted tests); never projected for runtime use.
    ExclusivePhaseOnly,
}

const FIELD_AUTHORITY: &[(&str, Authority)] = &[
    ("kernel_aspace", Authority::ExclusivePhaseOnly),
    ("hal", Authority::ExclusivePhaseOnly),
    ("user_spaces", Authority::Lock("vm_state_lock")),
    ("scheduler_state", Authority::SelfLocked),
    ("ipc_state_lock", Authority::IsLock),
    ("driver_state_lock", Authority::IsLock),
    ("fault_state_lock", Authority::IsLock),
    ("restart_state_lock", Authority::IsLock),
    ("capability_state_lock", Authority::IsLock),
    ("telemetry_state_lock", Authority::IsLock),
    ("boot_config_state_lock", Authority::IsLock),
    ("vm_state_lock", Authority::IsLock),
    ("task_state_lock", Authority::IsLock),
    ("memory_state_lock", Authority::IsLock),
    ("ipc", Authority::Lock("ipc_state_lock")),
    ("capability", Authority::Lock("capability_state_lock")),
    (
        "tid_allocation_policy",
        Authority::ImmutableAfterConstruction,
    ),
    ("tid_allocation_cursor", Authority::Lock("task_state_lock")),
    (
        "spawn_reservation_generation",
        Authority::Lock("task_state_lock"),
    ),
    ("tcbs", Authority::Lock("task_state_lock")),
    ("task_classes", Authority::Lock("task_state_lock")),
    ("tls_restore_pending", Authority::Lock("task_state_lock")),
    ("robust_futex", Authority::Lock("task_state_lock")),
    ("memory", Authority::Lock("memory_state_lock")),
    ("drivers", Authority::Lock("driver_state_lock")),
    ("telemetry", Authority::Lock("telemetry_state_lock")),
    ("boot_config", Authority::Lock("boot_config_state_lock")),
    ("faults", Authority::Lock("fault_state_lock")),
    ("restart", Authority::Lock("restart_state_lock")),
];

/// Every field has exactly one recorded authority, and every raw projection out of the cell
/// respects it: a data field is projected only together with its own lock, a construction-only
/// field only for reading, and a whole-state-only field never.
#[test]
fn blr1_kernel_state_field_authority_is_total_and_projections_respect_it() {
    let boot_mod = fs::read_to_string(repo_root().join("src/kernel/boot/mod.rs")).unwrap();
    let body = code_mask(
        boot_mod
            .split("pub struct KernelState {")
            .nth(1)
            .expect("KernelState")
            .split("\n}\n")
            .next()
            .unwrap(),
    );
    let mut fields = Vec::new();
    for line in body.lines() {
        let l = line.trim();
        let l = l
            .strip_prefix("pub(crate) ")
            .or_else(|| l.strip_prefix("pub "))
            .unwrap_or(l);
        if let Some((name, _)) = l.split_once(':')
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            fields.push(name.to_string());
        }
    }
    let table: Vec<String> = FIELD_AUTHORITY
        .iter()
        .map(|(f, _)| (*f).to_string())
        .collect();
    assert_eq!(
        fields, table,
        "every KernelState field needs exactly one recorded authority, in declaration order — a \
         new field must be classified here and in doc/KERNEL_UNLOCKING.md §BLR1"
    );
    for (_, a) in FIELD_AUTHORITY {
        if let Authority::Lock(l) = a {
            assert_eq!(
                FIELD_AUTHORITY
                    .iter()
                    .find(|(f, _)| f == l)
                    .map(|(_, a)| *a),
                Some(Authority::IsLock),
                "{l} must be a lock field"
            );
        }
    }
    let authority = |f: &str| {
        FIELD_AUTHORITY
            .iter()
            .find(|(n, _)| *n == f)
            .map(|(_, a)| *a)
    };

    let orch =
        fs::read_to_string(repo_root().join("src/kernel/boot/orchestrator_state.rs")).unwrap();
    let orch = code_mask(&split_cfg_test(&orch).0);
    let mut audited = 0usize;
    for (at, _) in orch.match_indices("unsafe fn ") {
        let head = &orch[at + "unsafe fn ".len()..];
        let name: String = head
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.ends_with("_from_raw") {
            continue;
        }
        let rest = &head[name.len()..];
        let end = [
            "\n    pub(crate) unsafe fn ",
            "\n    pub(crate) fn ",
            "\n    pub fn ",
            "\n    fn ",
            "\n    unsafe fn ",
            "\n}\n",
        ]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
        let fbody = &rest[..end];
        let mut projected: Vec<(String, bool)> = Vec::new();
        for (macro_, mutable) in [
            ("addr_of_mut!((*state).", true),
            ("addr_of!((*state).", false),
        ] {
            for (p, _) in fbody.match_indices(macro_) {
                let f: String = fbody[p + macro_.len()..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                projected.push((f, mutable));
            }
        }
        assert!(
            !projected.is_empty() || fbody.contains("_from_raw("),
            "{name}: projects nothing"
        );
        let names: Vec<&str> = projected.iter().map(|(f, _)| f.as_str()).collect();
        for (f, mutable) in &projected {
            match authority(f).unwrap_or_else(|| panic!("{name}: unknown field {f}")) {
                Authority::Lock(l) => assert!(
                    names.contains(&l),
                    "{name}: projects `{f}` without its authority `{l}`"
                ),
                Authority::ImmutableAfterConstruction => assert!(
                    !mutable && fbody.contains(".read()"),
                    "{name}: `{f}` is construction-only and may only be read by value"
                ),
                Authority::ExclusivePhaseOnly => {
                    panic!("{name}: `{f}` is whole-state-only and must never be projected")
                }
                Authority::IsLock | Authority::SelfLocked => {}
            }
        }
        audited += 1;
    }
    assert!(
        audited >= 30,
        "the projection audit must cover the projector family ({audited})"
    );
}

/// `Send` / `Sync`: exactly the two justified impls on `SharedKernel`, the state's own `Send`
/// bound asserted, and nothing that would let the boot token or the cell cross CPUs.
#[test]
fn blr1_send_sync_impls_are_the_audited_ones() {
    let runtime = fs::read_to_string(repo_root().join("src/runtime.rs")).unwrap();
    let code = code_mask(&split_cfg_test(&runtime).0);
    assert_eq!(
        code.matches("unsafe impl Sync for SharedKernel {}").count(),
        1
    );
    assert_eq!(
        code.matches("unsafe impl Send for SharedKernel {}").count(),
        1
    );
    assert!(code.contains("assert_send::<KernelState>();"));
    for banned in [
        "impl Send for BootExclusive",
        "impl Sync for BootExclusive",
        "impl Send for KernelStateCell",
        "impl Sync for KernelStateCell",
        "impl Send for KernelState",
        "impl Sync for KernelState",
    ] {
        let all = fs::read_to_string(repo_root().join("src/kernel/boot/mod.rs")).unwrap() + &code;
        assert!(
            !all.contains(banned),
            "`{banned}` would let a whole-state path cross CPUs"
        );
    }
    assert!(
        code.contains("_not_send_sync: core::marker::PhantomData<*mut KernelState>,"),
        "the boot token stays !Send + !Sync"
    );
}

// ── The BUILD check ────────────────────────────────────────────────────────────────────────
//
// This file is an integration test: it links the hosted NON-test library, built exactly as a
// production consumer would see it. The probe below never runs; it only has to COMPILE. Each
// call resolves to the probe trait's method only if `SharedKernel` has NO inherent method of that
// name — if one comes back (in any form, with any signature) inherent resolution wins, the
// closure's `&mut ()` no longer fits, and this whole test binary stops compiling. The freestanding
// ports cannot do better than the hosted library here: `cfg(test)` is never set when they build.
mod blr1_build_probe {
    pub(super) trait Absent {
        fn with<R>(&self, _f: impl FnOnce(&mut ()) -> R) -> &'static str {
            "absent"
        }
        fn with_cpu<R>(&self, _cpu: (), _f: impl FnOnce(&mut ()) -> R) -> &'static str {
            "absent"
        }
        fn lock(&self, _probe: ()) -> &'static str {
            "absent"
        }
        fn borrow_kernel_for_boot(&self, _probe: ()) -> &'static str {
            "absent"
        }
    }
    impl Absent for yarm::runtime::SharedKernel {}

    #[allow(dead_code)]
    pub(super) fn probe(k: &yarm::runtime::SharedKernel) -> [&'static str; 4] {
        [
            k.with(|_: &mut ()| ()),
            k.with_cpu((), |_: &mut ()| ()),
            k.lock(()),
            k.borrow_kernel_for_boot(()),
        ]
    }
}

#[test]
fn blr1_hosted_non_test_library_has_no_whole_state_accessor() {
    // Reaching here means `blr1_build_probe::probe` compiled: none of the four names exists as an
    // inherent method of the non-test `SharedKernel`.
    let _ = blr1_build_probe::probe as fn(&yarm::runtime::SharedKernel) -> [&'static str; 4];
}
