// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! U9-RESIDUAL1 §3 — the cooperative yield as ONE policy over owners.
//!
//! # What was residual about NR 0
//!
//! Yield's queue-advancing *dispatch* left the broad lock long ago: Stage 192B (x86_64), 195G
//! (AArch64) and 196G (RISC-V) each re-enqueue the caller, clear `current`, publish a one-shot
//! per-CPU deferral and skip the in-lock dispatch, leaving the authoritative `dispatch_next_on` to
//! the post-lock drain. All three are default-on in production.
//!
//! What did **not** leave is the decision. `handle_yield` is reached only from
//! `dispatch_syscall`, which runs inside `shared.with_cpu(...)` — the terminal broad acquisition.
//! So every NR 0, including the ones that go on to defer, still enters that acquisition to decide
//! whether to defer. That is the residual edge this stage removes.
//!
//! # One policy, two acquisition adapters
//!
//! The three arch blocks in `KernelState::yield_current` were three copies of one decision, and a
//! split route written beside them would have been a fourth. [`run_yield_transaction`] is the
//! single copy. Both callers drive it:
//!
//! * `SharedYieldOwners` — the split route, each method one domain-local acquisition with the
//!   broad `SpinLock<KernelState>` released;
//! * `BroadYieldOwners` — `yield_current`'s deferral half, each method a direct field access under
//!   the broad guard the caller already holds.
//!
//! The per-architecture eligibility that genuinely differs — x86_64 gates on `d6_genuine_enabled`
//! and has no BSP requirement, AArch64 and RISC-V require the BSP, RISC-V additionally refuses
//! while any of three deferrals is pending — lives in [`yield_deferral_arch_gate`], one function
//! with `cfg` arms. Unifying those conditions would have *changed* production behaviour on at
//! least one architecture, which is not what "one policy" means here.
//!
//! # Order, and why every decline is free
//!
//! ```text
//!   pre-lock  the outgoing task is this CPU's current                    (decline: free)
//!   pre-lock  the per-architecture deferral gate                         (decline: free)
//!   ——        the queue-advance ADMISSION (drainer, one dispatcher, us)  (decline: free)
//!   ——        RESERVE the one-shot Yield deferral                        (decline: free)
//!   rank 2    Running -> Runnable, the exact typed transition            (decline: free)
//!   rank 1    re-enqueue at tail + clear `current`                       (decline: ROLLED BACK)
//!   ——        return DeferredToDrain; the EXISTING drain selects and applies the next task
//! ```
//!
//! Only the rank-1 step can fail after something has been written, and the failure is *exactly*
//! reversible: `preempt_reenqueue_only` restores `current` itself before returning `None`, and this
//! transaction then applies [`TaskTransition::RollbackPreemptOutgoing`] — the named inverse of the
//! rank-2 step — and releases the reservation. So **every** [`YieldDecline`] leaves the world
//! byte-for-byte as it was found, which is what makes handing back to the unchanged broad path
//! safe rather than merely convenient.
//!
//! There is no broad fallback *after* the publication, and none is needed: past the rank-1 step the
//! caller is queued exactly once, `current` is empty, and the drain that consumes the deferral is
//! the same one FutexWait and the terminal fault already share.
//!
//! # U9-YIELD2 §1 — NR 0 is NOT closed, and this file is where the honest statement belongs
//!
//! Every [`YieldDecline`] above makes `try_split_yield_into_frame` answer `NotHandled`, and
//! `NotHandled` is an entry into the terminal broad dispatcher. U9-RESIDUAL1 §5 reported "no
//! refusal occurred in nine qualifying boots" and was careful not to claim source totality; the
//! matrix row it wrote nonetheless reads as a closed family, so it is corrected here.
//!
//! Of the six declines, four are unreachable for a **userspace** NR 0 by construction (`NoCurrent`,
//! `DeferralHeld`, `RouteNotAdmitted::CpuOutOfRange` and `RouteNotAdmitted::NoTrapDrainer`), and
//! two more are unreachable only on the single-dispatcher default. Two are genuinely reachable in
//! supported configurations, and both are witnessed or constructible rather than inferred:
//!
//! * **`RouteNotAdmitted::MultiDispatcher`** under the default-off `yarm.ap_user_dispatch` knob.
//!   Live at base: `YIELD_SPLIT_REFUSED cpu=1 reason=multi_cpu` in
//!   `scripts/qemu-x86_64-ap-saved-return-smoke.sh`, on the AP, for a real userspace `Yield`.
//! * **`ArchGateOff`** on x86_64 under `yarm.d6_switch_proof` / `yarm.d6_switch_a`, where
//!   `d6_genuine_enabled()` is false — and where the Yield DRAIN in `arch/trap_entry.rs` is gated
//!   off by the same two predicates, so no deferral could be consumed even if one were published.
//!
//! Neither can be closed by this route. The reason is not Yield's: it is that
//! `SharedKernel::queue_advance_select_step_split` — the ONE selection owner every queue-advancing
//! drain uses — authenticates `sched.current_cpu == cpu` before dequeuing, and `current_cpu` is a
//! single global field that any CPU's `with_cpu` rebinds. See `doc/KERNEL_UNLOCKING.md`, U9-YIELD2
//! §2/§3, for the derivation and the exact missing contract.

use crate::kernel::scheduler::CpuId;

/// What a committed yield produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct YieldOutcome {
    /// The task that was re-enqueued and taken off `current`. The drain will select the next task
    /// — possibly this one again, when it is the only runnable task.
    pub(crate) outgoing: u64,
}

/// Why the deferral was not taken. **Every variant is pre-mutation**: the caller may run the
/// unchanged in-lock path, and a split caller may answer `NotHandled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YieldDecline {
    /// This CPU has no current task, so there is nothing to yield.
    NoCurrent,
    /// The per-architecture deferral gate is closed for this build or this CPU.
    ArchGateOff,
    /// Some route already holds this CPU's one-shot Yield deferral.
    DeferralHeld,
    /// The topology admission refused. Carries WHICH condition, because the delivered in-lock
    /// vocabulary distinguished them and a refusal that cannot be told apart in a log cannot be
    /// diagnosed.
    RouteNotAdmitted(crate::kernel::boot::TerminalAdmissionRefusal),
    /// The caller is not `Running`. The in-lock path answers `TaskMissing` for this; declining
    /// lets it produce exactly that, from the same transition owner.
    NotRunning,
    /// The scheduler refused the re-enqueue. `current` was restored by the primitive and the
    /// rank-2 transition was rolled back, so nothing is left half-done.
    ReenqueueRefused,
}

// A `YieldDecline` names itself through [`legacy_reason`], and only through it. Two spellings of
// one vocabulary is how a live log comes to disagree with itself: the delivered in-lock blocks
// already had reason strings, and `legacy_reason` is exactly those strings.

/// Which `Running → Runnable` the rank-2 step actually applied — and therefore what its inverse
/// must undo.
///
/// The ordinary transition writes a field; the idle-only twin is `Runnable → Runnable` and writes
/// nothing. An inverse that did not know which had run would promote a never-`Running` idle task to
/// `Running` on the rollback path — a corruption invented by the undo. So the forward step reports
/// what it did, and the inverse is chosen by that report rather than guessed from the current
/// status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreemptApplied {
    /// `Running → Runnable` on an ordinary task. Undone by `RollbackPreemptOutgoing`.
    Ordinary,
    /// `Runnable → Runnable` on [`crate::kernel::task_transition::IDLE_TID`]. Nothing was written,
    /// so nothing is undone.
    IdleNoop,
}

/// The acquisitions [`run_yield_transaction`] needs. Each method is ONE acquisition of ONE rank in
/// the split adapter, and one direct field access in the broad adapter. No method decides anything.
pub(crate) trait YieldOwners {
    /// rank 1 — this CPU's current task, read authoritatively from scheduler state.
    fn current_tid_on_cpu(&self, cpu: CpuId) -> Option<u64>;
    /// Is this CPU the bootstrap CPU? Part of the AArch64/RISC-V gate.
    fn is_bootstrap_cpu(&self, cpu: CpuId) -> bool;
    /// Does any route already hold a deferral that would collide with this one? The set differs by
    /// architecture and is asked by [`yield_deferral_arch_gate`], never re-derived here.
    fn colliding_deferral_pending(&self, cpu: CpuId) -> bool;
    /// The topology ADMISSION — drainer, at most one dispatching CPU, and that CPU is this one.
    /// Non-mutating, and it reports which condition refused.
    fn queue_advance_admission(
        &self,
        cpu: CpuId,
    ) -> Result<(), crate::kernel::boot::TerminalAdmissionRefusal>;

    /// Reserve the ONE Yield deferral for this CPU. `false` means another route holds it.
    fn reserve_yield_deferral(&mut self, cpu: CpuId, outgoing: u64) -> bool;
    /// Release it. Only ever called on a path that mutated nothing else.
    fn release_yield_deferral(&mut self, cpu: CpuId);

    /// rank 2 — the EXACT `Running → Runnable` transition, with the idle-only twin. Reports WHICH
    /// of the two it applied, because that is what its inverse has to undo.
    fn preempt_outgoing(&mut self, tid: u64) -> Option<PreemptApplied>;
    /// rank 2 — the exact inverse of what `preempt_outgoing` reported. Called only to undo a
    /// transition this same transaction applied.
    fn rollback_preempt_outgoing(&mut self, tid: u64, applied: PreemptApplied) -> bool;

    /// rank 1 — re-enqueue the current task at its priority tail and clear `current`, as one
    /// scheduler operation. `None` means the scheduler refused and **restored `current` itself**.
    fn reenqueue_and_clear_current(&mut self, cpu: CpuId) -> Option<u64>;
}

// Telemetry is deliberately NOT part of this trait. `scheduler_yield_calls` counts *yields*, not
// deferrals, and the two routes reach that count at different points for a reason: the broad path
// counts on entry, before it knows whether it will defer, because it counts the in-lock yields too;
// the split route counts only what it commits, and a decline is counted by the broad path that then
// runs. Either way exactly one increment per NR 0, and the broad path's existing order is unchanged.

/// U9-RESIDUAL1 §3 — the per-architecture deferral gate, in ONE place.
///
/// These three conditions are not a policy choice this stage gets to make: they are the conditions
/// the delivered in-lock code already applies, and changing any of them would change production
/// behaviour on that architecture. They are reproduced here exactly, so that the broad and split
/// adapters consult one definition instead of two copies drifting apart.
///
/// * **x86_64** (Stage 192B) — `d6_genuine_enabled()`, no bootstrap-CPU requirement, refuses while
///   a Yield deferral is already pending.
/// * **AArch64** (Stage 195G) — no knob, bootstrap CPU only, refuses while a Yield deferral is
///   already pending.
/// * **RISC-V** (Stage 196G) — no knob, bootstrap CPU only, and refuses while a Yield, FutexWait
///   **or** 196D foundation deferral is pending, because all three are drained by the same tail.
///
/// The trap-drainer and single-dispatcher conditions are deliberately NOT here: they belong to the
/// queue-advance admission, which owns them for every queue-advancing route.
pub(crate) fn yield_deferral_arch_gate<O: YieldOwners>(
    owners: &O,
    cpu: CpuId,
) -> Result<(), YieldDecline> {
    #[cfg(target_arch = "x86_64")]
    {
        if !crate::kernel::boot::d6_genuine_enabled() {
            return Err(YieldDecline::ArchGateOff);
        }
    }
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    {
        if !owners.is_bootstrap_cpu(cpu) {
            return Err(YieldDecline::ArchGateOff);
        }
    }
    // Referenced on every architecture so the signature is not conditionally unused.
    let _ = owners.is_bootstrap_cpu(cpu);
    if owners.colliding_deferral_pending(cpu) {
        return Err(YieldDecline::DeferralHeld);
    }
    Ok(())
}

/// U9-RESIDUAL1 §3 — defer this CPU's yield to the existing post-lock drain, or decline having
/// mutated nothing.
///
/// See the module documentation for the order and for why every decline is free.
pub(crate) fn run_yield_transaction<O: YieldOwners>(
    owners: &mut O,
    cpu: CpuId,
) -> Result<YieldOutcome, YieldDecline> {
    // (1) The self is defined by the scheduler, never by an argument.
    let outgoing = owners
        .current_tid_on_cpu(cpu)
        .ok_or(YieldDecline::NoCurrent)?;

    // (2) The per-architecture gate, and the colliding-deferral check it owns.
    yield_deferral_arch_gate(owners, cpu)?;

    // (3) ADMISSION. The drainer, the dispatching-CPU count and the bound dispatch CPU — the same
    // owner every queue-advancing route consults. Non-mutating, so this is still free.
    owners
        .queue_advance_admission(cpu)
        .map_err(YieldDecline::RouteNotAdmitted)?;

    // (4) RESERVE the one-shot deferral BEFORE any mutation. Holding it is what guarantees the
    // drain will run; a route that mutated first and then failed to reserve would have advanced
    // the queue with nothing to consume the advance.
    if !owners.reserve_yield_deferral(cpu, outgoing) {
        return Err(YieldDecline::DeferralHeld);
    }

    // (5) rank 2 — the exact `Running → Runnable` transition. Fail-closed: a refusal writes no
    // field, so releasing the reservation restores the entry state exactly.
    let Some(applied) = owners.preempt_outgoing(outgoing) else {
        owners.release_yield_deferral(cpu);
        return Err(YieldDecline::NotRunning);
    };

    // (6) rank 1 — re-enqueue at the priority tail and clear `current`, as ONE scheduler
    // operation. This is the only step that can fail after something was written, and its failure
    // is exactly reversible: the primitive restores `current` before returning `None`, and the
    // rank-2 write above has a named inverse.
    let Some(reenqueued) = owners.reenqueue_and_clear_current(cpu) else {
        // Undo (5), then (4). Order matters: the reservation must outlive the rollback, or a
        // concurrent route could take it and observe a caller that is briefly `Runnable` and
        // current at once.
        let rolled_back = owners.rollback_preempt_outgoing(outgoing, applied);
        owners.release_yield_deferral(cpu);
        debug_assert!(
            rolled_back,
            "the inverse of a transition this transaction just applied must succeed"
        );
        return Err(YieldDecline::ReenqueueRefused);
    };
    debug_assert_eq!(
        reenqueued, outgoing,
        "the scheduler must re-enqueue the exact task this transaction preempted"
    );

    Ok(YieldOutcome { outgoing })
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// The two adapters. Neither contains policy: each is a set of acquisitions around the state the
// transaction above decides over, so the broad NR 0 and the split NR 0 run the SAME decision.
// ═══════════════════════════════════════════════════════════════════════════════════════════

/// The SPLIT owner. Each method takes exactly the one domain lock its answer needs, with the broad
/// `SpinLock<KernelState>` already released.
///
/// Constructed only by `syscall_split::try_split_yield_into_frame`, which is stubbed out under
/// `hosted-dev` — the hosted suite exercises the BROAD adapter and the transaction directly, and
/// the split route is proven live. The allow is scoped to that build, so a dead construction in a
/// production build would still be reported.
#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) struct SharedYieldOwners<'a> {
    pub(crate) shared: &'a crate::runtime::SharedKernel,
}

impl YieldOwners for SharedYieldOwners<'_> {
    fn current_tid_on_cpu(&self, cpu: CpuId) -> Option<u64> {
        self.shared.current_tid_authoritative(cpu)
    }
    fn is_bootstrap_cpu(&self, cpu: CpuId) -> bool {
        cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
    }
    fn colliding_deferral_pending(&self, cpu: CpuId) -> bool {
        colliding_deferral_pending_for(cpu.0 as usize)
    }
    fn queue_advance_admission(
        &self,
        cpu: CpuId,
    ) -> Result<(), crate::kernel::boot::TerminalAdmissionRefusal> {
        // U9-DISPATCH-CPU1 §3: NR 0 is AUTHORITY-BOUND. Its drain — the post-lock Yield drain on
        // all three architectures — now selects through `queue_advance_acquire_incoming_split`,
        // which authenticates this trap's own `DispatchAuthority` rather than the ambient
        // `sched.current_cpu`. So the two conditions that existed to keep that ambient binding
        // stable, `MultiDispatcher` and `NotDispatchCpu`, no longer protect anything for this
        // family and are not asked. The drainer condition is unchanged and still asked.
        //
        // Every other family keeps `AmbientBound` until its own gate is individually justified.
        self.shared.split_terminal_route_admission(
            cpu,
            crate::runtime::TerminalRouteTopology::AuthorityBound,
        )
    }

    fn reserve_yield_deferral(&mut self, cpu: CpuId, outgoing: u64) -> bool {
        crate::kernel::boot::yield_dispatch_try_defer(cpu.0 as usize, outgoing)
    }
    fn release_yield_deferral(&mut self, cpu: CpuId) {
        crate::kernel::boot::yield_dispatch_clear(cpu.0 as usize);
    }

    fn preempt_outgoing(&mut self, tid: u64) -> Option<PreemptApplied> {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| apply_preempt_outgoing_locked(tcbs, tid))
    }
    fn rollback_preempt_outgoing(&mut self, tid: u64, applied: PreemptApplied) -> bool {
        self.shared
            .with_task_tcbs_split_mut(|tcbs| apply_rollback_preempt_locked(tcbs, tid, applied))
    }

    fn reenqueue_and_clear_current(&mut self, cpu: CpuId) -> Option<u64> {
        self.shared.with_scheduler_split_mut(|sched| {
            crate::kernel::boot::kernel_mut(&mut sched.scheduler)
                .preempt_reenqueue_only_on(cpu)
                .map(|t| t.0)
        })
    }
}

/// The BROAD owner. Each method is a direct access under the guard the caller already holds, so
/// `KernelState::yield_current` runs the same decision without acquiring anything twice.
pub(crate) struct BroadYieldOwners<'a> {
    pub(crate) kernel: &'a mut crate::kernel::boot::KernelState,
}

impl YieldOwners for BroadYieldOwners<'_> {
    fn current_tid_on_cpu(&self, _cpu: CpuId) -> Option<u64> {
        // The broad path's `cpu` IS `self.kernel.current_cpu()`, so this is the same read
        // `yield_current` has always performed.
        self.kernel.current_tid()
    }
    fn is_bootstrap_cpu(&self, cpu: CpuId) -> bool {
        cpu.0 == crate::arch::platform_constants::BOOTSTRAP_CPU_ID
    }
    fn colliding_deferral_pending(&self, cpu: CpuId) -> bool {
        colliding_deferral_pending_for(cpu.0 as usize)
    }
    fn queue_advance_admission(
        &self,
        cpu: CpuId,
    ) -> Result<(), crate::kernel::boot::TerminalAdmissionRefusal> {
        // U9-DISPATCH-CPU1 §3: the broad adapter moves to AUTHORITY-BOUND with the split one, and
        // it has to. The whole point of one transaction driven by two adapters is that the two
        // routes cannot come to disagree about when a yield may be deferred; leaving the
        // dispatching-CPU count here would have made the broad NR 0 refuse a topology the split
        // NR 0 admits, which is precisely the drift U9-RESIDUAL1 §3 extracted this transaction to
        // prevent.
        //
        // It is also correct on its own terms. The deferral this adapter publishes is consumed by
        // the SAME post-lock drain — the broad `yield_current` runs inside a trap, so that trap's
        // drain is what settles it — and that drain now authenticates the trap's own authority.
        // The two conditions that were dropped, `MultiDispatcher` and `NotDispatchCpu`, existed
        // only to keep the ambient binding stable for it.
        //
        // `current_cpu == cpu` was trivially true here anyway: `yield_current` derives `cpu` from
        // `self.current_cpu()`, which IS the scheduler's bound dispatcher.
        //
        // This adapter holds a `&mut KernelState` and so cannot reach
        // `SharedKernel::split_terminal_route_admission`; it calls the SAME route-local function
        // that method calls, rather than keeping a hand-written copy of the two conditions.
        crate::runtime::terminal_route_admission_authority_bound(cpu)
    }

    fn reserve_yield_deferral(&mut self, cpu: CpuId, outgoing: u64) -> bool {
        crate::kernel::boot::yield_dispatch_try_defer(cpu.0 as usize, outgoing)
    }
    fn release_yield_deferral(&mut self, cpu: CpuId) {
        crate::kernel::boot::yield_dispatch_clear(cpu.0 as usize);
    }

    fn preempt_outgoing(&mut self, tid: u64) -> Option<PreemptApplied> {
        self.kernel
            .with_tcbs_mut(|tcbs| apply_preempt_outgoing_locked(tcbs, tid))
    }
    fn rollback_preempt_outgoing(&mut self, tid: u64, applied: PreemptApplied) -> bool {
        self.kernel
            .with_tcbs_mut(|tcbs| apply_rollback_preempt_locked(tcbs, tid, applied))
    }

    fn reenqueue_and_clear_current(&mut self, _cpu: CpuId) -> Option<u64> {
        self.kernel.preempt_reenqueue_current_cpu()
    }
}

/// The deferrals that collide with a Yield deferral on this architecture.
///
/// x86_64 and AArch64 share one tail with the FutexWait drain but sequence the two cells
/// independently, so only a pending Yield collides. RISC-V drains all three from one tail, so a
/// pending FutexWait or 196D foundation deferral collides too. This reproduces exactly what the
/// delivered in-lock blocks check — it is not a new rule.
fn colliding_deferral_pending_for(cpu_idx: usize) -> bool {
    if cpu_idx >= crate::kernel::scheduler::MAX_CPUS {
        return true;
    }
    if crate::kernel::boot::yield_dispatch_is_deferred(cpu_idx) {
        return true;
    }
    #[cfg(target_arch = "riscv64")]
    {
        if crate::kernel::boot::futex_wait_dispatch_is_deferred(cpu_idx)
            || crate::kernel::boot::riscv_queue_switch_foundation_is_deferred(cpu_idx)
        {
            return true;
        }
    }
    false
}

/// `Running → Runnable` for the outgoing task, with the idle-only twin — the EXACT transition the
/// in-lock path has always applied, through the same owner. Reports which twin ran.
pub(crate) fn apply_preempt_outgoing_locked(
    tcbs: &mut [Option<crate::kernel::task::ThreadControlBlock>],
    tid: u64,
) -> Option<PreemptApplied> {
    use crate::kernel::task_transition::{TaskTransition, apply_task_transition};
    match apply_task_transition(tcbs, tid, None, TaskTransition::PreemptOutgoing) {
        Ok(_) => Some(PreemptApplied::Ordinary),
        Err(first) => {
            // Idle-only fallback: the idle task is made `current` by the rank-1 scheduler without a
            // mark-running step, so preempting it out is `Runnable → Runnable`. Restricted to
            // `IDLE_TID` inside the primitive, so an ordinary task cannot reach it.
            match apply_task_transition(tcbs, tid, None, TaskTransition::PreemptOutgoingIdle) {
                Ok(_) => Some(PreemptApplied::IdleNoop),
                Err(_) => {
                    crate::kernel::task_transition::log_transition_refusal(
                        "yield_txn/preempt_outgoing",
                        tid,
                        TaskTransition::PreemptOutgoing,
                        first,
                    );
                    None
                }
            }
        }
    }
}

/// The exact inverse of what [`apply_preempt_outgoing_locked`] reported.
///
/// `IdleNoop` wrote nothing, so its inverse writes nothing. Undoing it with
/// `RollbackPreemptOutgoing` would be worse than a no-op: `Runnable → Running` is a legal
/// transition for the idle task's status, so the write would SUCCEED and leave a never-`Running`
/// idle task marked `Running` — a corruption invented by the undo itself.
pub(crate) fn apply_rollback_preempt_locked(
    tcbs: &mut [Option<crate::kernel::task::ThreadControlBlock>],
    tid: u64,
    applied: PreemptApplied,
) -> bool {
    use crate::kernel::task_transition::{TaskTransition, apply_task_transition};
    if applied == PreemptApplied::IdleNoop {
        return true;
    }
    apply_task_transition(tcbs, tid, None, TaskTransition::RollbackPreemptOutgoing)
        .map_err(|refusal| {
            crate::kernel::task_transition::log_transition_refusal(
                "yield_txn/rollback_preempt_outgoing",
                tid,
                TaskTransition::RollbackPreemptOutgoing,
                refusal,
            );
        })
        .is_ok()
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// The live vocabulary. ONE owner, three architectures — because a log reader must see exactly the
// strings the delivered in-lock blocks emitted, and because the split route must be indistinguishable
// from the broad one in a log except where it is genuinely different.
// ═══════════════════════════════════════════════════════════════════════════════════════════

/// Attest a committed deferral, in this architecture's exact delivered vocabulary.
///
/// x86_64 (192B) logs `YIELD_DISPATCH_*` with `tid=`; AArch64 (195G) logs `AARCH64_YIELD_DISPATCH_*`
/// with `tid=` and the one-shot default-on notice; RISC-V (196G) logs `RISCV_YIELD_DISPATCH_*` with
/// `outgoing=` and its own default-on notice. Those differences are not tidiness — the three
/// post-lock drains and the live oracles each match their own prefix, so unifying the strings would
/// silently zero every architecture's Yield evidence.
pub(crate) fn log_yield_deferred(cpu: CpuId, outgoing: u64) {
    let _ = (cpu, outgoing);
    #[cfg(target_arch = "x86_64")]
    {
        crate::yarm_log!("YIELD_DISPATCH_DEFER_BEGIN cpu={} tid={}", cpu.0, outgoing);
        crate::yarm_log!("YIELD_DISPATCH_REENQUEUE_OK cpu={} tid={}", cpu.0, outgoing);
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::kernel::boot::maybe_log_yield_default_on();
        crate::yarm_log!(
            "AARCH64_YIELD_DISPATCH_DEFER_BEGIN cpu={} tid={}",
            cpu.0,
            outgoing
        );
        crate::yarm_log!(
            "AARCH64_YIELD_DISPATCH_REENQUEUE_OK cpu={} tid={}",
            cpu.0,
            outgoing
        );
    }
    #[cfg(target_arch = "riscv64")]
    {
        crate::kernel::boot::maybe_log_riscv_yield_retire_default_on();
        crate::yarm_log!(
            "RISCV_YIELD_DISPATCH_DEFER_BEGIN cpu={} outgoing={}",
            cpu.0,
            outgoing
        );
        crate::yarm_log!(
            "RISCV_YIELD_DISPATCH_REENQUEUE_OK cpu={} outgoing={}",
            cpu.0,
            outgoing
        );
    }
}

/// Attest a decline, in this architecture's exact delivered vocabulary.
///
/// The reason strings are the delivered ones — `no_trap_drainer`, `multi_cpu`, `not_bsp`,
/// `already_deferred`, `reenqueue_failed` — mapped from [`YieldDecline`] so no attribution is lost.
pub(crate) fn log_yield_declined(_cpu: CpuId, outgoing: u64, decline: YieldDecline) {
    let reason = legacy_reason(decline);
    let _ = (outgoing, reason);
    #[cfg(target_arch = "x86_64")]
    crate::yarm_log!(
        "YIELD_INLOCK_DISPATCH_FALLBACK reason={} tid={}",
        reason,
        outgoing
    );
    #[cfg(target_arch = "aarch64")]
    crate::yarm_log!(
        "AARCH64_YIELD_INLOCK_DISPATCH_FALLBACK reason={} tid={}",
        reason,
        outgoing
    );
    #[cfg(target_arch = "riscv64")]
    crate::yarm_log!(
        "RISCV_YIELD_DISPATCH_FALLBACK reason={} tid={}",
        reason,
        outgoing
    );
}

/// [`YieldDecline`] in the delivered blocks' own words.
///
/// `ArchGateOff` maps to the reason each architecture's gate actually is: x86_64's is the
/// `d6_genuine` predicate, AArch64's and RISC-V's is the bootstrap-CPU requirement, and the
/// delivered code called the latter `not_bsp`.
pub(crate) const fn legacy_reason(decline: YieldDecline) -> &'static str {
    match decline {
        YieldDecline::NoCurrent => "no_current",
        YieldDecline::ArchGateOff => {
            if cfg!(target_arch = "x86_64") {
                "d6_genuine_off"
            } else {
                "not_bsp"
            }
        }
        YieldDecline::DeferralHeld => "already_deferred",
        YieldDecline::RouteNotAdmitted(why) => why.marker(),
        YieldDecline::NotRunning => "not_running",
        YieldDecline::ReenqueueRefused => "reenqueue_failed",
    }
}
