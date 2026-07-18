// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{
    IpcEndpointRecvResult, IpcEndpointSendResult, IpcEndpointSplitRejectReason, IpcFastpathResult,
    IpcSubsystem, KernelError, KernelState, MAX_ENDPOINT_SENDER_WAITERS, MAX_IRQ_LINES,
    NotificationObject, ReceiverWaiterIdentity, ReplyCapRecord, ReplyRecordSetOutcome,
    SenderWaiter, kernel_mut, kernel_ref, map_ipc_error,
};
use crate::kernel::capabilities::{CapId, CapObject, CapRights, Capability};
use crate::kernel::ipc::{Endpoint, EndpointMode, Message, ThreadId};
use crate::kernel::syscall::complete_blocked_recv_for_waiter;
use crate::kernel::task::{RecvAbiVariant, TaskStatus, WaitReason};
use crate::kernel::vm::Asid;
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, PROC_OP_EXECUTE_RESTART,
};

/// D-NEXT-1 PR-A (Stage 111): rank-ordered phase plan threaded through the
/// `block_current_on_receive_with_deadline` sequence. Carrying a typed plan
/// instead of a bare `ThreadId` makes each phase's pre/post condition
/// explicit at the type level and gives tests a stable seam to assert phase
/// completion independently of the final outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecvBlockPhasePlan {
    blocked_tid: ThreadId,
    endpoint_idx: usize,
    recv_cap: CapId,
    /// Stage 198E3B2B2: the blocking receiver's captured ASID, threaded from the task phase (rank 2)
    /// so the ipc phase (rank 3) can publish the COMPLETE generation-bearing waiter identity.
    receiver_asid: Asid,
}

impl IpcSubsystem {
    /// Stage 198E3B2B2 — endpoint RECEIVE-waiter slot accessors. The slot stores a complete
    /// [`ReceiverWaiterIdentity`]; every authority operation (claim / clear / cleanup / restore) goes
    /// through these, so numeric TID alone can never authorize a waiter mutation.

    /// The present receiver waiter's TID (for waking / telemetry only — never an authority check).
    pub(crate) fn endpoint_waiter_tid(&self, idx: usize) -> Option<ThreadId> {
        self.endpoint_waiters
            .get(idx)
            .copied()
            .flatten()
            .map(|w| w.tid)
    }

    /// The present receiver waiter's COMPLETE identity, or `None`.
    pub(crate) fn endpoint_waiter_identity(&self, idx: usize) -> Option<ReceiverWaiterIdentity> {
        self.endpoint_waiters.get(idx).copied().flatten()
    }

    /// Whether a receiver waiter is present at `idx` (no identity comparison).
    pub(crate) fn endpoint_waiter_present(&self, idx: usize) -> bool {
        self.endpoint_waiters.get(idx).map(Option::is_some) == Some(true)
    }

    /// Publish (overwrite) the complete identity at `idx`. Returns the displaced identity, if any.
    pub(crate) fn set_endpoint_waiter(
        &mut self,
        idx: usize,
        identity: ReceiverWaiterIdentity,
    ) -> Option<ReceiverWaiterIdentity> {
        let displaced = self.endpoint_waiters.get(idx).copied().flatten();
        self.endpoint_waiters[idx] = Some(identity);
        displaced
    }

    /// Unconditionally take (remove + return) the waiter at `idx`.
    pub(crate) fn take_endpoint_waiter(&mut self, idx: usize) -> Option<ReceiverWaiterIdentity> {
        self.endpoint_waiters.get_mut(idx).and_then(Option::take)
    }

    /// Clear the slot at `idx` iff it EXACTLY matches `identity` (full generation-bearing compare).
    /// Returns true iff a waiter was cleared. Never removes a replacement task's waiter.
    pub(crate) fn clear_endpoint_waiter_if_identity(
        &mut self,
        idx: usize,
        identity: ReceiverWaiterIdentity,
    ) -> bool {
        if self.endpoint_waiters.get(idx).copied().flatten() == Some(identity) {
            self.endpoint_waiters[idx] = None;
            true
        } else {
            false
        }
    }

    /// Clear EVERY endpoint receive-waiter slot whose complete identity matches `identity`
    /// (task-teardown / timeout cleanup — identity-keyed, so a replacement waiter is never removed).
    pub(crate) fn clear_endpoint_waiters_for_identity(&mut self, identity: ReceiverWaiterIdentity) {
        for waiter in self.endpoint_waiters.iter_mut() {
            if *waiter == Some(identity) {
                *waiter = None;
            }
        }
    }

    /// Whether any endpoint receive-waiter slot still holds the complete `identity`.
    pub(crate) fn any_endpoint_waiter_is(&self, identity: ReceiverWaiterIdentity) -> bool {
        self.endpoint_waiters.iter().any(|w| *w == Some(identity))
    }
}

impl KernelState {
    fn wake_tid_to_runnable(&mut self, tid: ThreadId) -> Result<(), KernelError> {
        let old_status = self.task_status(tid.0).ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_WAKE_BEGIN tid={} old_status={:?}", tid.0, old_status);
        if !matches!(
            old_status,
            TaskStatus::Blocked(_) | TaskStatus::Runnable | TaskStatus::Running
        ) {
            crate::yarm_log!(
                "SCHED_WAKE_FAIL tid={} reason=unexpected_status:{:?}",
                tid.0,
                old_status
            );
            return Err(KernelError::WouldBlock);
        }
        if !matches!(old_status, TaskStatus::Runnable) {
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == tid.0)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Runnable;
                Ok::<_, KernelError>(())
            })?;
        }
        crate::yarm_log!("SCHED_WAKE_SET_RUNNABLE tid={} new_status=Runnable", tid.0);
        self.clear_ipc_timeout_for_tid(tid.0)?;
        if self.current_tid() == Some(tid.0) && matches!(old_status, TaskStatus::Running) {
            crate::yarm_log!("SCHED_WAKE_ALREADY_RUNNABLE tid={}", tid.0);
        } else {
            let (cpu, reason) = self.enqueue_woken_task(tid.0)?;
            let queue_len = self
                .with_scheduler_state(|sched| kernel_ref(&sched.scheduler).runnable_count_on(cpu));
            crate::yarm_log!(
                "SCHED_WAKE_ENQUEUE tid={} cpu={} queue_len={} reason={}",
                tid.0,
                cpu.0,
                queue_len,
                reason
            );
        }
        Ok(())
    }

    /// Store the waiter-local Reply cap CapId in the matching global ReplyCapRecord.
    ///
    /// Called by the materialization path after it mints a Reply cap directly into
    /// the waiter's cnode (without delegation tracking).  `ipc_reply` later reads
    /// this value to fast-revoke the exact cnode slot using a kernel-controlled
    /// CapId rather than the user-supplied argument.
    pub(crate) fn set_reply_cap_waiter_cap(
        &mut self,
        reply_index: usize,
        reply_generation: u64,
        cap: CapId,
    ) {
        // Stage 105 / D5: thin wrapper over try_set_reply_cap_waiter_cap that
        // discards the stale signal. The canonical reply arm of
        // materialize_received_message_cap holds the global lock for the
        // duration of the reply materialization, so a stale outcome here is
        // unreachable on that path. The D5 split path uses the fallible form
        // directly to drive its mint rollback.
        let _ = self.try_set_reply_cap_waiter_cap(reply_index, reply_generation, cap);
    }

    /// Stage 105 / D5: fallible variant of [`set_reply_cap_waiter_cap`].
    ///
    /// Returns [`ReplyRecordSetOutcome::Set`] when the record was updated, or
    /// a typed stale-outcome variant when the index, generation, or slot no
    /// longer matches what the caller minted against. Callers driving the D5
    /// reply-cap split (`cap_transfer_split::phase_b_prime_record_reply_cap`)
    /// translate any non-`Set` outcome into a mint rollback so a stale record
    /// can never leave a freshly-minted reply cap orphaned in the receiver
    /// cnode.
    ///
    /// Locks: rank 3 (IPC) only — identical to the wrapper above. The marker
    /// `IPC_RECV_REPLY_CAP_WAITER_CAP_SET` is emitted on success; the new
    /// `D5_REPLY_RECORD_SET_STALE` marker is emitted on each stale path with
    /// a `reason=` tag so smoke logs distinguish the three stale modes.
    pub(crate) fn try_set_reply_cap_waiter_cap(
        &mut self,
        reply_index: usize,
        reply_generation: u64,
        cap: CapId,
    ) -> ReplyRecordSetOutcome {
        let outcome = self.with_ipc_state_mut(|ipc| {
            if reply_index >= super::MAX_REPLY_CAPS {
                return ReplyRecordSetOutcome::IndexOutOfRange;
            }
            if ipc.reply_cap_generations[reply_index] != reply_generation {
                return ReplyRecordSetOutcome::GenerationMismatch;
            }
            if let Some(record) = &mut ipc.reply_caps[reply_index] {
                record.waiter_cap_id = Some(cap);
                crate::yarm_log!(
                    "IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index={} reply_gen={} cap={}",
                    reply_index,
                    reply_generation,
                    cap.0
                );
                ReplyRecordSetOutcome::Set
            } else {
                ReplyRecordSetOutcome::SlotEmpty
            }
        });
        if !matches!(outcome, ReplyRecordSetOutcome::Set) {
            let reason = match outcome {
                ReplyRecordSetOutcome::IndexOutOfRange => "index_out_of_range",
                ReplyRecordSetOutcome::GenerationMismatch => "generation_mismatch",
                ReplyRecordSetOutcome::SlotEmpty => "slot_empty",
                ReplyRecordSetOutcome::Set => unreachable!(),
            };
            crate::yarm_log!(
                "D5_REPLY_RECORD_SET_STALE reply_index={} reply_gen={} cap={} reason={}",
                reply_index,
                reply_generation,
                cap.0,
                reason
            );
        }
        outcome
    }

    /// Stage 20: clear a previously-set waiter Reply CapId in the global record.
    ///
    /// Called by `rollback_materialized_recv_cap` when a recv-delivery copy fails
    /// after the Reply cap was materialized: the receiver-cnode slot is being
    /// fast-revoked, so the global record must no longer reference it.  Leaves the
    /// `ReplyCapRecord` itself intact (the reply is still live and re-deliverable);
    /// only the stale waiter_cap_id is dropped.  Generation-guarded so a stale call
    /// cannot disturb a record that was already reused.
    pub(crate) fn clear_reply_cap_waiter_cap(&mut self, reply_index: usize, reply_generation: u64) {
        self.with_ipc_state_mut(|ipc| {
            if reply_index >= super::MAX_REPLY_CAPS {
                return;
            }
            if ipc.reply_cap_generations[reply_index] != reply_generation {
                return;
            }
            if let Some(record) = &mut ipc.reply_caps[reply_index] {
                record.waiter_cap_id = None;
            }
        });
    }

    /// Stage 199A2B1 — AUTHORITATIVE caller-side reply-record cleanup keyed on the
    /// complete exiting identity supplied by the caller. The exit site captures the
    /// exiting task's `{tid, asid}` while its TCB is still live (the authoritative
    /// moment), and this entry point matches records on that EXACT identity — it does
    /// NOT re-resolve an ASID from the numeric TID. A replacement task that reuses the
    /// numeric TID always carries a different ASID, so its records are untouched.
    /// `caller_asid == Asid(0)` is the no-address-space sentinel captured at creation
    /// and compared verbatim (both sides use `Asid(0)` for an ASID-less task).
    pub(crate) fn revoke_reply_caps_for_caller_identity(
        &mut self,
        caller: ReceiverWaiterIdentity,
    ) -> usize {
        self.with_ipc_state_mut(|ipc| {
            let mut revoked = 0usize;
            for slot in ipc.reply_caps.iter_mut() {
                if slot.is_some_and(|record| {
                    record.caller_tid == caller.tid && record.caller_asid == caller.asid
                }) {
                    *slot = None;
                    revoked += 1;
                }
            }
            revoked
        })
    }

    /// Numeric-TID convenience wrapper (tests / non-authoritative callers). Resolves
    /// the ASID once and delegates to the authoritative identity entry point. NOT used
    /// as the production cleanup authority — the exit sites call
    /// `revoke_reply_caps_for_caller_identity` with the identity they captured while
    /// the exiting task was live.
    pub(crate) fn revoke_reply_caps_for_caller(&mut self, caller_tid: u64) -> usize {
        let asid = self.task_asid(caller_tid).unwrap_or(Asid(0));
        self.revoke_reply_caps_for_caller_identity(ReceiverWaiterIdentity::new(
            ThreadId(caller_tid),
            asid,
        ))
    }

    /// Clear every global `ReplyCapRecord` whose `responder_tid` (the replier) is
    /// `tid`.  Mirror of `revoke_reply_caps_for_caller` but keyed on the *replier*
    /// side so that a record involving a torn-down replier is freed proactively at
    /// the replier's own exit/death — not only when the (possibly long-lived)
    /// caller eventually exits.
    ///
    /// Lock-rank: runs under `ipc_state_lock` (rank 3), identical to
    /// `revoke_reply_caps_for_caller`.  Clears matching slots in place; no wake,
    /// no scheduler mutation, no further lock acquisition.
    ///
    /// Generation: like the caller-side revoke, the slot is set to `None` (not
    /// generation-bumped).  An empty slot already invalidates any outstanding
    /// Reply cap — `resolve_reply_index` returns `StaleCapability` when the slot
    /// is `None`, before the generation is consulted; the generation is bumped on
    /// the next slot reuse by `create_reply_cap_for_caller`.
    ///
    /// Idempotent: a slot already cleared by a prior caller- or replier-side
    /// revoke is `None` and is skipped, so repeated/interleaved teardown is a
    /// no-op past the first clear.
    /// Stage 199A2B1 — AUTHORITATIVE replier-side reply-record cleanup keyed on the
    /// complete exiting identity supplied by the caller (captured while the replier's
    /// TCB was live). Matches records on the EXACT `{responder_tid, replier_asid}`
    /// identity — no ASID is re-resolved from the numeric TID here. A record is
    /// SKIPPED only when it stored a concrete `replier_asid` that DIFFERS from the
    /// supplied identity (a prior incarnation at the same numeric TID); a record with
    /// no stored replier ASID (`None`, an ASID-less responder at creation) carries no
    /// incarnation evidence and is matched on the numeric TID (the safe, never-leak
    /// direction).
    pub(crate) fn revoke_reply_caps_for_replier_identity(
        &mut self,
        replier: ReceiverWaiterIdentity,
    ) -> usize {
        self.with_ipc_state_mut(|ipc| {
            let mut revoked = 0usize;
            for slot in ipc.reply_caps.iter_mut() {
                if slot.is_some_and(|record| {
                    record.responder_tid == Some(replier.tid)
                        && record
                            .replier_asid
                            .is_none_or(|stored| stored == replier.asid)
                }) {
                    *slot = None;
                    revoked += 1;
                }
            }
            revoked
        })
    }

    /// Numeric-TID convenience wrapper (tests / non-authoritative callers). Resolves
    /// the ASID once and delegates to the authoritative identity entry point.
    pub(crate) fn revoke_reply_caps_for_replier(&mut self, tid: u64) -> usize {
        let asid = self.task_asid(tid).unwrap_or(Asid(0));
        self.revoke_reply_caps_for_replier_identity(ReceiverWaiterIdentity::new(
            ThreadId(tid),
            asid,
        ))
    }

    /// Remove `tid` from all IPC waiter slots.
    ///
    /// Must be called on task exit and death so a dead task cannot be found as
    /// a pending receiver, sender, or notification waiter.  The IPC timeout
    /// path (`process_ipc_timeout_deadlines`) performs the same clearance for
    /// timed-out tasks; this helper applies the same logic unconditionally.
    pub(crate) fn clear_ipc_waiters_for_tid(&mut self, tid: u64) {
        let tid_id = crate::kernel::ipc::ThreadId(tid);
        // Stage 198E3B2B2: the endpoint RECEIVE-waiter is cleared by the exiting task's COMPLETE
        // identity (tid + its captured ASID), so a sweep can never remove a replacement task's
        // waiter (a reused numeric TID always carries a different ASID). Task exit runs synchronously
        // under the global lock while this is still the task at `tid`, so its current ASID IS the one
        // it published with. Sender/notification waiter structures keep their numeric-TID sweep — they
        // are not endpoint receive-waiters and are out of this stage's scope.
        let identity = ReceiverWaiterIdentity::new(tid_id, self.task_asid(tid).unwrap_or(Asid(0)));
        self.with_ipc_state_mut(|ipc| {
            ipc.clear_endpoint_waiters_for_identity(identity);
            for queue in ipc.endpoint_sender_waiters.iter_mut() {
                for slot in queue.iter_mut() {
                    if slot.as_ref().is_some_and(|w| w.tid == tid_id) {
                        *slot = None;
                    }
                }
            }
            for waiter in ipc.notification_waiters.iter_mut() {
                if *waiter == Some(tid_id) {
                    *waiter = None;
                }
            }
        });
    }

    /// Return the `waiter_cap_id` recorded in the global `ReplyCapRecord` at
    /// `reply_index`, or `None` if the slot is empty or the field is unset.
    #[cfg(test)]
    pub(crate) fn reply_cap_record_waiter_cap(&self, reply_index: usize) -> Option<CapId> {
        self.with_ipc_state(|ipc| {
            ipc.reply_caps
                .get(reply_index)
                .and_then(|slot| slot.as_ref())
                .and_then(|record| record.waiter_cap_id)
        })
    }

    /// Stage 199A2A: read the caller/replier INCARNATION ASIDs captured in the
    /// global `ReplyCapRecord` at `reply_index` (`None` if the slot is empty).
    #[cfg(test)]
    pub(crate) fn reply_cap_record_caller_asid(&self, reply_index: usize) -> Option<Asid> {
        self.with_ipc_state(|ipc| {
            ipc.reply_caps
                .get(reply_index)
                .and_then(|slot| slot.as_ref())
                .map(|record| record.caller_asid)
        })
    }

    #[cfg(test)]
    pub(crate) fn reply_cap_record_replier_asid(&self, reply_index: usize) -> Option<Asid> {
        self.with_ipc_state(|ipc| {
            ipc.reply_caps
                .get(reply_index)
                .and_then(|slot| slot.as_ref())
                .and_then(|record| record.replier_asid)
        })
    }

    /// Stage 199A2A test-only mutators: overwrite the captured incarnation ASIDs on a
    /// live record to simulate a numeric-TID-reused REPLACEMENT task holding the same
    /// slot under a DIFFERENT address space, so the authority/cleanup gates can be
    /// exercised without physically recycling a TID. Returns `true` if a live record
    /// was present at `reply_index`.
    #[cfg(test)]
    pub(crate) fn force_reply_cap_record_replier_asid(
        &mut self,
        reply_index: usize,
        asid: Option<Asid>,
    ) -> bool {
        self.with_ipc_state_mut(|ipc| {
            if let Some(Some(record)) = ipc.reply_caps.get_mut(reply_index) {
                record.replier_asid = asid;
                true
            } else {
                false
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn force_reply_cap_record_caller_asid(
        &mut self,
        reply_index: usize,
        asid: Asid,
    ) -> bool {
        self.with_ipc_state_mut(|ipc| {
            if let Some(Some(record)) = ipc.reply_caps.get_mut(reply_index) {
                record.caller_asid = asid;
                true
            } else {
                false
            }
        })
    }

    /// Return whether the global `ReplyCapRecord` at `reply_index` is present.
    ///
    /// Stage 198D2A: used in production by the queued reply-cap materialize as the
    /// record-present half of the finalization revalidation (paired with the
    /// generation check in `capability_object_live`). Caller exit clears the record
    /// slot WITHOUT bumping the object generation, so this presence check is
    /// load-bearing across the queue interval.
    pub(crate) fn reply_cap_record_present(&self, reply_index: usize) -> bool {
        self.with_ipc_state(|ipc| {
            ipc.reply_caps
                .get(reply_index)
                .map(|slot| slot.is_some())
                .unwrap_or(false)
        })
    }

    /// Stage 198E3C1B rollback accessor: the number of live (allocated) endpoint slots. A leak-free
    /// provisioning rollback must leave this UNCHANGED versus the pre-attempt count.
    #[cfg(test)]
    pub(crate) fn live_endpoint_count_for_test(&self) -> usize {
        self.with_ipc_state(|ipc| ipc.endpoints.iter().flatten().count())
    }

    /// Count non-None endpoint_waiters slots for test assertions.
    #[cfg(test)]
    pub(crate) fn endpoint_waiter_count(&self, endpoint_idx: usize) -> usize {
        self.with_ipc_state(|ipc| {
            if ipc.endpoint_waiter_present(endpoint_idx) {
                1
            } else {
                0
            }
        })
    }

    /// Count non-None sender_waiter slots for a given endpoint for test assertions.
    #[cfg(test)]
    pub(crate) fn sender_waiter_count(&self, endpoint_idx: usize) -> usize {
        self.with_ipc_state(|ipc| {
            ipc.endpoint_sender_waiters
                .get(endpoint_idx)
                .map_or(0, |q| q.iter().filter(|s| s.is_some()).count())
        })
    }

    /// Count tasks blocked with WaitReason::Futex on `addr` for test assertions.
    #[cfg(test)]
    pub(crate) fn futex_waiter_count(&self, addr: usize) -> usize {
        use crate::kernel::task::{TaskStatus, WaitReason};
        use crate::kernel::vm::VirtAddr;
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .filter(|tcb| {
                    tcb.status == TaskStatus::Blocked(WaitReason::Futex(VirtAddr(addr as u64)))
                })
                .count()
        })
    }

    /// Count non-None notification_waiters slots for a given notification index.
    #[cfg(test)]
    pub(crate) fn notification_waiter_count(&self, notification_idx: usize) -> usize {
        self.with_ipc_state(|ipc| {
            if ipc
                .notification_waiters
                .get(notification_idx)
                .and_then(|w| *w)
                .is_some()
            {
                1
            } else {
                0
            }
        })
    }

    /// Return 1 if `tid` has a non-None `ipc_timeout_deadline`, else 0.
    #[cfg(test)]
    pub(crate) fn ipc_deadline_count_for_tid(&self, tid: u64) -> usize {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map_or(0, |tcb| {
                    if tcb.ipc_timeout_deadline.is_some() {
                        1
                    } else {
                        0
                    }
                })
        })
    }

    /// Count tasks blocked with WaitReason::Join(target_tid) for test assertions.
    #[cfg(test)]
    pub(crate) fn join_waiter_count(&self, target_tid: u64) -> usize {
        use crate::kernel::task::{TaskStatus, WaitReason};
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .filter(|tcb| {
                    tcb.status
                        == TaskStatus::Blocked(WaitReason::Join(crate::kernel::ipc::ThreadId(
                            target_tid,
                        )))
                })
                .count()
        })
    }

    /// Return the number of pending cross-CPU work items queued for `cpu`.
    ///
    /// Returns 0 for an out-of-range `cpu` rather than propagating an error, so
    /// tests can call this unconditionally as a queue-depth probe.
    #[cfg(test)]
    pub(crate) fn cross_cpu_work_count_for_cpu(
        &self,
        cpu: crate::kernel::scheduler::CpuId,
    ) -> usize {
        self.with_ipc_state(|ipc| ipc.cross_cpu_work.pending_for_cpu(cpu).unwrap_or(0))
    }

    fn clear_ipc_timeout_for_tid(&mut self, tid: u64) -> Result<(), KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.ipc_timeout_deadline = None;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })
    }

    pub(crate) fn consume_ipc_timeout_fired_for_tid(
        &mut self,
        tid: u64,
    ) -> Result<bool, KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            let fired = tcb.ipc_timeout_fired;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(fired)
        })
    }

    fn enqueue_sender_waiter(
        &mut self,
        endpoint_idx: usize,
        waiter: SenderWaiter,
    ) -> Result<(), KernelError> {
        let waiter_tid = waiter.tid.0;
        self.with_ipc_state_mut(|ipc| {
            // Defence-in-depth: verify endpoint still exists.
            ipc.endpoints
                .get(endpoint_idx)
                .and_then(Option::as_ref)
                .ok_or(KernelError::WrongObject)?;
            let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
            if let Some(slot) = queue[..MAX_ENDPOINT_SENDER_WAITERS]
                .iter_mut()
                .find(|s| s.is_none())
            {
                *slot = Some(waiter);
                // Stage 163 (sub-knob-gated, no-op otherwise): the sender is now a
                // real waiter on this endpoint. If this is the proof loopback E1,
                // push the deterministic waiter-present signal into the proof
                // coordination endpoint E2 WITHIN this same `ipc_state_lock`
                // critical section, so init (which non-blocking-polls E2) drains E1
                // only after the sender is provably blocked — race-free on SMP.
                // No scheduler/cap/user-copy work here (init polls; no wake needed),
                // so no lock-order hazard.
                if let Some(e2_idx) = super::proof_sender_wake_coordination_target(endpoint_idx) {
                    super::proof_sender_wake_push_coordination_locked(ipc, e2_idx, waiter_tid);
                    crate::yarm_log!(
                        "IPC_RECV_PROOF_SENDER_WAKE_WAITER_PRESENT endpoint={} tid={}",
                        endpoint_idx,
                        waiter_tid
                    );
                }
                return Ok(());
            }
            Err(KernelError::EndpointQueueFull)
        })
    }

    fn resolve_send_cap_task_local(&self, send_cap: CapId) -> Result<Capability, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.capability_for_cnode_local(cnode, send_cap)
            .ok_or(KernelError::InvalidCapability)
    }

    fn resolve_recv_cap_task_local(&self, recv_cap: CapId) -> Result<Capability, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.capability_for_cnode_local(cnode, recv_cap)
            .ok_or(KernelError::InvalidCapability)
    }

    fn mint_capability_for_active_cnode(
        &mut self,
        capability: Capability,
    ) -> Result<CapId, KernelError> {
        let cnode = self.current_task_cnode().ok_or(KernelError::TaskMissing)?;
        self.mint_capability_in_cnode(cnode, capability)
    }

    /// D-NEXT-1 PR-A Phase A — scheduler domain, rank 1: make the
    /// block-current-task scheduling decision. Acquires and releases only
    /// the scheduler's embedded lock (`block_current_cpu` →
    /// `scheduler_state()`), the same field `SharedKernel::
    /// with_scheduler_split_mut` (§6.6) guards — never the task or ipc lock.
    fn recv_block_phase_a_scheduler(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
    ) -> Result<RecvBlockPhasePlan, KernelError> {
        crate::yarm_log!(
            "D2_RECV_WAITER_SPLIT_BEGIN endpoint={} recv_cap={}",
            endpoint_idx,
            recv_cap.0
        );
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_BLOCK tid={}", blocked_tid);
        // Stage 198E3B2B2: capture the receiver's ASID now (task rank 2, no lock held) so the ipc
        // publish (rank 3) stores the COMPLETE generation-bearing waiter identity. An address-space-
        // less task (kernel/test context) falls back to Asid(0) — such tasks are not the numeric-TID
        // reuse surface (real user receivers always carry a distinct ASID).
        let receiver_asid = self.task_asid(blocked_tid).unwrap_or(Asid(0));
        Ok(RecvBlockPhasePlan {
            blocked_tid: ThreadId(blocked_tid),
            endpoint_idx,
            recv_cap,
            receiver_asid,
        })
    }

    /// D-NEXT-1 PR-A Phase B — task/TCB domain, rank 2: transition the
    /// blocked thread's TCB to `Blocked(EndpointReceive)` and stage the
    /// deadline. Acquires and releases only the task lock (`with_tcbs_mut`),
    /// the same field `SharedKernel::with_task_tcbs_split_mut` (§6.6)
    /// guards — never the scheduler or ipc lock. Entered strictly after
    /// Phase A has released the scheduler lock (rank 1 → rank 2, never
    /// reversed).
    fn recv_block_phase_b_task(
        &mut self,
        plan: RecvBlockPhasePlan,
        deadline: Option<u64>,
    ) -> Result<RecvBlockPhasePlan, KernelError> {
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == plan.blocked_tid.0)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(plan.recv_cap));
            tcb.ipc_timeout_deadline = deadline;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })?;
        crate::yarm_log!("D2_RECV_WAITER_TASK_BLOCKED tid={}", plan.blocked_tid.0);
        Ok(plan)
    }

    /// D-NEXT-1 PR-A Phase C — ipc domain, rank 3: atomically recheck the
    /// endpoint queue and publish the waiter under `ipc_state_lock`, via the
    /// unchanged Stage 106 live primitive. Entered strictly after Phase B
    /// has released the task lock (rank 2 → rank 3, never reversed). On
    /// `QueueNonEmpty` the caller drives the no-lost-wakeup unwind back
    /// through ranks 2 and 1 (`recv_block_unwind_race`).
    fn recv_block_phase_c_ipc_publish(
        &mut self,
        plan: RecvBlockPhasePlan,
    ) -> crate::kernel::recv_waiter_split::PublishWaiterOutcome {
        let outcome = self.publish_recv_waiter_live(
            plan.endpoint_idx,
            ReceiverWaiterIdentity::new(plan.blocked_tid, plan.receiver_asid),
            plan.recv_cap,
        );
        if matches!(
            outcome,
            crate::kernel::recv_waiter_split::PublishWaiterOutcome::Published
        ) {
            crate::yarm_log!("D2_RECV_WAITER_PUBLISHED tid={}", plan.blocked_tid.0);
        }
        outcome
    }

    /// D-NEXT-1 PR-A: no-lost-wakeup unwind for the Phase C `QueueNonEmpty`
    /// race — a sender enqueued between the caller's Phase-1 empty dequeue
    /// and the Phase C publish. Unreachable under the serialized global
    /// lock (Phase A/B/C run in the same `&mut KernelState` borrow today);
    /// REQUIRED for correctness once a future pass genuinely exits the
    /// global lock for this path (see the module-level deferral note above
    /// `block_current_on_receive_with_deadline`). Reverses Phase B (task →
    /// Runnable, rank 2) then re-enters the scheduler (rank 1) to
    /// redispatch — byte-identical to the Stage 106 unwind, only extracted
    /// into a named call boundary.
    fn recv_block_unwind_race(
        &mut self,
        plan: RecvBlockPhasePlan,
    ) -> Result<ThreadId, KernelError> {
        crate::yarm_log!(
            "D2_PUBLISH_RACE_UNWIND endpoint={} tid={}",
            plan.endpoint_idx,
            plan.blocked_tid.0
        );
        crate::yarm_log!("D2_RECV_WAITER_RACE_UNWIND tid={}", plan.blocked_tid.0);
        self.note_d2_publish_race_unwind();
        self.wake_tid_to_runnable(plan.blocked_tid)?;
        if crate::kernel::boot::d2_recv_genuine_enabled() {
            // Stage 168 (D2-GENUINE-RECV): no-lost-wakeup rollback — Phase B was
            // reversed (task → Runnable) and the scheduler re-entered before the
            // redispatch below.
            crate::yarm_log!("D2_RECV_GENUINE_ROLLBACK_OK tid={}", plan.blocked_tid.0);
        }
        let _ = self.dispatch_next_task()?;
        Ok(plan.blocked_tid)
    }

    /// VALIDATION: D2_LIVE_SPLIT (Stage 106 / Pass 3; phase-named Stage 111)
    ///
    /// D-NEXT-1 PR-A note (Stage 111): this orchestrator now calls three
    /// named, rank-ordered phase functions (`recv_block_phase_a_scheduler`
    /// rank 1 → `recv_block_phase_b_task` rank 2 →
    /// `recv_block_phase_c_ipc_publish` rank 3) instead of inlining the
    /// scheduler/task/ipc steps. Each phase still acquires its domain lock
    /// through the existing `KernelState` alias method (`block_current_cpu`
    /// / `with_tcbs_mut` / `publish_recv_waiter_live`) rather than through
    /// `SharedKernel::with_scheduler_split_mut` /
    /// `with_task_tcbs_split_mut` (§6.6) directly: those seams derive their
    /// pointer via `self.state.data_ptr()` on `SharedKernel`, and this
    /// method runs nested inside an already-held `&mut KernelState` borrow
    /// (`SharedKernel::with`/`with_cpu`) reached from the trap dispatcher —
    /// calling back into a sibling raw-pointer projection of the *same*
    /// backing storage while that exclusive borrow is alive would alias it,
    /// and would not actually shrink the global-lock hold time since the
    /// outer borrow remains live for the whole call. Genuinely exiting the
    /// global lock for this path requires relocating the IpcRecv-block
    /// entry point to before `SharedKernel::with_cpu` in trap dispatch —
    /// mirroring `SharedKernel::try_split_ipc_recv_queued_plain_into_frame`,
    /// the existing non-blocking split precedent — which is deferred to a
    /// follow-on PR (see `doc/KERNEL_UNLOCKING.md` §D-NEXT-1 PR-A). The
    /// `M2_SEAM_HELPER_ONLY` fence for the scheduler/task seams is therefore
    /// kept as-is; behavior, lock order, and the no-lost-wakeup contract are
    /// byte-identical to Stage 106.
    fn block_current_on_receive_with_deadline(
        &mut self,
        endpoint_idx: usize,
        recv_cap: CapId,
        deadline: Option<u64>,
    ) -> Result<ThreadId, KernelError> {
        // Phase order: scheduler (rank 1) → task/TCB (rank 2) → ipc (rank
        // 3) → dispatch. Sequential acquire/release, never nested — each
        // phase function below acquires and releases its own domain lock
        // before the next phase begins. Because the TCB is Blocked (Phase
        // B) BEFORE the publish becomes visible (Phase C), any sender that
        // observes the published waiter also observes a Blocked TCB — wake
        // cannot be lost on that edge (audit doc §15.2 / §18).
        // Phase A (scheduler rank 1) → Phase B (task rank 2): block + Blocked TCB.
        let plan = self.recv_block_phase_a_scheduler(endpoint_idx, recv_cap)?;
        let plan = self.recv_block_phase_b_task(plan, deadline)?;
        if crate::kernel::boot::d2_recv_genuine_enabled() {
            // Stage 168 (D2-GENUINE-RECV): scheduler+task phases complete.
            crate::yarm_log!(
                "D2_RECV_GENUINE_PHASE_TASK_BLOCK tid={}",
                plan.blocked_tid.0
            );
        }
        // Phase C (ipc rank 3): publish waiter, with the no-lost-wakeup unwind.
        match self.recv_block_phase_c_ipc_publish(plan) {
            crate::kernel::recv_waiter_split::PublishWaiterOutcome::Published => {}
            crate::kernel::recv_waiter_split::PublishWaiterOutcome::QueueNonEmpty => {
                return self.recv_block_unwind_race(plan);
            }
            // The live primitive preserves canonical overwrite semantics for
            // a pre-existing waiter (it never returns ReceiverAlreadyWaiting)
            // and endpoint_idx was validated by resolve_endpoint_index, so
            // InvalidEndpoint is defensively unreachable here.
            crate::kernel::recv_waiter_split::PublishWaiterOutcome::ReceiverAlreadyWaiting
            | crate::kernel::recv_waiter_split::PublishWaiterOutcome::InvalidEndpoint => {
                return Err(KernelError::WrongObject);
            }
        }
        crate::yarm_log!(
            "IPC_RECV_BLOCK_REGISTER endpoint={} tid={}",
            plan.endpoint_idx,
            plan.blocked_tid.0
        );
        if crate::kernel::boot::d2_recv_genuine_enabled() {
            // Stage 168 (D2-GENUINE-RECV): ipc publish done; enter dispatch.
            crate::yarm_log!("D2_RECV_GENUINE_PHASE_IPC_LOCK tid={}", plan.blocked_tid.0);
            crate::yarm_log!("D2_RECV_GENUINE_PHASE_DISPATCH tid={}", plan.blocked_tid.0);
        }
        // Stage 168B (D2-GENUINE-RECV completion): defer the queue-advancing
        // dispatch OUT of the global KernelState lock. Phase A (`block_current`)
        // removed the recv task from `current`, so the dispatch that follows
        // genuinely advances the run queue — the case Stage 168A had to fall
        // back on (reason=switch_required). When eligible, record a per-CPU
        // deferral drained by the trap entry after the global guard drops and
        // SKIP the in-lock authoritative dispatch entirely; the trap-entry drain
        // runs the single authoritative `dispatch_next_on` under only the
        // scheduler seam and the arch thread-state restore via the hardened
        // D6-SWITCH-A path. Ineligible cases keep the in-lock dispatch fallback.
        #[cfg(target_arch = "x86_64")]
        if crate::kernel::boot::d2_recv_genuine_enabled()
            && !crate::kernel::boot::d6_controlled_switch_proof_enabled()
            && !crate::kernel::boot::d6_switch_a_enabled()
        {
            let cpu_idx = self.current_cpu().0 as usize;
            let trap_path = cpu_idx < crate::kernel::scheduler::MAX_CPUS
                && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
                    .load(core::sync::atomic::Ordering::Relaxed);
            // Stage 183.6: single-DISPATCHER predicate — wake-only APs run no
            // dispatcher, so the accepted out-of-lock recv dispatch stays live
            // under real SMP (online>1) with no in-lock multi_cpu fallback.
            let single_cpu = self.dispatching_cpu_count() <= 1;
            let already = crate::kernel::boot::d2_recv_dispatch_is_deferred(cpu_idx);
            if trap_path
                && single_cpu
                && !already
                && crate::kernel::boot::d2_recv_dispatch_try_defer(cpu_idx, plan.blocked_tid.0)
            {
                crate::yarm_log!(
                    "D2_RECV_GENUINE_DISPATCH_DEFERRED tid={} cpu={}",
                    plan.blocked_tid.0,
                    cpu_idx
                );
                crate::yarm_log!(
                    "D2_RECV_GENUINE_NO_INLOCK_DISPATCH tid={}",
                    plan.blocked_tid.0
                );
                // The out-of-lock trap-entry drain performs the authoritative
                // queue-advancing dispatch; do NOT dispatch in-lock here.
                return Ok(plan.blocked_tid);
            }
            let reason = if !trap_path {
                "no_trap_drainer"
            } else if !single_cpu {
                "multi_cpu"
            } else {
                "already_deferred"
            };
            crate::yarm_log!(
                "D2_RECV_GENUINE_FALLBACK reason={} tid={}",
                reason,
                plan.blocked_tid.0
            );
        }
        let _ = self.dispatch_next_task()?;
        Ok(plan.blocked_tid)
    }

    fn block_current_on_send_with_deadline(
        &mut self,
        endpoint_idx: usize,
        send_cap: CapId,
        msg: Message,
        deadline: Option<u64>,
    ) -> Result<ThreadId, KernelError> {
        // Phase A (scheduler rank 1) → Phase B (task rank 2): block + Blocked TCB.
        let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("SCHED_BLOCK tid={}", blocked_tid);
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == blocked_tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Blocked(WaitReason::EndpointSend(send_cap));
            tcb.ipc_timeout_deadline = deadline;
            tcb.ipc_timeout_fired = false;
            Ok::<_, KernelError>(())
        })?;
        if crate::kernel::boot::d2_send_genuine_enabled() {
            crate::yarm_log!("D2_SEND_GENUINE_PHASE_TASK_BLOCK tid={}", blocked_tid);
        }
        // Phase C (ipc rank 3): publish the sender-waiter (its message rides with
        // it, so the receiver-side wake/handoff and the Stage 163P sender-wake
        // oracle are unchanged).
        self.enqueue_sender_waiter(
            endpoint_idx,
            SenderWaiter {
                tid: ThreadId(blocked_tid),
                msg,
            },
        )?;
        if crate::kernel::boot::d2_send_genuine_enabled() {
            crate::yarm_log!("D2_SEND_GENUINE_PHASE_IPC_LOCK tid={}", blocked_tid);
            crate::yarm_log!("D2_SEND_GENUINE_PHASE_DISPATCH tid={}", blocked_tid);
        }
        // Stage 169 (D2-GENUINE-SEND): defer the queue-advancing dispatch OUT of
        // the global lock, exactly as Stage 168B did for recv. Phase A
        // (`block_current`) removed the sender from `current`, so the dispatch
        // that follows genuinely advances the run queue. When eligible, record a
        // per-CPU deferral drained by the trap entry after the global guard drops
        // and SKIP the in-lock authoritative dispatch entirely.
        #[cfg(target_arch = "x86_64")]
        if crate::kernel::boot::d2_send_genuine_enabled()
            && !crate::kernel::boot::d6_controlled_switch_proof_enabled()
            && !crate::kernel::boot::d6_switch_a_enabled()
        {
            let cpu_idx = self.current_cpu().0 as usize;
            let trap_path = cpu_idx < crate::kernel::scheduler::MAX_CPUS
                && crate::kernel::boot::GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE[cpu_idx]
                    .load(core::sync::atomic::Ordering::Relaxed);
            // Stage 183.6: single-DISPATCHER predicate (see recv-side note).
            let single_cpu = self.dispatching_cpu_count() <= 1;
            let already = crate::kernel::boot::d2_send_dispatch_is_deferred(cpu_idx);
            if trap_path
                && single_cpu
                && !already
                && crate::kernel::boot::d2_send_dispatch_try_defer(cpu_idx, blocked_tid)
            {
                crate::yarm_log!(
                    "D2_SEND_GENUINE_DISPATCH_DEFERRED tid={} cpu={}",
                    blocked_tid,
                    cpu_idx
                );
                crate::yarm_log!("D2_SEND_GENUINE_NO_INLOCK_DISPATCH tid={}", blocked_tid);
                crate::yarm_log!("D2_SEND_GENUINE_BLOCKED_OK tid={}", blocked_tid);
                crate::yarm_log!("D2_SEND_GENUINE_DONE result=blocked tid={}", blocked_tid);
                // The out-of-lock trap-entry drain performs the authoritative
                // queue-advancing dispatch; do NOT dispatch in-lock here.
                return Ok(ThreadId(blocked_tid));
            }
            let reason = if !trap_path {
                "no_trap_drainer"
            } else if !single_cpu {
                "multi_cpu"
            } else {
                "already_deferred"
            };
            crate::yarm_log!(
                "D2_SEND_GENUINE_FALLBACK reason={} tid={}",
                reason,
                blocked_tid
            );
        }
        let _ = self.dispatch_next_task()?;
        Ok(ThreadId(blocked_tid))
    }

    pub(crate) fn wake_waiter_for_endpoint(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(), KernelError> {
        let waiter = self.with_ipc_state_mut(|ipc| ipc.take_endpoint_waiter(endpoint_idx));
        if let Some(waiter) = waiter {
            crate::yarm_log!("SCHED_WAKE tid={}", waiter.tid.0);
            self.wake_tid_to_runnable(waiter.tid)?;
        }
        Ok(())
    }

    /// Enqueue `msg` into the endpoint at `endpoint_idx` and wake any waiter.
    ///
    /// This is the canonical "supervisor notify" pattern used by fault reporting,
    /// task-exit, transfer-revoke, and TLB-shootdown escalation.  It enforces the
    /// lock ordering:
    ///
    /// - Enqueue under `ipc_state_lock` (rank 3) so the message is visible before
    ///   the lock is released.
    /// - Wake the waiter **after** releasing `ipc_state_lock` — `wake_tid_to_runnable`
    ///   acquires `task_state_lock` (rank 2), which ranks below IPC (rank 3) and
    ///   must therefore not be acquired while the IPC lock is held.
    pub(crate) fn send_message_to_endpoint_and_wake(
        &mut self,
        endpoint_idx: usize,
        msg: crate::kernel::ipc::Message,
    ) -> Result<(), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            let Some(ep_storage) = ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut)
            else {
                return Err(KernelError::WrongObject);
            };
            kernel_mut(ep_storage)
                .send(msg)
                .map_err(|_| KernelError::EndpointQueueFull)?;
            Ok(())
        })?;
        let _ = self.wake_waiter_for_endpoint(endpoint_idx);
        Ok(())
    }

    pub(crate) fn process_ipc_timeout_deadlines(
        &mut self,
        now_tick: u64,
    ) -> Result<usize, KernelError> {
        // Stage 171 (SCHED-TIMEOUT), Task F: bounded-stack chunked scan. The
        // historical MAX_TASKS-wide `Option<ThreadId>` scratch array (512 entries,
        // ~8 KiB) was allocated on EVERY timer-tick trap frame; this
        // processes expirations in fixed `TIMEOUT_SCAN_CHUNK` batches so the stack
        // frame is O(CHUNK) regardless of `MAX_TASKS`. Behavior-equivalent: returns
        // the TOTAL expired count, clears every waiter slot for each expired task,
        // and enqueues each expired task exactly once. Each expired task's deadline
        // is cleared in the pass that wakes it, so no task is selected twice across
        // passes (the loop terminates once a pass finds zero expirations).
        //
        // Rank order per batch: task (rank 2, mark Runnable + clear deadline) ->
        // ipc (rank 3, clear waiter slots) -> scheduler (rank 1, enqueue OUTSIDE
        // the task/ipc locks). Locks are acquired and released per phase, never
        // nested, so no lower-rank lock is ever taken while a higher-rank lock is
        // held.
        const TIMEOUT_SCAN_CHUNK: usize = 32;
        let proof = crate::kernel::boot::sched_timeout_enabled();
        let mut total = 0usize;
        let mut scan_announced = false;
        loop {
            // `(tid, is_send, asid)` — `is_send` classifies the SCHED_TIMEOUT_EXPIRED kind; `asid` is
            // the timed-out task's captured ASID (Stage 198E3B2B2), so the endpoint receive-waiter is
            // cleared by COMPLETE identity, never numeric TID alone.
            let mut expired: [Option<(ThreadId, bool, Asid)>; TIMEOUT_SCAN_CHUNK] =
                [None; TIMEOUT_SCAN_CHUNK];
            let mut n = 0usize;
            // Phase 1 (task rank 2): mark up to CHUNK expired tasks Runnable.
            self.with_tcbs_mut(|tcbs| {
                for tcb in tcbs.iter_mut().flatten() {
                    if n >= TIMEOUT_SCAN_CHUNK {
                        break;
                    }
                    let Some(deadline) = tcb.ipc_timeout_deadline else {
                        continue;
                    };
                    let is_send = match tcb.status {
                        TaskStatus::Blocked(WaitReason::EndpointReceive(_)) => false,
                        TaskStatus::Blocked(WaitReason::EndpointSend(_)) => true,
                        _ => continue,
                    };
                    if now_tick.wrapping_sub(deadline) > 0 || now_tick == deadline {
                        tcb.status = TaskStatus::Runnable;
                        tcb.ipc_timeout_deadline = None;
                        tcb.ipc_timeout_fired = true;
                        expired[n] = Some((tcb.tid, is_send, tcb.asid.unwrap_or(Asid(0))));
                        n += 1;
                    }
                }
                Ok::<_, KernelError>(())
            })?;
            if n == 0 {
                break;
            }
            if proof {
                if !scan_announced {
                    crate::yarm_log!("SCHED_TIMEOUT_SCAN_BEGIN now={}", now_tick);
                    scan_announced = true;
                }
                for entry in expired.iter().take(n).flatten() {
                    crate::yarm_log!(
                        "SCHED_TIMEOUT_EXPIRED tid={} kind={}",
                        entry.0.0,
                        if entry.1 { "send" } else { "recv" }
                    );
                }
                crate::yarm_log!("SCHED_TIMEOUT_TASK_WAKE_BEGIN count={}", n);
            }
            // Phase 2 (ipc rank 3): remove every timed-out waiter from ALL waiter
            // structures, then re-check none of the batch tids remain (Task D).
            let mut stranded_in_batch = false;
            self.with_ipc_state_mut(|ipc| {
                for entry in expired.iter().take(n).flatten() {
                    let tid = entry.0;
                    let identity = ReceiverWaiterIdentity::new(tid, entry.2);
                    // Endpoint receive-waiter: cleared by COMPLETE identity (never numeric TID alone).
                    ipc.clear_endpoint_waiters_for_identity(identity);
                    for queue in ipc.endpoint_sender_waiters.iter_mut() {
                        for slot in queue.iter_mut() {
                            if slot.as_ref().is_some_and(|w| w.tid == tid) {
                                *slot = None;
                            }
                        }
                    }
                    for waiter in ipc.notification_waiters.iter_mut() {
                        if *waiter == Some(tid) {
                            *waiter = None;
                        }
                    }
                }
                // Task D re-check (single-lock, cheap): a stranded waiter would be a
                // batch tid still referenced after the clear — provably impossible
                // unless the clear loop has a bug.
                for entry in expired.iter().take(n).flatten() {
                    let tid = entry.0;
                    let identity = ReceiverWaiterIdentity::new(tid, entry.2);
                    let remains = ipc.any_endpoint_waiter_is(identity)
                        || ipc
                            .endpoint_sender_waiters
                            .iter()
                            .any(|q| q.iter().any(|s| s.as_ref().is_some_and(|w| w.tid == tid)))
                        || ipc.notification_waiters.iter().any(|w| *w == Some(tid));
                    if remains {
                        stranded_in_batch = true;
                    }
                }
            });
            if stranded_in_batch {
                crate::yarm_log!("SCHED_TIMEOUT_STRANDED_WAITER now={}", now_tick);
            }
            // Phase 3 (scheduler rank 1, OUTSIDE task/ipc locks): enqueue each once.
            for entry in expired.iter().take(n).flatten() {
                let _ = self.enqueue_task(entry.0.0)?;
                if proof {
                    crate::yarm_log!("SCHED_TIMEOUT_RUNQUEUE_ENQUEUE tid={}", entry.0.0);
                }
            }
            if proof {
                crate::yarm_log!("SCHED_TIMEOUT_TASK_WAKE_DONE count={}", n);
            }
            total += n;
        }
        if proof && total > 0 {
            crate::yarm_log!("SCHED_TIMEOUT_NO_STRANDED_WAITERS woken={}", total);
            crate::yarm_log!("SCHED_TIMEOUT_SCAN_DONE expired={}", total);
        }
        Ok(total)
    }

    /// Stage 171 (SCHED-TIMEOUT), Task E: the earliest pending IPC timeout
    /// deadline across all `Blocked(EndpointReceive|EndpointSend)` tasks, or
    /// `None` when no IPC timeout is armed. Read-only (task rank 2); used only by
    /// the diagnostic idle-entry markers (knob-gated + rate-limited), never on the
    /// hot path.
    pub(crate) fn sched_timeout_earliest_pending(&self) -> Option<u64> {
        self.with_tcbs(|tcbs| {
            let mut earliest: Option<u64> = None;
            for tcb in tcbs.iter().flatten() {
                let Some(deadline) = tcb.ipc_timeout_deadline else {
                    continue;
                };
                let blocked_ipc = matches!(
                    tcb.status,
                    TaskStatus::Blocked(WaitReason::EndpointReceive(_))
                        | TaskStatus::Blocked(WaitReason::EndpointSend(_))
                );
                if !blocked_ipc {
                    continue;
                }
                earliest = Some(match earliest {
                    Some(e) if e <= deadline => e,
                    _ => deadline,
                });
            }
            earliest
        })
    }

    pub(crate) fn resolve_endpoint_index(&self, object: CapObject) -> Result<usize, KernelError> {
        let limits = self.runtime_capacity_config();
        match object {
            CapObject::Endpoint { index, generation } => self.with_ipc_state(|ipc| {
                if index >= limits.max_endpoints {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoints[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if ipc.endpoint_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            CapObject::Kernel
            | CapObject::AddressSpace { .. }
            | CapObject::IovaSpace { .. }
            | CapObject::MemoryObject { .. }
            | CapObject::DmaRegion { .. }
            | CapObject::Notification { .. }
            | CapObject::Reply { .. }
            | CapObject::Irq { .. } => Err(KernelError::WrongObject),
        }
    }

    /// Stage 4C/4D endpoint-domain split receive helper.
    ///
    /// Preconditions: the caller has already validated the receive capability and
    /// endpoint generation. This helper mutates only the selected buffered endpoint
    /// queue under `ipc_state_lock`.
    ///
    /// Stage 4C: no sender waiter — dequeue the plain message and return Received.
    ///
    /// Stage 4D: plain sender waiter at queue head — two-phase refill:
    ///   Phase 1 (here): dequeue receiver message, dequeue first sender waiter,
    ///   enqueue sender's message into the newly-freed slot.
    ///   Phase 2 (caller, outside lock): wake sender via apply_split_sender_wake_plan.
    ///   Returns ReceivedWithSenderWake so the caller can apply the wake plan outside
    ///   any lock.
    ///
    /// Falls back (Ineligible) for every case that requires scheduler/TCB mutation,
    /// capability operations, user-memory access, TrapFrame writes, complex (flagged)
    /// messages, non-buffered endpoints, or sender waiters with complex messages.
    #[allow(dead_code)]
    pub(crate) fn ipc_try_recv_queued_plain_endpoint_only(
        &mut self,
        endpoint_idx: usize,
    ) -> IpcEndpointRecvResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            if ipc.endpoint_waiter_present(endpoint_idx) {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::ReceiverWaiterPresent,
                );
            }

            // Stage 4D: peek at sender waiter queue head (position 0) before touching the
            // endpoint. Copies data so no reference is held across the endpoint borrow below.
            let head_waiter: Option<(ThreadId, Message)> =
                ipc.endpoint_sender_waiters[endpoint_idx][0].map(|w| (w.tid, w.msg));

            // If the queue is sparse (position 0 is None but later positions are Some), the
            // gap was left by a timed-out sender; fall back to the full path which handles
            // arbitrary queue state correctly.
            if head_waiter.is_none()
                && ipc.endpoint_sender_waiters[endpoint_idx]
                    .iter()
                    .any(Option::is_some)
            {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::SenderWaiterPresent,
                );
            }

            // If a sender waiter exists at position 0, validate their message before
            // dequeuing anything — ensures we can commit to the full two-phase refill.
            if let Some((_, waiter_msg)) = head_waiter {
                let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                    | Message::FLAG_CAP_TRANSFER_PLAIN
                    | Message::FLAG_REPLY_CAP;
                if (waiter_msg.flags & split_unsafe_flags) != 0
                    || waiter_msg.transferred_cap().is_some()
                {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
            }

            // Borrow endpoint, validate mode and message, then dequeue.
            // Scoped block so the &mut Endpoint borrow ends before the sender-waiter
            // refill accesses ipc.endpoint_sender_waiters and re-borrows ipc.endpoints.
            let received = {
                let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EndpointMissing,
                    );
                };
                let endpoint = kernel_mut(endpoint_storage);
                if endpoint.mode() != EndpointMode::Buffered {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                    );
                }
                let Some(message) = endpoint.peek().copied() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EmptyQueue,
                    );
                };
                let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                    | Message::FLAG_CAP_TRANSFER_PLAIN
                    | Message::FLAG_REPLY_CAP;
                if (message.flags & split_unsafe_flags) != 0 || message.transferred_cap().is_some()
                {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                    );
                }
                endpoint
                    .recv()
                    .expect("peeked plain endpoint message must remain queued")
            };

            // Stage 4D two-phase refill: only reached when head_waiter is Some with a
            // plain message (validated above). All mutations stay under ipc_state_lock.
            if let Some((waiter_tid, waiter_msg)) = head_waiter {
                // Phase 1a: remove first sender waiter and compact the queue.
                {
                    let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
                    queue[0] = None;
                    for idx in 1..queue.len() {
                        queue[idx - 1] = queue[idx].take();
                    }
                }
                // Phase 1b: enqueue the sender's message into the slot freed by recv.
                {
                    let ep = kernel_mut(
                        ipc.endpoints[endpoint_idx]
                            .as_mut()
                            .expect("endpoint must remain present after recv dequeue"),
                    );
                    ep.send(waiter_msg)
                        .expect("one slot must be free after recv dequeue");
                }
                crate::yarm_log!("IPC_RECV_SPLIT_REFILL_QUEUED waiter_tid={}", waiter_tid.0);
                return IpcEndpointRecvResult::ReceivedWithSenderWake(received, waiter_tid);
            }

            IpcEndpointRecvResult::Received(received)
        })
    }

    /// Stage 42+43: like [`ipc_try_recv_queued_plain_endpoint_only`] but allows
    /// cap-transfer and reply-cap flagged messages at the receiver side.
    ///
    /// The receiver's message is dequeued regardless of `FLAG_CAP_TRANSFER`,
    /// `FLAG_CAP_TRANSFER_PLAIN`, or `FLAG_REPLY_CAP` flags.  The caller must
    /// call `materialize_received_message_cap` BEFORE the user-space writeback.
    ///
    /// Sender-waiter guard is unchanged: if the sender's refill message has any
    /// of those flags, the function still returns
    /// `Ineligible(SenderWaiterPresent)` — cap-transfer refill on the split path
    /// is deferred to a future stage.
    #[allow(dead_code)]
    pub(crate) fn ipc_try_recv_queued_with_cap_transfer(
        &mut self,
        endpoint_idx: usize,
    ) -> IpcEndpointRecvResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            if ipc.endpoint_waiter_present(endpoint_idx) {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::ReceiverWaiterPresent,
                );
            }

            let head_waiter: Option<(ThreadId, Message)> =
                ipc.endpoint_sender_waiters[endpoint_idx][0].map(|w| (w.tid, w.msg));

            if head_waiter.is_none()
                && ipc.endpoint_sender_waiters[endpoint_idx]
                    .iter()
                    .any(Option::is_some)
            {
                return IpcEndpointRecvResult::Ineligible(
                    IpcEndpointSplitRejectReason::SenderWaiterPresent,
                );
            }

            // Reject if the SENDER'S refill message is cap-flagged (unchanged guard).
            if let Some((_, waiter_msg)) = head_waiter {
                let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                    | Message::FLAG_CAP_TRANSFER_PLAIN
                    | Message::FLAG_REPLY_CAP;
                if (waiter_msg.flags & split_unsafe_flags) != 0
                    || waiter_msg.transferred_cap().is_some()
                {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
            }

            let received = {
                let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EndpointMissing,
                    );
                };
                let endpoint = kernel_mut(endpoint_storage);
                if endpoint.mode() != EndpointMode::Buffered {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                    );
                }
                let Some(_message) = endpoint.peek() else {
                    return IpcEndpointRecvResult::Ineligible(
                        IpcEndpointSplitRejectReason::EmptyQueue,
                    );
                };
                // Cap-transfer check intentionally omitted — caller handles materialization.
                endpoint
                    .recv()
                    .expect("peeked endpoint message must remain queued")
            };

            if let Some((waiter_tid, waiter_msg)) = head_waiter {
                {
                    let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
                    queue[0] = None;
                    for idx in 1..queue.len() {
                        queue[idx - 1] = queue[idx].take();
                    }
                }
                {
                    let ep = kernel_mut(
                        ipc.endpoints[endpoint_idx]
                            .as_mut()
                            .expect("endpoint must remain present after recv dequeue"),
                    );
                    ep.send(waiter_msg)
                        .expect("one slot must be free after recv dequeue");
                }
                crate::yarm_log!(
                    "IPC_RECV_SPLIT_CAP_REFILL_QUEUED waiter_tid={}",
                    waiter_tid.0
                );
                return IpcEndpointRecvResult::ReceivedWithSenderWake(received, waiter_tid);
            }

            IpcEndpointRecvResult::Received(received)
        })
    }

    /// Apply a deferred scheduler wake plan returned by the Stage 4D split recv helper.
    ///
    /// Must be called outside `ipc_state_lock`. Wakes the sender whose message was
    /// already refilled into the endpoint queue under ipc_state_lock.
    pub(crate) fn apply_split_sender_wake_plan(
        &mut self,
        sender_tid: ThreadId,
    ) -> Result<(), KernelError> {
        crate::yarm_log!("IPC_RECV_SPLIT_REFILL_WAKE_APPLY tid={}", sender_tid.0);
        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(sender_tid))
    }

    pub(crate) fn ipc_try_send_queued_plain_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        msg: Message,
    ) -> IpcEndpointSendResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            let receiver_waiter = ipc.endpoint_waiter_identity(endpoint_idx);
            let has_sender_waiters = ipc.endpoint_sender_waiters[endpoint_idx]
                .iter()
                .any(Option::is_some);

            match (receiver_waiter, has_sender_waiters) {
                (Some(_), true) => {
                    // Both receiver and sender waiters present: complex ordering state.
                    // Fall back to the full IPC send path which handles this correctly.
                    return IpcEndpointSendResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
                (Some(receiver), false) => {
                    // Receiver waiter present, no sender waiters.
                    // TID comes from a locked ipc_state_lock read — no unlocked access needed.
                    // Caller must check is_task_recv_v2_blocked (task_state_lock rank 3) before
                    // calling ipc_try_send_to_plain_receiver_endpoint_only (ipc_state_lock rank 4).
                    return IpcEndpointSendResult::ReceiverWaiterFound(receiver);
                }
                (None, true) => {
                    return IpcEndpointSendResult::Ineligible(
                        IpcEndpointSplitRejectReason::SenderWaiterPresent,
                    );
                }
                (None, false) => {
                    // No waiters: fall through to Stage 4E queue-enqueue logic below.
                }
            }

            // Stage 4E: FLAG_REPLY_CAP messages carry a kernel reply-cap handle and
            // must use the full send path (reply-cap semantics require endpoint-side
            // tracking not present here).
            //
            // FLAG_CAP_TRANSFER and FLAG_CAP_TRANSFER_PLAIN are safe for Stage 4E:
            // stash_transfer_handle already moved the cap into the transfer-envelope
            // table before this call, so the message's transferred_cap field is merely
            // a numeric envelope handle.  For the no-receiver buffered-enqueue case,
            // ipc_send_with_optional_deadline does an identical endpoint.send(msg),
            // making Stage 4E a strict behavioural subset of the full path.
            // The receiver's ipc_recv (or ipc_recv_timeout) falls through to the full
            // path to materialise the cap, since Stage 4C/4D still rejects cap-transfer
            // messages on the recv side.
            if (msg.flags & Message::FLAG_REPLY_CAP) != 0 {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                );
            }

            let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointMissing,
                );
            };
            let endpoint = kernel_mut(endpoint_storage);
            if endpoint.mode() != EndpointMode::Buffered {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                );
            }

            if endpoint.send(msg).is_err() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointQueueFull,
                );
            }

            IpcEndpointSendResult::Enqueued
        })
    }

    /// Stage 193E (BROAD-IPC DECOMPOSITION): the IpcSend PLAIN no-waiter enqueue boundary
    /// split. Thin instrumentation wrapper over the Stage 4E endpoint-only enqueue
    /// [`Self::ipc_try_send_queued_plain_endpoint_only`]: for a PLAIN message (no cap
    /// transfer, no reply cap, no shared-region, no transferred cap) it emits the enqueue
    /// boundary markers around the endpoint-only enqueue and, on a successful `Enqueued`
    /// with no blocked receiver, fires the one-shot IpcSendPlainEnqueue retirement. The
    /// enqueue itself is UNCHANGED — the same rank-4 IPC-lock endpoint enqueue (NO user
    /// copy, NO cap materialization, NO receiver wake, NO sender block): there is no
    /// deferred Phase B/C work, so the sender returns the legacy result directly.
    ///
    /// Non-plain messages (cap-transfer / reply-cap) skip the markers entirely and go
    /// straight to the unchanged Stage 4E path (which still enqueues cap-transfer messages);
    /// the `ReceiverWaiterFound` / `Ineligible` (queue-full, non-buffered, …) outcomes emit a
    /// DEFERRED marker and return unchanged so the caller's legacy fallback runs. Byte-
    /// identical to the Stage 4E enqueue in every case; only additive markers differ.
    pub(crate) fn ipc_try_send_enqueue_boundary_split_plain(
        &mut self,
        endpoint_idx: usize,
        msg: Message,
    ) -> IpcEndpointSendResult {
        // PLAIN only: any cap-transfer / reply-cap / plain-cap flag or non-`None`
        // transferred cap is NOT the 193E slice — route it to the 193F ordinary-cap
        // enqueue boundary, which retires the ordinary-object case and defers reply-cap /
        // shared / Reply-object transfers to the unchanged Stage 4E path.
        let plain = (msg.flags
            & (Message::FLAG_CAP_TRANSFER
                | Message::FLAG_CAP_TRANSFER_PLAIN
                | Message::FLAG_REPLY_CAP))
            == 0
            && msg.transferred_cap().is_none();
        if !plain {
            return self.ipc_try_send_enqueue_boundary_split_ordinary_cap(endpoint_idx, msg);
        }
        crate::yarm_log!(
            "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_BEGIN endpoint={} len={}",
            endpoint_idx,
            msg.as_slice().len()
        );
        // Phase A: the payload/meta are snapshotted by value (msg is Copy, passed by
        // value into the endpoint enqueue) — no user copy, no cap materialization.
        crate::yarm_log!(
            "IPC_SEND_ENQUEUE_BOUNDARY_SNAPSHOT_OK endpoint={}",
            endpoint_idx
        );
        let result = self.ipc_try_send_queued_plain_endpoint_only(endpoint_idx, msg);
        match result {
            IpcEndpointSendResult::Enqueued => {
                // Enqueued exactly once into the endpoint queue (no blocked receiver).
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_ENQUEUE_OK endpoint={}",
                    endpoint_idx
                );
                // Sender state matches legacy: a plain non-blocking send that enqueues does
                // NOT block the sender and is NOT published as a sender-waiter — it returns
                // Ok and continues.
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_SENDER_STATE_OK endpoint={} sender_blocked=0",
                    endpoint_idx
                );
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DONE result=ok endpoint={}",
                    endpoint_idx
                );
                crate::kernel::boot::maybe_log_ipc_send_plain_enqueue_retired();
            }
            IpcEndpointSendResult::ReceiverWaiterFound(_) => {
                // A receiver waiter is present — this is the blocked-waiter deliver slice
                // (193A/legacy), NOT the no-waiter enqueue. Defer unchanged.
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=receiver_waiter_present endpoint={}",
                    endpoint_idx
                );
            }
            IpcEndpointSendResult::Ineligible(_) => {
                // Queue full / non-buffered / etc. — the legacy send path handles capacity
                // + blocking semantics. Defer unchanged (nothing was mutated wrongly).
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=ineligible endpoint={}",
                    endpoint_idx
                );
            }
            IpcEndpointSendResult::EnqueuedWakeReceiver(_) => {
                // The endpoint-only enqueue never returns this for the no-waiter path.
                crate::yarm_log!(
                    "IPC_SEND_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=wake_receiver endpoint={}",
                    endpoint_idx
                );
            }
        }
        result
    }

    /// Stage 193F (BROAD-IPC DECOMPOSITION): the IpcSend ORDINARY-CAP no-waiter enqueue
    /// boundary split. Thin instrumentation wrapper over the SAME Stage 4E endpoint-only
    /// enqueue seam, for a cap-transfer message whose transferred OBJECT is ORDINARY (not a
    /// Reply, not a shared-region). The transfer envelope is PRESERVED at enqueue (the queued
    /// message carries only its numeric handle) — NO receiver cap materialization, NO user
    /// copy, NO receiver wake, NO sender block. The receiver's LATER recv_v2 consumes the
    /// envelope + materializes a fresh receiver-local cap (`IPC_TRANSFER_CAP_MATERIALIZE_OK`).
    ///
    /// Object-based routing (mirrors the 193D blocked-waiter reply-cap split): the userspace
    /// IpcSend ABI has no reply flag, so a Reply-typed transfer is identified by peeking the
    /// envelope's source object (read-only, non-consuming). Reply objects, shared-region
    /// transfers (`OPCODE_SHARED_MEM`), and `FLAG_REPLY_CAP` messages are DEFERRED to the
    /// unchanged Stage 4E path BEFORE any marker/mutation (they are NOT retired under this
    /// ordinary-cap class). Byte-identical to the Stage 4E enqueue in every case; only
    /// additive markers differ.
    fn ipc_try_send_enqueue_boundary_split_ordinary_cap(
        &mut self,
        endpoint_idx: usize,
        msg: Message,
    ) -> IpcEndpointSendResult {
        // ORDINARY cap-transfer only: a cap-transfer flag set, NOT a reply-cap flag, exactly
        // one transferred cap, NOT a shared-region opcode, and the transferred OBJECT is not a
        // Reply. Anything else → the unchanged Stage 4E path (no ordinary-cap markers).
        let is_reply_flag = (msg.flags & Message::FLAG_REPLY_CAP) != 0;
        let is_transfer =
            (msg.flags & (Message::FLAG_CAP_TRANSFER | Message::FLAG_CAP_TRANSFER_PLAIN)) != 0;
        let is_shared = msg.opcode == crate::kernel::syscall::OPCODE_SHARED_MEM;
        let handle = msg.transferred_cap();
        let is_reply_object = match handle {
            Some(h) => matches!(
                self.peek_transfer_envelope_source_object(h.0),
                Some(CapObject::Reply { .. })
            ),
            None => false,
        };
        let ordinary =
            is_transfer && !is_reply_flag && !is_shared && handle.is_some() && !is_reply_object;
        if !ordinary {
            // reply-cap / shared-region / Reply object → Stage 4E unchanged (NOT retired here).
            return self.ipc_try_send_queued_plain_endpoint_only(endpoint_idx, msg);
        }
        crate::yarm_log!(
            "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_BEGIN endpoint={} len={}",
            endpoint_idx,
            msg.as_slice().len()
        );
        // Phase A: the payload/meta + the numeric envelope handle are snapshotted by value
        // (msg is Copy) — no user copy, no cap materialization.
        crate::yarm_log!(
            "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SNAPSHOT_OK endpoint={}",
            endpoint_idx
        );
        let result = self.ipc_try_send_queued_plain_endpoint_only(endpoint_idx, msg);
        match result {
            IpcEndpointSendResult::Enqueued => {
                // Enqueued exactly once into the endpoint queue (no blocked receiver).
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_ENQUEUE_OK endpoint={}",
                    endpoint_idx
                );
                // Transfer state matches legacy: the envelope is PRESERVED in the envelope
                // table (NOT consumed, NO cap materialized at enqueue). The queued message
                // carries only the numeric handle; the receiver's later recv_v2 consumes the
                // envelope + materializes a fresh receiver-local cap.
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_TRANSFER_STATE_OK endpoint={} envelope=preserved",
                    endpoint_idx
                );
                // Sender state matches legacy: a non-blocking send that enqueues does NOT
                // block the sender and is NOT published as a sender-waiter.
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SENDER_STATE_OK endpoint={} sender_blocked=0",
                    endpoint_idx
                );
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_DONE result=ok endpoint={}",
                    endpoint_idx
                );
                crate::kernel::boot::maybe_log_ipc_send_ordinary_cap_enqueue_retired();
            }
            IpcEndpointSendResult::ReceiverWaiterFound(_) => {
                // A receiver waiter is present — the blocked-waiter deliver slice (193C /
                // legacy), NOT the no-waiter enqueue. Defer unchanged.
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=receiver_waiter_present endpoint={}",
                    endpoint_idx
                );
            }
            IpcEndpointSendResult::Ineligible(_) => {
                // Queue full / non-buffered / etc. — the legacy send path handles capacity +
                // blocking semantics + the envelope disposition. Defer unchanged.
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=ineligible endpoint={}",
                    endpoint_idx
                );
            }
            IpcEndpointSendResult::EnqueuedWakeReceiver(_) => {
                crate::yarm_log!(
                    "IPC_SEND_CAP_ENQUEUE_BOUNDARY_SPLIT_DEFERRED reason=wake_receiver endpoint={}",
                    endpoint_idx
                );
            }
        }
        result
    }

    /// Return true if the task identified by `tid` is blocked on a recv-v2 operation.
    /// Acquires task_state_lock (rank 3). Must be called before ipc_state_lock (rank 4).
    pub(crate) fn is_task_recv_v2_blocked(&self, tid: u64) -> bool {
        self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
        })
    }

    /// Stage 4F: plain send to a waiting legacy (non-recv-v2) receiver on a buffered endpoint.
    ///
    /// Preconditions (caller must verify before this call):
    ///   - `expected_receiver_tid` is not recv-v2 blocked (checked under task_state_lock rank 3)
    ///   - message has no cap-transfer or reply-cap flags
    ///
    /// Under ipc_state_lock:
    ///   - re-verifies receiver slot still holds expected_receiver_tid
    ///   - enqueues msg into the endpoint queue
    ///   - clears endpoint_waiters slot
    ///
    /// Returns EnqueuedWakeReceiver(tid) on success; caller must call
    /// apply_split_receiver_wake_plan outside the lock.
    pub(crate) fn ipc_try_send_to_plain_receiver_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver: ReceiverWaiterIdentity,
        msg: Message,
    ) -> IpcEndpointSendResult {
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointIndexOutOfRange,
                );
            }
            // Re-verify receiver slot by COMPLETE identity (Stage 198E3B2B2): a timeout may have
            // cleared it, or a replacement task may have reused the numeric TID with a different ASID,
            // between the pre-check and this lock — numeric TID alone must not authorize the clear.
            if ipc.endpoint_waiter_identity(endpoint_idx) != Some(expected_receiver) {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::ReceiverWaiterPresent,
                );
            }
            // Defense-in-depth: sender waiters should have been screened out by
            // ipc_try_send_queued_plain_endpoint_only returning ReceiverWaiterFound only when
            // no sender waiters are present. Re-check under lock for safety.
            if ipc.endpoint_sender_waiters[endpoint_idx]
                .iter()
                .any(Option::is_some)
            {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::SenderWaiterPresent,
                );
            }
            let split_unsafe_flags = Message::FLAG_CAP_TRANSFER
                | Message::FLAG_CAP_TRANSFER_PLAIN
                | Message::FLAG_REPLY_CAP;
            if (msg.flags & split_unsafe_flags) != 0 || msg.transferred_cap().is_some() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::TransferOrReplyCapMessage,
                );
            }
            let Some(endpoint_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointMissing,
                );
            };
            let endpoint = kernel_mut(endpoint_storage);
            if endpoint.mode() != EndpointMode::Buffered {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::NonBufferedEndpoint,
                );
            }
            if endpoint.send(msg).is_err() {
                return IpcEndpointSendResult::Ineligible(
                    IpcEndpointSplitRejectReason::EndpointQueueFull,
                );
            }
            // Clear receiver from waiters by the EXACT identity re-verified above; wake outside lock.
            ipc.clear_endpoint_waiter_if_identity(endpoint_idx, expected_receiver);
            crate::yarm_log!(
                "IPC_SEND_SPLIT_ENQUEUED_WAKE_RECEIVER receiver_tid={}",
                expected_receiver.tid.0
            );
            IpcEndpointSendResult::EnqueuedWakeReceiver(expected_receiver.tid)
        })
    }

    /// Apply the deferred receiver-wake plan returned by ipc_try_send_to_plain_receiver_endpoint_only.
    /// Must be called outside ipc_state_lock.
    pub(crate) fn apply_split_receiver_wake_plan(
        &mut self,
        receiver_tid: ThreadId,
    ) -> Result<(), KernelError> {
        crate::yarm_log!("IPC_SEND_SPLIT_RECEIVER_WAKE_APPLY tid={}", receiver_tid.0);
        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(receiver_tid))
    }

    /// Apply a general-purpose deferred wake plan produced by any kernel domain.
    ///
    /// Callers compute the plan while holding a domain-specific lock, then release
    /// that lock before calling this function.  The function itself acquires only
    /// scheduler-internal state (rank 1–2) which is below all IPC/task/capability
    /// locks, so no lock-ordering violation is possible.
    ///
    /// See `doc/KERNEL_LOCKING.md §SchedulerWakePlan` for the full protocol.
    pub(crate) fn apply_scheduler_wake_plan(
        &mut self,
        plan: super::SchedulerWakePlan,
    ) -> Result<(), KernelError> {
        match plan {
            super::SchedulerWakePlan::None => Ok(()),
            super::SchedulerWakePlan::Wake(tid) => self.wake_tid_to_runnable(tid),
        }
    }

    /// Apply a deferred cooperative-handoff plan.
    ///
    /// `YieldTo(tid)` calls `yield_current_to(tid)` — a one-shot preempt that removes
    /// `tid` from the run-queue and makes it current directly, bypassing FIFO order.
    /// Returns `true` when `tid` became the current task, `false` otherwise.
    ///
    /// Callers that guarantee `tid` was just enqueued (e.g. via `wake_waiter_for_endpoint`)
    /// will always get `true` back.
    ///
    /// Must be called outside all IPC/cap/VM/memory domain locks.
    pub(crate) fn apply_scheduler_handoff_plan(
        &mut self,
        plan: super::SchedulerHandoffPlan,
    ) -> Result<bool, KernelError> {
        match plan {
            super::SchedulerHandoffPlan::None => Ok(false),
            super::SchedulerHandoffPlan::YieldTo(tid) => self.yield_current_to(tid),
        }
    }

    fn resolve_reply_index(&self, object: CapObject) -> Result<usize, KernelError> {
        match object {
            CapObject::Reply { index, generation } => self.with_ipc_state(|ipc| {
                if index >= super::MAX_REPLY_CAPS {
                    return Err(KernelError::WrongObject);
                }
                if ipc.reply_caps[index].is_none() || ipc.reply_cap_generations[index] != generation
                {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn create_reply_cap_for_caller(
        &mut self,
        caller_tid: ThreadId,
        caller_reply_recv_cap: CapId,
        responder_tid: Option<ThreadId>,
    ) -> Result<CapId, KernelError> {
        self.create_reply_cap_for_caller_in_cnode(
            caller_tid,
            caller_reply_recv_cap,
            responder_tid,
            None,
        )
    }

    /// Stage 193D: `create_reply_cap_for_caller` with an explicit destination cnode
    /// for the minted Reply cap. `dest_cnode == None` mints into the ACTIVE cnode
    /// (byte-identical to the historical behavior every existing caller relies on);
    /// `Some(cnode)` mints into that cnode instead (used to provision a transferable
    /// reply cap into init's cnode at boot for the reply-cap live oracle). The
    /// one-shot reply record reservation / rollback / Phase-3 persist is UNCHANGED —
    /// only the mint target is parameterized.
    pub fn create_reply_cap_for_caller_in_cnode(
        &mut self,
        caller_tid: ThreadId,
        caller_reply_recv_cap: CapId,
        responder_tid: Option<ThreadId>,
        dest_cnode: Option<crate::kernel::capabilities::CNodeId>,
    ) -> Result<CapId, KernelError> {
        let reply_capability =
            self.resolve_capability_for_task(caller_tid.0, caller_reply_recv_cap)?;
        if !reply_capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        let reply_endpoint = match reply_capability.object {
            CapObject::Endpoint { .. } => reply_capability.object,
            _ => return Err(KernelError::WrongObject),
        };

        crate::yarm_log!(
            "IPC_CALL_REPLY_CAP_ALLOC_BEGIN caller_tid={} responder={}",
            caller_tid.0,
            responder_tid.map(|t| t.0).unwrap_or(u64::MAX)
        );

        // Stage 199A2A: capture the INCARNATION discriminators (ASIDs) for the caller
        // and (if bound) the responder BEFORE the mutable ipc-state borrow. These are
        // stored in the record so every downstream authority/cleanup decision uses the
        // complete `{tid, asid}` identity — a numeric TID reused by a replacement task
        // (different ASID) can never authorize a reply or clear a fresh incarnation's
        // record. `task_asid` returns `None` for a task with no address space (kernel
        // task); the caller then records `Asid(0)` and the responder records `None`.
        let caller_asid = self.task_asid(caller_tid.0).unwrap_or(Asid(0));
        let replier_asid = responder_tid.and_then(|t| self.task_asid(t.0));

        // Phase 1: Reserve a global reply slot with a placeholder caller_cap_id.
        // The real CapId is filled in after the mint succeeds (Phase 3).
        let (slot, generation) = self
            .with_ipc_state_mut(|ipc| {
                for idx in 0..super::MAX_REPLY_CAPS {
                    if ipc.reply_caps[idx].is_none() {
                        let mut next_generation = ipc.reply_cap_generations[idx].wrapping_add(1);
                        if next_generation == 0 {
                            next_generation = 1;
                        }
                        ipc.reply_cap_generations[idx] = next_generation;
                        ipc.reply_caps[idx] = Some(ReplyCapRecord {
                            caller_tid,
                            caller_asid,
                            reply_endpoint,
                            responder_tid,
                            replier_asid,
                            caller_cap_id: CapId(0), // placeholder; updated in Phase 3
                            waiter_cap_id: None,     // filled in when cap is materialized
                        });
                        return Ok::<_, KernelError>((idx, next_generation));
                    }
                }
                Err(KernelError::CapabilityFull)
            })
            .map_err(|err| {
                crate::yarm_log!(
                    "IPC_CALL_REPLY_RECORD_ALLOC_FAIL caller_tid={} err={:?}",
                    caller_tid.0,
                    err
                );
                err
            })?;

        // Phase 2: Mint the Reply cap into the destination cnode (the caller's active
        // cnode when dest_cnode is None — the historical default; an explicit cnode
        // for the Stage 193D reply-cap oracle provisioning).
        let reply_capability_to_mint = Capability::new(
            CapObject::Reply {
                index: slot,
                generation,
            },
            CapRights::SEND,
        );
        let mint_result = match dest_cnode {
            Some(cnode) => self.mint_capability_in_cnode(cnode, reply_capability_to_mint),
            None => self.mint_capability_for_active_cnode(reply_capability_to_mint),
        };
        let cap_id = match mint_result {
            Ok(id) => id,
            Err(err) => {
                let active_cnode = self.current_task_cnode().map(|c| c.0).unwrap_or(u64::MAX);
                crate::yarm_log!(
                    "IPC_CALL_MINT_REPLY_CAP_FAIL caller_tid={} slot={} cnode={} err={:?}",
                    caller_tid.0,
                    slot,
                    active_cnode,
                    err
                );
                // Rollback: free the slot reserved in Phase 1 so it can be reused.
                self.with_ipc_state_mut(|ipc| {
                    ipc.reply_caps[slot] = None;
                });
                return Err(err);
            }
        };

        // Phase 3: Persist the minted CapId in the record so that ipc_reply can revoke
        // it from the caller's cnode when the reply is eventually delivered.
        self.with_ipc_state_mut(|ipc| {
            if let Some(record) = &mut ipc.reply_caps[slot] {
                record.caller_cap_id = cap_id;
            }
        });

        crate::yarm_log!(
            "IPC_CALL_REPLY_CAP_ALLOC_DONE caller_tid={} slot={} cap={}",
            caller_tid.0,
            slot,
            cap_id.0
        );
        Ok(cap_id)
    }

    /// Return the reply endpoint recorded in the `ReplyCapRecord` for `reply_cap`
    /// (resolved in the **current** task's cnode) without consuming or modifying the record.
    ///
    /// Used by `handle_ipc_reply` in syscall.rs to look up the reply endpoint so
    /// it can call `stash_transfer_handle` before committing to the reply.  The
    /// `ipc_reply` call itself would also validate and then consume the record, so
    /// we can safely peek here without any TOCTOU concern (both paths run single-
    /// threaded inside the kernel lock).
    pub fn reply_cap_peek_endpoint(&self, reply_cap: CapId) -> Result<CapObject, KernelError> {
        let capability = self.resolve_send_cap_task_local(reply_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let slot = self.resolve_reply_index(capability.object)?;
        self.with_ipc_state(|ipc| {
            ipc.reply_caps[slot]
                .ok_or(KernelError::StaleCapability)
                .map(|rec| rec.reply_endpoint)
        })
    }

    /// Stage 188F — dispatch an `ipc_reply` delivery to a blocked recv-v2 caller
    /// through the existing 188B/188C/188D dispatch-return producers, in
    /// preference order plain → ordinary-cap → reply-cap. Runs entirely in Phase A
    /// (under the broad borrow): each producer consumes the blocked state + any
    /// transfer/reply envelope ONCE and stashes a by-value `DispatchPostWork`; the
    /// trap-entry drain then runs the seam mint + 186E copy + slot-clear + wake
    /// after the borrow drops. No seam / user-copy / cap-materialization happens
    /// here. Reuses the 188B/188C/188D producers — no duplicated delivery logic.
    ///
    /// Returns `Ok(true)` when a producer stashed post-work (caller returns Ok;
    /// Phase C does the slot-clear + wake), `Ok(false)` when no producer applied
    /// (shared-region reply, or no trap-entry drainer — caller uses the legacy
    /// path), or `Err` on a real producer error (mapped to `UserMemoryFault`,
    /// exactly as the legacy delivery failure).
    ///
    /// The replier's reply-cap record was already consumed once above (the
    /// `reply_caps[slot] = None` take), so the boundary is one-shot before any
    /// producer runs.
    fn try_ipc_reply_boundary_split(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        use crate::kernel::syscall::{
            produce_blocked_waiter_ordinary_cap_delivery, produce_blocked_waiter_plain_delivery,
            produce_blocked_waiter_reply_cap_delivery,
        };
        crate::yarm_log!(
            "IPC_REPLY_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
            waiter_tid,
            endpoint_idx
        );
        // The replier's reply-cap record was consumed once in Phase A above.
        crate::yarm_log!(
            "IPC_REPLY_BOUNDARY_REPLY_CAP_CONSUME_OK waiter_tid={}",
            waiter_tid
        );

        type Producer = fn(
            &mut KernelState,
            u64,
            usize,
            &Message,
        ) -> Result<bool, crate::kernel::syscall::SyscallError>;
        let producers: [(&str, Producer); 3] = [
            ("plain", produce_blocked_waiter_plain_delivery),
            ("ordinary_cap", produce_blocked_waiter_ordinary_cap_delivery),
            ("reply_cap", produce_blocked_waiter_reply_cap_delivery),
        ];
        for (kind, producer) in producers {
            match producer(self, waiter_tid, endpoint_idx, msg) {
                Ok(true) => {
                    crate::yarm_log!("IPC_REPLY_BOUNDARY_POST_WORK_STASH_OK kind={}", kind);
                    self.note_ipc_reply_split_delivery();
                    crate::yarm_log!(
                        "IPC_REPLY_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                        waiter_tid,
                        endpoint_idx
                    );
                    crate::yarm_log!("IPC_REPLY_BOUNDARY_SPLIT_DONE result=ok kind={}", kind);
                    return Ok(true);
                }
                Ok(false) => continue,
                Err(e) => {
                    crate::yarm_log!(
                        "IPC_REPLY_BOUNDARY_SPLIT_FAIL reason=producer_error kind={} err={:?}",
                        kind,
                        e
                    );
                    return Err(KernelError::UserMemoryFault);
                }
            }
        }
        // No producer applied: shared-region reply, or no trap-entry drainer.
        crate::yarm_log!(
            "IPC_REPLY_BOUNDARY_SPLIT_DEFERRED reason=unsupported_or_no_drainer waiter_tid={}",
            waiter_tid
        );
        Ok(false)
    }

    /// Stage 193A (BROAD-IPC DECOMPOSITION): the IpcSend PLAIN waiting-receiver boundary
    /// split. Reuses the SAME 188 plain-delivery producer + trap-entry drain as
    /// [`try_ipc_reply_boundary_split`], but ONLY for the plain slice — the producer returns
    /// `Ok(false)` for any cap-transfer / reply-cap / shared-region message (those fall back
    /// to the legacy in-broad-lock `complete_blocked_recv_for_waiter`). Phase A here snapshots
    /// payload/meta by value (no user copy, no cap materialization, no `ipc_state_lock` across
    /// a copy); the drain does the copy + endpoint-slot-clear + wake AFTER the broad borrow
    /// drops. Tags the stash origin so the drain emits the IpcSend boundary markers.
    ///
    /// `Ok(true)` — snapshotted; the drain completes copy + wake (sender returns Ok).
    /// `Ok(false)` — not the plain slice / no drainer; caller uses the legacy path (the
    ///   producer consumed NOTHING before returning false).
    /// `Err(_)` — a real Phase-A error (undersized/unmapped waiter buffer) → `UserMemoryFault`,
    ///   exactly as the legacy delivery failure (the blocked state was already consumed).
    fn try_ipc_send_boundary_split_plain(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        use crate::kernel::syscall::produce_blocked_waiter_plain_delivery;
        crate::yarm_log!(
            "IPC_SEND_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
            waiter_tid,
            endpoint_idx
        );
        match produce_blocked_waiter_plain_delivery(self, waiter_tid, endpoint_idx, msg) {
            Ok(true) => {
                let cpu_idx = self.current_cpu().0 as usize;
                crate::kernel::boot::ipc_send_boundary_origin_set(cpu_idx);
                crate::yarm_log!(
                    "IPC_SEND_BOUNDARY_PLAIN_SNAPSHOT_OK waiter_tid={}",
                    waiter_tid
                );
                Ok(true)
            }
            Ok(false) => {
                crate::yarm_log!(
                    "IPC_SEND_BOUNDARY_SPLIT_DEFERRED reason=unsupported_or_no_drainer waiter_tid={}",
                    waiter_tid
                );
                Ok(false)
            }
            Err(e) => {
                crate::yarm_log!(
                    "IPC_SEND_BOUNDARY_SPLIT_FAIL reason=producer_error waiter_tid={} err={:?}",
                    waiter_tid,
                    e
                );
                Err(KernelError::UserMemoryFault)
            }
        }
    }

    /// Stage 193A: `pub(crate)` entry the IpcSend syscall handler calls for the plain
    /// waiting-receiver boundary split.
    pub(crate) fn try_ipc_send_boundary_split_plain_pub(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        self.try_ipc_send_boundary_split_plain(waiter_tid, endpoint_idx, msg)
    }

    /// Stage 193C (BROAD-IPC DECOMPOSITION): the IpcSend ORDINARY cap-transfer
    /// waiting-receiver boundary split. Reuses the SAME 188C ordinary-cap producer +
    /// trap-entry executor as [`try_ipc_reply_boundary_split`], but ONLY for the ordinary
    /// cap-transfer slice — the producer returns `Ok(false)` for any plain / reply-cap /
    /// shared-region message (those fall back to the legacy in-broad-lock path). Phase A
    /// consumes the transfer envelope ONCE and snapshots object/rights/delegation-parent +
    /// payload/meta by value (NO mint, NO user copy, NO `ipc_state_lock` across a copy); the
    /// drain materializes the fresh receiver-local cap through the 186D2/186D3 seam, copies
    /// payload/meta through the 186E seam, and wakes the receiver once AFTER the broad borrow
    /// drops. Tags the stash origin so the drain emits the IpcSend-cap boundary markers.
    ///
    /// `Ok(true)` — snapshotted (envelope consumed once); the drain completes materialize +
    ///   copy + wake (sender returns Ok).
    /// `Ok(false)` — not the ordinary cap-transfer slice / no drainer; caller uses the legacy
    ///   path (the producer consumed NOTHING before returning false).
    /// `Err(_)` — a real Phase-A error (undersized/unmapped waiter buffer, missing/dead
    ///   envelope, source-cap resolution) → `UserMemoryFault`, exactly as the legacy delivery
    ///   failure (the envelope disposition matches the legacy arm).
    fn try_ipc_send_boundary_split_ordinary_cap(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        use crate::kernel::syscall::produce_blocked_waiter_ordinary_cap_delivery;
        crate::yarm_log!(
            "IPC_SEND_CAP_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
            waiter_tid,
            endpoint_idx
        );
        match produce_blocked_waiter_ordinary_cap_delivery(self, waiter_tid, endpoint_idx, msg) {
            Ok(true) => {
                let cpu_idx = self.current_cpu().0 as usize;
                crate::kernel::boot::ipc_send_cap_boundary_origin_set(cpu_idx);
                crate::yarm_log!(
                    "IPC_SEND_CAP_BOUNDARY_SNAPSHOT_OK waiter_tid={}",
                    waiter_tid
                );
                Ok(true)
            }
            Ok(false) => {
                crate::yarm_log!(
                    "IPC_SEND_CAP_BOUNDARY_SPLIT_DEFERRED reason=unsupported_or_no_drainer waiter_tid={}",
                    waiter_tid
                );
                Ok(false)
            }
            Err(e) => {
                crate::yarm_log!(
                    "IPC_SEND_CAP_BOUNDARY_SPLIT_FAIL reason=producer_error waiter_tid={} err={:?}",
                    waiter_tid,
                    e
                );
                Err(KernelError::UserMemoryFault)
            }
        }
    }

    /// Stage 193C: `pub(crate)` entry the IpcSend syscall handler calls for the ordinary
    /// cap-transfer waiting-receiver boundary split.
    pub(crate) fn try_ipc_send_boundary_split_ordinary_cap_pub(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        self.try_ipc_send_boundary_split_ordinary_cap(waiter_tid, endpoint_idx, msg)
    }

    /// Stage 193D (BROAD-IPC DECOMPOSITION): the IpcSend REPLY-CAP transfer
    /// waiting-receiver boundary split. Reuses the SAME 188D reply-cap producer +
    /// trap-entry executor as [`try_ipc_reply_boundary_split`], but ONLY for the reply-cap
    /// slice — the producer returns `Ok(false)` for any plain / ordinary-cap / shared
    /// message (those fall back to the legacy in-broad-lock path). Phase A consumes the
    /// reply-cap transfer envelope ONCE and snapshots the reply object's registry
    /// coordinates (reply_index, reply_generation) + payload/meta by value (NO mint, NO IPC
    /// record, NO user copy, NO `ipc_state_lock` across a copy); the drain mints the fresh
    /// receiver-local one-shot reply cap through the rank-4 seam, records the waiter-cap
    /// through the rank-3 IPC seam, copies payload/meta through the 186E seam, and wakes the
    /// receiver once AFTER the broad borrow drops. Tags the stash origin so the drain emits
    /// the IpcSend-reply-cap boundary markers.
    ///
    /// `Ok(true)` — snapshotted (envelope consumed once); the drain completes mint + record
    ///   + copy + wake (sender returns Ok).
    /// `Ok(false)` — not the reply-cap slice / no drainer; caller uses the legacy path (the
    ///   producer consumed NOTHING before returning false).
    /// `Err(_)` — a real Phase-A error (undersized/unmapped waiter buffer, missing/dead
    ///   envelope, non-`Reply` object) → `UserMemoryFault`, exactly as the legacy delivery
    ///   failure (the envelope disposition matches the legacy arm).
    fn try_ipc_send_boundary_split_reply_cap(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        use crate::kernel::syscall::produce_blocked_waiter_reply_cap_delivery;
        // OBJECT-BASED routing: the userspace IpcSend ABI carries no FLAG_REPLY_CAP
        // (handle_ipc_send tags every transfer as FLAG_CAP_TRANSFER), so a reply-cap
        // transfer is identified by the transferred cap's OBJECT being a `Reply`, not
        // by a message flag. Peek the envelope's source object WITHOUT consuming it;
        // anything but a Reply object declines (Ok(false)) so the ordinary-cap slice /
        // legacy path handles it. This runs BEFORE the ordinary-cap slice so a Reply
        // object is never mis-consumed by the ordinary producer.
        let Some(handle) = msg.transferred_cap() else {
            return Ok(false);
        };
        let is_reply_object = matches!(
            self.peek_transfer_envelope_source_object(handle.0),
            Some(CapObject::Reply { .. })
        );
        if !is_reply_object {
            return Ok(false);
        }
        crate::yarm_log!(
            "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_BEGIN waiter_tid={} endpoint={}",
            waiter_tid,
            endpoint_idx
        );
        // The 188D producer gates on FLAG_REPLY_CAP; synthesize a reply-flagged view
        // of this message (same sender/opcode/payload/handle) so it takes the reply-cap
        // envelope + reply-object snapshot path. `Message::with_header` re-validates the
        // flag/handle pairing (a reply-cap flag requires a transfer handle — present).
        let reply_msg = match Message::with_header(
            msg.sender_tid.0,
            msg.opcode,
            Message::FLAG_REPLY_CAP,
            Some(handle.0),
            msg.as_slice(),
        ) {
            Ok(m) => m,
            Err(_) => {
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL reason=synth_msg waiter_tid={}",
                    waiter_tid
                );
                return Err(KernelError::UserMemoryFault);
            }
        };
        match produce_blocked_waiter_reply_cap_delivery(self, waiter_tid, endpoint_idx, &reply_msg)
        {
            Ok(true) => {
                let cpu_idx = self.current_cpu().0 as usize;
                crate::kernel::boot::ipc_send_reply_cap_boundary_origin_set(cpu_idx);
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SNAPSHOT_OK waiter_tid={}",
                    waiter_tid
                );
                Ok(true)
            }
            Ok(false) => {
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_DEFERRED reason=unsupported_or_no_drainer waiter_tid={}",
                    waiter_tid
                );
                Ok(false)
            }
            Err(e) => {
                crate::yarm_log!(
                    "IPC_SEND_REPLY_CAP_BOUNDARY_SPLIT_FAIL reason=producer_error waiter_tid={} err={:?}",
                    waiter_tid,
                    e
                );
                Err(KernelError::UserMemoryFault)
            }
        }
    }

    /// Stage 193D: `pub(crate)` entry the IpcSend syscall handler calls for the reply-cap
    /// transfer waiting-receiver boundary split.
    pub(crate) fn try_ipc_send_boundary_split_reply_cap_pub(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        self.try_ipc_send_boundary_split_reply_cap(waiter_tid, endpoint_idx, msg)
    }

    /// Stage 193D: try every IpcSend boundary split in the ONE correct order for a
    /// recv-v2-blocked receiver: plain (193A) → reply-cap object (193D) → ordinary
    /// cap (193C). Reply-cap runs BEFORE ordinary-cap because a transferred `Reply`
    /// object must not be mis-consumed by the ordinary producer (the object-based
    /// reply-cap split peeks WITHOUT consuming, so a non-Reply transfer declines and
    /// falls straight through to the ordinary slice). Returns `Ok(true)` if any slice
    /// produced a boundary snapshot (the drain completes copy/materialize + wake),
    /// `Ok(false)` if none applied (caller uses the legacy in-broad-lock path — the
    /// producers consumed NOTHING), or `Err` on a real Phase-A fault.
    pub(crate) fn try_ipc_send_boundary_split_any_pub(
        &mut self,
        waiter_tid: u64,
        endpoint_idx: usize,
        msg: &Message,
    ) -> Result<bool, KernelError> {
        match self.try_ipc_send_boundary_split_plain(waiter_tid, endpoint_idx, msg)? {
            true => return Ok(true),
            false => {}
        }
        match self.try_ipc_send_boundary_split_reply_cap(waiter_tid, endpoint_idx, msg)? {
            true => return Ok(true),
            false => {}
        }
        self.try_ipc_send_boundary_split_ordinary_cap(waiter_tid, endpoint_idx, msg)
    }

    pub fn ipc_reply(&mut self, reply_cap: CapId, msg: Message) -> Result<(), KernelError> {
        let current_tid = self.current_tid().ok_or(KernelError::TaskMissing)?;
        crate::yarm_log!("IPC_REPLY_ENTER tid={}", current_tid);
        let capability = self.resolve_send_cap_task_local(reply_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let slot = match self.resolve_reply_index(capability.object) {
            Ok(slot) => slot,
            Err(err) => return Err(err),
        };
        let replier_tid = ThreadId(self.current_tid().ok_or(KernelError::TaskMissing)?);
        // Stage 199A2A: resolve the CURRENT replier's incarnation ASID before the
        // ipc-state borrow. Authorization requires the COMPLETE `{tid, asid}` identity
        // to match the identity captured when the record was created — a numeric
        // replier TID reused by a replacement task (different ASID) is rejected.
        // Numeric TID alone never authorizes a reply delivery/wake.
        let replier_asid_now = self.task_asid(replier_tid.0);
        let allowed = self.with_ipc_state(|ipc| {
            let rec = ipc.reply_caps[slot].ok_or(KernelError::StaleCapability)?;
            let tid_ok = rec.responder_tid.is_none_or(|tid| tid == replier_tid);
            // Incarnation gate: when the record bound a specific responder AND both
            // the stored and current ASIDs are known, they MUST match. A mismatch is
            // a reused-numeric-TID replacement task and is rejected. When either ASID
            // is unknown (kernel task / unresolved), the numeric-TID decision stands.
            let asid_ok = match rec.responder_tid {
                None => true,
                Some(_) => match (rec.replier_asid, replier_asid_now) {
                    (Some(stored), Some(now)) => stored == now,
                    _ => true,
                },
            };
            Ok::<_, KernelError>(tid_ok && asid_ok)
        })?;
        if !allowed {
            crate::yarm_log!(
                "IPC_REPLY_INCARNATION_REJECT replier_tid={} replier_asid={:?} record_responder={:?} record_replier_asid={:?}",
                replier_tid.0,
                replier_asid_now,
                self.with_ipc_state(|ipc| ipc.reply_caps[slot]
                    .and_then(|r| r.responder_tid)
                    .map(|t| t.0)),
                self.with_ipc_state(|ipc| ipc.reply_caps[slot].and_then(|r| r.replier_asid)),
            );
            return Err(KernelError::MissingRight);
        }
        let record = self.with_ipc_state_mut(|ipc| {
            let rec = ipc.reply_caps[slot].ok_or(KernelError::StaleCapability)?;
            ipc.reply_caps[slot] = None;
            Ok::<_, KernelError>(rec)
        })?;

        // ── No-alloc reply-cap cleanup ─────────────────────────────────────────
        // Replaces the previous revoke_capability_in_cnode() calls which allocated
        // up to 81920 bytes (Box<[Option<DelegatedCapabilityLink>; 2048]>) inside
        // collect_delegated_descendants() — causing a panic on freestanding AArch64
        // when the kernel heap was exhausted.
        //
        // The narrow fast-revoke path:
        // - performs no heap allocation
        // - does not traverse delegation trees (Reply caps are never delegated)
        // - clears exactly one cnode slot and bumps the generation
        // - returns bool for diagnostics only
        //
        // Failures are non-fatal: the global ReplyCapRecord is already consumed so
        // the reply is irrevocably committed. The worst case of a false return is a
        // leaked cnode slot, not a safety violation.
        let reply_object = capability.object;

        // Revoke the Reply cap from the replier's (current task's) cnode.
        //
        // Prefer `record.waiter_cap_id` (kernel-controlled CapId set during
        // materialization) over the user-supplied `reply_cap` argument. Both
        // should refer to the same slot, but using the kernel-recorded value
        // is more robust against a misbehaving replier passing a different CapId
        // that happens to resolve to the same Reply object.
        //
        // Fall back to `reply_cap` when `waiter_cap_id` is not set (e.g. the
        // receiver consumed the message via the legacy non-v2 recv path that
        // does not populate this field, or the materialization path was not the
        // direct-mint path).
        let replier_cap_to_revoke = record.waiter_cap_id.unwrap_or(reply_cap);
        let replier_ok = if let Some(replier_cnode) = self.current_task_cnode() {
            self.fast_revoke_reply_cap_in_cnode(replier_cnode, replier_cap_to_revoke, reply_object)
        } else {
            false
        };
        crate::yarm_log!(
            "IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid={} replier_tid={} cap={} waiter_cap={} expected={:?} ok={}",
            record.caller_tid.0,
            replier_tid.0,
            reply_cap.0,
            replier_cap_to_revoke.0,
            reply_object,
            replier_ok
        );
        if !replier_ok {
            // If waiter_cap_id revoke failed, attempt with user-supplied reply_cap as
            // a best-effort fallback (covers the case where waiter_cap_id wasn't set).
            if record.waiter_cap_id.is_some() && replier_cap_to_revoke.0 != reply_cap.0 {
                let fallback_ok = if let Some(replier_cnode) = self.current_task_cnode() {
                    self.fast_revoke_reply_cap_in_cnode(replier_cnode, reply_cap, reply_object)
                } else {
                    false
                };
                if fallback_ok {
                    crate::yarm_log!(
                        "IPC_REPLY_FAST_REVOKE_FAIL reason=waiter_cap_mismatch_used_fallback_reply_cap ok={}",
                        fallback_ok
                    );
                } else {
                    crate::yarm_log!(
                        "IPC_REPLY_FAST_REVOKE_FAIL reason=replier_cap_not_found_or_mismatch"
                    );
                }
            } else {
                crate::yarm_log!(
                    "IPC_REPLY_FAST_REVOKE_FAIL reason=replier_cap_not_found_or_mismatch"
                );
            }
        }

        // Revoke the Reply cap that create_reply_cap_for_caller minted into the
        // caller's cnode. record.caller_cap_id == 0 is the "not yet set" sentinel
        // (should never occur in practice; Phase-3 of create_reply_cap_for_caller
        // always updates it before returning to userspace).
        if record.caller_cap_id.0 != 0 {
            let caller_ok = if let Some(caller_cnode) = self.task_cnode(record.caller_tid.0) {
                self.fast_revoke_reply_cap_in_cnode(
                    caller_cnode,
                    record.caller_cap_id,
                    reply_object,
                )
            } else {
                false
            };
            crate::yarm_log!(
                "IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid={} cap={} expected={:?} ok={}",
                record.caller_tid.0,
                record.caller_cap_id.0,
                reply_object,
                caller_ok
            );
            if !caller_ok {
                crate::yarm_log!(
                    "IPC_REPLY_FAST_REVOKE_FAIL reason=caller_cap_not_found_or_mismatch"
                );
            }
        }

        // ── Deliver the reply ──────────────────────────────────────────────────
        let endpoint_idx = match self.resolve_endpoint_index(record.reply_endpoint) {
            Ok(idx) => idx,
            Err(err) => return Err(err),
        };
        if let CapObject::Reply { index, generation } = capability.object {
            crate::yarm_log!(
                "IPC_REPLY_OBJECT_OK tid={} cap={} reply_index={} generation={} target_endpoint={}",
                current_tid,
                reply_cap.0,
                index,
                generation,
                endpoint_idx
            );
        }
        // Phase 1: snapshot waiter TID under ipc_state_lock — consistent with the
        // Stage 4K/4L Phase 1–5 discipline.  The lock is released before Phase 2–5
        // so it is never held across user-memory copy, cap ops, or scheduler mutation.
        let opt_waiter_tid: Option<ThreadId> =
            self.with_ipc_state(|ipc| ipc.endpoint_waiter_tid(endpoint_idx));
        if let Some(waiter_tid) = opt_waiter_tid {
            crate::yarm_log!(
                "IPC_REPLY_DELIVER_TO_WAITER tid={} endpoint={} len={}",
                waiter_tid.0,
                endpoint_idx,
                msg.len
            );
            // Phase 2: confirm recv-v2 under task_state_lock (rank 3) before
            // re-acquiring ipc_state_lock (rank 4) in Phase 4.
            let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                    .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                    .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
            });
            if waiter_recv_v2_blocked {
                // Stage 188F: dispatch the reply delivery to the blocked recv-v2
                // caller through the 188B/188C/188D dispatch-return producers
                // (plain → ordinary-cap → reply-cap). On a stash, the seam mint +
                // 186E user copy + slot-clear + wake all run in the trap-entry drain
                // AFTER the broad borrow drops — no seam/copy/materialization here.
                // Shared-region replies and the no-drainer case return Ok(false) and
                // fall through to the unchanged legacy path below.
                match self.try_ipc_reply_boundary_split(waiter_tid.0, endpoint_idx, &msg) {
                    Ok(true) => {
                        // Slot-clear + wake happen in Phase C (the executor),
                        // preserving the legacy order copy → clear → wake.
                        return Ok(());
                    }
                    Ok(false) => { /* unsupported / no drainer — use the legacy path */ }
                    Err(e) => return Err(e),
                }
                // Phase 3: complete delivery outside all locks (legacy path for
                // shared-region replies, or when no trap-entry drainer is active).
                match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_REPLY_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                            waiter_tid.0,
                            endpoint_idx
                        );
                        self.note_ipc_reply_split_delivery();
                        // Phase 4: clear waiter slot under ipc_state_lock.
                        self.ipc_clear_plain_receiver_waiter_only(endpoint_idx, waiter_tid);
                        // Phase 5: wake receiver outside locks.
                        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(waiter_tid))?;
                        return Ok(());
                    }
                    Err(err) => {
                        crate::yarm_log!(
                            "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                            waiter_tid.0,
                            err
                        );
                        return Err(KernelError::UserMemoryFault);
                    }
                }
            }
            crate::yarm_log!("IPC_REPLY_WAKE_CALLER tid={}", waiter_tid.0);
        }
        // Phase 3: enqueue reply and atomically snapshot receiver waiter under
        // ipc_state_lock (rank 3).  Lock released before Phase 4 scheduler mutation.
        let wake_plan = self.with_ipc_state_mut(|ipc| {
            let ep_storage = ipc.endpoints[endpoint_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            kernel_mut(ep_storage)
                .send(msg)
                .map_err(|_| KernelError::EndpointQueueFull)?;
            Ok::<_, KernelError>(
                ipc.take_endpoint_waiter(endpoint_idx)
                    .map(|w| super::SchedulerWakePlan::Wake(w.tid))
                    .unwrap_or(super::SchedulerWakePlan::None),
            )
        })?;
        // Phase 4: wake receiver outside all locks.
        self.apply_scheduler_wake_plan(wake_plan)?;
        Ok(())
    }

    pub fn destroy_endpoint(&mut self, endpoint_idx: usize) -> Result<(), KernelError> {
        let limits = self.runtime_capacity_config();
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= limits.max_endpoints || ipc.endpoints[endpoint_idx].is_none() {
                return Err(KernelError::WrongObject);
            }
            ipc.endpoints[endpoint_idx] = None;
            ipc.endpoint_waiters[endpoint_idx] = None;
            ipc.endpoint_sender_waiters[endpoint_idx] = [None; MAX_ENDPOINT_SENDER_WAITERS];
            let mut next_generation = ipc.endpoint_generations[endpoint_idx].wrapping_add(1);
            if next_generation == 0 {
                next_generation = 1;
            }
            ipc.endpoint_generations[endpoint_idx] = next_generation;
            Ok(())
        })?;
        self.with_fault_state_mut(|faults| {
            if faults.fault_handler_endpoint == Some(endpoint_idx) {
                faults.fault_handler_endpoint = None;
            }
        });
        Ok(())
    }

    pub fn create_endpoint(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        self.create_endpoint_with_mode(max_depth, EndpointMode::Buffered)
    }

    pub fn create_endpoint_with_mode(
        &mut self,
        max_depth: usize,
        mode: EndpointMode,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let max_endpoints = self.runtime_capacity_config().max_endpoints;
        // Atomic under ipc_state_lock: find free slot, assign generation, store endpoint.
        // Capability minting happens outside the lock (capability rank 4 > ipc rank 3).
        let (endpoint_idx, generation) = self.with_ipc_state_mut(|ipc| {
            let slot = ipc
                .endpoints
                .iter()
                .take(max_endpoints)
                .position(Option::is_none)
                .ok_or(KernelError::EndpointFull)?;
            let mut next_gen = ipc.endpoint_generations[slot].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.endpoint_generations[slot] = next_gen;
            ipc.endpoints[slot] = Some(super::store_kernel_value(
                Endpoint::new_with_mode(max_depth, mode).map_err(map_ipc_error)?,
            ));
            Ok::<_, KernelError>((slot, next_gen))
        })?;
        let send_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint {
                index: endpoint_idx,
                generation,
            },
            CapRights::SEND,
        ))?;
        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Endpoint {
                index: endpoint_idx,
                generation,
            },
            CapRights::RECEIVE,
        ))?;
        Ok((endpoint_idx, send_cap, recv_cap))
    }

    pub fn create_notification(
        &mut self,
        max_depth: usize,
    ) -> Result<(usize, CapId, CapId), KernelError> {
        let max_notifications = self.runtime_capacity_config().max_notifications;
        // Slot selection, generation bump, and object storage are atomic under
        // ipc_state_lock. Capability minting happens outside the lock (cap rank
        // 4 > ipc rank 3; acquiring both simultaneously would invert lock order).
        let (notification_idx, generation) = self.with_ipc_state_mut(|ipc| {
            let slot = ipc
                .notifications
                .iter()
                .take(max_notifications)
                .position(Option::is_none)
                .ok_or(KernelError::EndpointFull)?;
            let mut next_gen = ipc.notification_generations[slot].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.notification_generations[slot] = next_gen;
            // Defence-in-depth (Stage 21): a reused slot must start with no
            // stale waiter and no IRQ route inherited from a prior occupant.
            // `destroy_notification` already performs this teardown, but a slot
            // could also be freed by a future path that forgets to; sanitising
            // here guarantees the new notification cannot be targeted by a
            // route bound to the previous generation.
            ipc.notification_waiters[slot] = None;
            for route in ipc.irq_routes.iter_mut() {
                if *route == Some(slot) {
                    *route = None;
                }
            }
            ipc.notifications[slot] = Some(NotificationObject::new(max_depth)?);
            Ok::<_, KernelError>((slot, next_gen))
        })?;

        let notification_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation,
            },
            CapRights::SIGNAL,
        ))?;

        let recv_cap = self.mint_capability_for_active_cnode(Capability::new(
            CapObject::Notification {
                index: notification_idx,
                generation,
            },
            CapRights::RECEIVE,
        ))?;

        Ok((notification_idx, notification_cap, recv_cap))
    }

    fn resolve_notification_index(&self, object: CapObject) -> Result<usize, KernelError> {
        let limits = self.runtime_capacity_config();
        match object {
            CapObject::Notification { index, generation } => self.with_ipc_state(|ipc| {
                if index >= limits.max_notifications || ipc.notifications[index].is_none() {
                    return Err(KernelError::WrongObject);
                }
                if ipc.notification_generations[index] != generation {
                    return Err(KernelError::StaleCapability);
                }
                Ok(index)
            }),
            _ => Err(KernelError::WrongObject),
        }
    }

    pub fn bind_irq_notification(
        &mut self,
        irq_line: u16,
        notification_cap: CapId,
    ) -> Result<(), KernelError> {
        let capability = self
            .capability_service()
            .resolve_current_task_capability(notification_cap)
            .ok_or(KernelError::InvalidCapability)?;
        if !capability.has_right(CapRights::SIGNAL) {
            return Err(KernelError::MissingRight);
        }

        let notif_idx = self.resolve_notification_index(capability.object)?;
        let irq_idx = irq_line as usize;
        if irq_idx >= MAX_IRQ_LINES {
            return Err(KernelError::WrongObject);
        }
        self.with_ipc_state_mut(|ipc| {
            ipc.irq_routes[irq_idx] = Some(notif_idx);
        });
        Ok(())
    }

    fn signal_notification(
        &mut self,
        notification_idx: usize,
        irq_line: u16,
    ) -> Result<(), KernelError> {
        // Phase 1: send signal and snapshot waiter TID under ipc_state_lock.
        // Lock released before Phase 2 (task_state_lock, rank 2) to preserve ordering.
        let opt_waiter_tid = self.with_ipc_state_mut(|ipc| {
            let notif = ipc.notifications[notification_idx]
                .as_mut()
                .ok_or(KernelError::WrongObject)?;
            notif.send_irq(irq_line)?;
            Ok::<_, KernelError>(ipc.notification_waiters[notification_idx].take())
        })?;
        // Phase 2: wake waiter outside ipc_state_lock.
        //
        // Invariant (Stage 21): only a task that is still `Blocked(_)` may be
        // transitioned to `Runnable` and enqueued.  A waiter TID snapshotted
        // from `notification_waiters` can race against `exit_task` /
        // `mark_task_dead` (which clear the slot) or against a timeout that
        // already woke the task; if the slot was observed non-None but the TCB
        // is now Dead/Exited/Runnable/Running/Faulted, signalling must NOT
        // resurrect or double-enqueue it.  This mirrors the cross-CPU
        // `apply_cross_cpu_wake_task` guard and the WakeTask work-item policy.
        if let Some(waiter_tid) = opt_waiter_tid {
            let should_enqueue = self.with_tcbs_mut(|tcbs| {
                let Some(tcb) = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                else {
                    // Missing TID (recycled/never registered): silent no-op.
                    return Ok::<bool, KernelError>(false);
                };
                if matches!(tcb.status, TaskStatus::Blocked(_)) {
                    tcb.status = TaskStatus::Runnable;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            if should_enqueue {
                self.enqueue_task(waiter_tid.0)?;
            }
        }
        Ok(())
    }

    /// Tear down a notification object and all IRQ routes that target it.
    ///
    /// Stage 21 lifetime invariant: an IRQ route must never outlive the
    /// notification it points at, and a freed notification slot must carry no
    /// stale waiter.  Teardown order is:
    ///   1. Drop every `irq_routes` entry whose target is `notification_idx`
    ///      (route teardown BEFORE the object is removed, so a concurrent
    ///      `route_external_irq` either sees the live object or no route).
    ///   2. Clear the `notification_waiters` slot (the waiter, if any, is woken
    ///      by the task-exit / unblock path; here we only drop the dangling
    ///      reference so a later signal cannot target a stale TID).
    ///   3. Remove the object and bump the generation so any surviving
    ///      `CapObject::Notification` cap fails the generation check in
    ///      `resolve_notification_index` / `capability_object_live`.
    ///
    /// All three steps run under a single `ipc_state_lock` critical section so
    /// the route/object/generation transition is atomic with respect to IRQ
    /// delivery. Returns the snapshotted waiter TID (if any) so the caller can
    /// unblock it outside the lock; `None` if the slot was already empty.
    ///
    /// Stage 22: wired into the capability-revoke / cnode-teardown / task-exit
    /// cleanup paths via `revoke_capability_in_cnode` and
    /// `revoke_capability_direct_in_process_cnode` (see
    /// `capability_lifecycle_state.rs`). Callers invoke this AFTER releasing
    /// `capability_state_lock` (rank 4) so the `ipc_state_lock` (rank 3) acquired
    /// here never inverts the cap→ipc rank ordering.
    pub(crate) fn destroy_notification(
        &mut self,
        notification_idx: usize,
    ) -> Result<Option<ThreadId>, KernelError> {
        if notification_idx >= self.runtime_capacity_config().max_notifications {
            return Err(KernelError::WrongObject);
        }
        self.with_ipc_state_mut(|ipc| {
            if ipc.notifications[notification_idx].is_none() {
                return Err(KernelError::WrongObject);
            }
            // 1. Route teardown first.
            for route in ipc.irq_routes.iter_mut() {
                if *route == Some(notification_idx) {
                    *route = None;
                }
            }
            // 2. Snapshot + clear waiter.
            let waiter = ipc.notification_waiters[notification_idx].take();
            // 3. Remove object and bump generation to invalidate live caps.
            ipc.notifications[notification_idx] = None;
            let mut next_gen = ipc.notification_generations[notification_idx].wrapping_add(1);
            if next_gen == 0 {
                next_gen = 1;
            }
            ipc.notification_generations[notification_idx] = next_gen;
            Ok::<Option<ThreadId>, KernelError>(waiter)
        })
    }

    /// Stage 22: wake a waiter that was parked on a notification destroyed by a
    /// capability revoke / cnode teardown.
    ///
    /// `destroy_notification` snapshots and clears the waiter slot under
    /// `ipc_state_lock` (rank 3) and returns the TID so the caller can unblock it
    /// AFTER the lock is released. This must run outside both `ipc_state_lock`
    /// and `capability_state_lock` (rank 4) to preserve lock-rank ordering.
    ///
    /// Wake gating mirrors `signal_notification`: only a task still `Blocked(_)`
    /// is transitioned to `Runnable` and enqueued. A snapshotted TID can race
    /// against a timeout / exit that already moved the task out of `Blocked`, so
    /// a Dead/Exited/Runnable/Running/Faulted task must NOT be resurrected or
    /// double-enqueued. Once runnable, the woken task re-resolves its recv cap and
    /// observes the destroyed object (generation mismatch → stale error), so no
    /// extra cancellation signalling is needed here.
    pub(crate) fn wake_destroyed_notification_waiter(
        &mut self,
        waiter_tid: ThreadId,
    ) -> Result<(), KernelError> {
        let should_enqueue = self.with_tcbs_mut(|tcbs| {
            let Some(tcb) = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == waiter_tid.0)
            else {
                return Ok::<bool, KernelError>(false);
            };
            if matches!(tcb.status, TaskStatus::Blocked(_)) {
                tcb.status = TaskStatus::Runnable;
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        if should_enqueue {
            self.enqueue_task(waiter_tid.0)?;
        }
        Ok(())
    }

    pub fn route_external_irq(&mut self, irq_line: u16) -> Result<(), KernelError> {
        let irq_idx = irq_line as usize;
        let notification_idx =
            self.with_ipc_state(|ipc| ipc.irq_routes.get(irq_idx).copied().flatten());
        let Some(notification_idx) = notification_idx else {
            return Ok(());
        };
        // Stage 21: a hardware IRQ that lands on a route whose target
        // notification was destroyed (object slot now None) is a benign no-op,
        // not an error — the IRQ simply has nowhere to be delivered. Without
        // this, a stale route surviving a destroy would turn every spurious
        // interrupt into a kernel error. `destroy_notification` clears matching
        // routes, so this is defence-in-depth for the destroy/deliver race.
        match self.signal_notification(notification_idx, irq_line) {
            Ok(()) => Ok(()),
            Err(KernelError::WrongObject) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn ipc_send(&mut self, send_cap: CapId, msg: Message) -> Result<(), KernelError> {
        self.ipc_send_with_optional_deadline(send_cap, msg, None)
    }

    pub fn ipc_send_with_deadline(
        &mut self,
        send_cap: CapId,
        msg: Message,
        timeout_ticks: u64,
    ) -> Result<(), KernelError> {
        let deadline = if timeout_ticks == 0 {
            None
        } else {
            Some(self.scheduler_tick_now().wrapping_add(timeout_ticks))
        };
        self.ipc_send_with_optional_deadline(send_cap, msg, deadline)
    }

    fn ipc_send_with_optional_deadline(
        &mut self,
        send_cap: CapId,
        msg: Message,
        deadline: Option<u64>,
    ) -> Result<(), KernelError> {
        let capability = self.resolve_send_cap_task_local(send_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        if capability.object == CapObject::Kernel {
            return self.handle_restart_control_kernel_ipc(msg);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        // Stage 169 (D2-GENUINE-SEND): rank-clean phase instrumentation for the
        // blocking-send path, default-off behind `yarm.d2_send_genuine=1`
        // (settable on x86_64 only). CAP phase is complete here; the block +
        // dispatch phases (and the out-of-global-lock dispatch relocation) are in
        // `block_current_on_send_with_deadline`. Immediate delivery / queued send
        // are byte-identical whether the knob is on or off.
        if crate::kernel::boot::d2_send_genuine_enabled() {
            let d2_send_tid = self.current_tid().unwrap_or(0);
            crate::yarm_log!(
                "D2_SEND_GENUINE_CANDIDATE tid={} endpoint={}",
                d2_send_tid,
                endpoint_idx
            );
            crate::yarm_log!("D2_SEND_GENUINE_PHASE_CAP_OK tid={}", d2_send_tid);
        }

        // Phase 0: snapshot endpoint mode under ipc_state_lock.
        let endpoint_mode = self
            .with_ipc_state(|ipc| {
                ipc.endpoints
                    .get(endpoint_idx)
                    .and_then(Option::as_ref)
                    .map(|e| e.mode())
            })
            .ok_or(KernelError::WrongObject)?;

        if endpoint_mode == EndpointMode::Synchronous {
            // Phase 1: snapshot waiter TID under ipc_state_lock.  Lock released before
            // Phase 2 (task_state_lock, rank 3) to preserve lock-rank ordering.
            let opt_waiter_tid: Option<ThreadId> =
                self.with_ipc_state(|ipc| ipc.endpoint_waiter_tid(endpoint_idx));
            if let Some(waiter_tid) = opt_waiter_tid {
                crate::yarm_log!(
                    "IPC_SEND_SYNC_WAITER endpoint={} waiter_tid={}",
                    endpoint_idx,
                    waiter_tid.0
                );
                self.with_ipc_state_mut(|ipc| {
                    ipc.telemetry.fastpath_attempts =
                        ipc.telemetry.fastpath_attempts.saturating_add(1);
                });
                // Phase 2: check recv-v2 under task_state_lock (rank 3), outside
                // ipc_state_lock (rank 4), to preserve lock-rank ordering.
                let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                    tcbs.iter()
                        .flatten()
                        .find(|tcb| tcb.tid.0 == waiter_tid.0)
                        .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                        .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
                });
                if waiter_recv_v2_blocked {
                    // Phase 3: complete delivery outside all locks (TrapFrame/user-memory write).
                    crate::yarm_log!(
                        "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={} len={} reply_cap={}",
                        waiter_tid.0,
                        endpoint_idx,
                        msg.len,
                        msg.transferred_cap().map(|c| c.0).unwrap_or(u64::MAX)
                    );
                    match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                        Ok(()) => {
                            crate::yarm_log!(
                                "IPC_RECV_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                                waiter_tid.0,
                                endpoint_idx
                            );
                            crate::yarm_log!(
                                "IPC_RECV_ENQUEUE_SKIPPED_WAITER_COMPLETED endpoint={}",
                                endpoint_idx
                            );
                        }
                        Err(err) => {
                            crate::yarm_log!(
                                "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                                waiter_tid.0,
                                err
                            );
                            return Err(KernelError::UserMemoryFault);
                        }
                    }
                }
                // Phase 4: under ipc_state_lock — for legacy path enqueue message, then
                // clear waiter slot and bump telemetry.  Re-verifies waiter slot for
                // defence-in-depth.
                let wake_plan = self.ipc_try_send_sync_endpoint_only(
                    endpoint_idx,
                    waiter_tid,
                    msg,
                    waiter_recv_v2_blocked,
                )?;
                crate::yarm_log!("IPC_SEND_SYNC_WAKE_DONE waiter_tid={}", waiter_tid.0);
                // Phase 5: wake receiver outside all locks.
                self.apply_scheduler_wake_plan(wake_plan)?;
                // Phase 6: cooperative handoff outside all locks.
                let switched = self.apply_scheduler_handoff_plan(
                    super::SchedulerHandoffPlan::YieldTo(waiter_tid),
                )?;
                if switched {
                    self.with_ipc_state_mut(|ipc| {
                        ipc.telemetry.fastpath_switches =
                            ipc.telemetry.fastpath_switches.saturating_add(1);
                        ipc.telemetry.scheduler_fastpath_handoffs =
                            ipc.telemetry.scheduler_fastpath_handoffs.saturating_add(1);
                    });
                }
                crate::yarm_log!("IPC_SEND_SYNC_SWITCH_DONE waiter_tid={}", waiter_tid.0);
                return Ok(());
            }

            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
            });
            return Err(KernelError::WouldBlock);
        }

        if let Some(waiter_tid) = self.with_ipc_state(|ipc| ipc.endpoint_waiter_tid(endpoint_idx)) {
            crate::yarm_log!(
                "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={} len={} reply_cap={}",
                waiter_tid.0,
                endpoint_idx,
                msg.len,
                msg.transferred_cap().map(|c| c.0).unwrap_or(u64::MAX)
            );
            let waiter_recv_v2_blocked = self.with_tcbs(|tcbs| {
                tcbs.iter()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == waiter_tid.0)
                    .and_then(|tcb| tcb.blocked_recv_state.as_ref())
                    .is_some_and(|state| state.recv_abi == RecvAbiVariant::RecvV2)
            });
            if waiter_recv_v2_blocked {
                match complete_blocked_recv_for_waiter(self, waiter_tid.0, &msg) {
                    Ok(()) => {
                        crate::yarm_log!(
                            "IPC_RECV_DELIVER_TO_WAITER_CONSUMED tid={} endpoint={}",
                            waiter_tid.0,
                            endpoint_idx
                        );
                        crate::yarm_log!(
                            "IPC_RECV_ENQUEUE_SKIPPED_WAITER_COMPLETED endpoint={}",
                            endpoint_idx
                        );
                        self.wake_waiter_for_endpoint(endpoint_idx)?;
                        return Ok(());
                    }
                    Err(err) => {
                        crate::yarm_log!(
                            "IPC_RECV_BLOCKED_COMPLETE_FAILED tid={} err={:?}",
                            waiter_tid.0,
                            err
                        );
                        return Err(KernelError::UserMemoryFault);
                    }
                }
            }
        }
        let queued = self.with_ipc_state_mut(|ipc| {
            let Some(endpoint_storage) =
                ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut)
            else {
                return Err(KernelError::WrongObject);
            };
            let endpoint = kernel_mut(endpoint_storage);
            Ok(endpoint.send(msg).is_ok())
        })?;
        if !queued {
            crate::yarm_log!("IPC_SEND_SYNC_NO_WAITER endpoint={}", endpoint_idx);
            let _ =
                self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
            });
            return Err(KernelError::WouldBlock);
        }

        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_sends = ipc.telemetry.queued_sends.saturating_add(1);
        });
        self.wake_waiter_for_endpoint(endpoint_idx)?;
        if crate::kernel::boot::d2_send_genuine_enabled() {
            let d2_send_tid = self.current_tid().unwrap_or(0);
            crate::yarm_log!("D2_SEND_GENUINE_IMMEDIATE_OK tid={}", d2_send_tid);
            crate::yarm_log!("D2_SEND_GENUINE_DONE result=immediate tid={}", d2_send_tid);
        }
        Ok(())
    }

    pub(crate) fn note_endpoint_only_queued_send_split(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_sends = ipc.telemetry.queued_sends.saturating_add(1);
        });
    }

    pub(crate) fn note_endpoint_only_queued_recv_split(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.queued_recvs = ipc.telemetry.queued_recvs.saturating_add(1);
        });
    }

    /// Stage 4K: clear `endpoint_waiters[endpoint_idx]` under `ipc_state_lock` if and only if
    /// the slot still holds `expected_receiver_tid`.  Called after `complete_blocked_recv_for_waiter`
    /// succeeds to finalise the three-phase recv-v2 split delivery.  Under the global kernel lock
    /// this always matches; the conditional re-verify is a defence-in-depth check.
    pub(crate) fn ipc_clear_plain_receiver_waiter_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver_tid: ThreadId,
    ) {
        // Stage 198E3B2B2: clear by the receiver's COMPLETE identity (tid + its current ASID) — a
        // stale slot bearing a different ASID (numeric TID reuse) is left untouched.
        let expected = ReceiverWaiterIdentity::new(
            expected_receiver_tid,
            self.task_asid(expected_receiver_tid.0).unwrap_or(Asid(0)),
        );
        self.with_ipc_state_mut(|ipc| {
            if ipc.clear_endpoint_waiter_if_identity(endpoint_idx, expected) {
                crate::yarm_log!(
                    "IPC_SEND_SPLIT_RECV_V2_CLEAR_WAITER receiver_tid={}",
                    expected_receiver_tid.0
                );
            }
        });
    }

    /// Stage 4Q: synchronous-endpoint send — clear waiter slot and (for legacy receivers)
    /// enqueue message, all under `ipc_state_lock`.
    ///
    /// Preconditions (caller must verify before this call):
    ///   - Phase 2 recv-v2 check done under task_state_lock (rank 3), outside ipc_state_lock
    ///   - if `recv_v2_completed` is true, message was already written to receiver's TrapFrame
    ///     via `complete_blocked_recv_for_waiter` outside all locks (Phase 3)
    ///
    /// Under ipc_state_lock:
    ///   - re-verifies receiver slot still holds `expected_receiver_tid` (defence-in-depth)
    ///   - for legacy (non-recv-v2): enqueues `msg` into the endpoint queue
    ///   - clears `endpoint_waiters` slot
    ///   - bumps `rendezvous_handoffs` telemetry
    ///
    /// Returns `SchedulerWakePlan::Wake(tid)` on success; caller must apply outside the lock
    /// via `apply_scheduler_wake_plan`, then optionally `apply_scheduler_handoff_plan`.
    pub(crate) fn ipc_try_send_sync_endpoint_only(
        &mut self,
        endpoint_idx: usize,
        expected_receiver_tid: ThreadId,
        msg: Message,
        recv_v2_completed: bool,
    ) -> Result<super::SchedulerWakePlan, KernelError> {
        // Stage 198E3B2B2: re-verify + clear by the receiver's COMPLETE identity (tid + current ASID).
        let expected = ReceiverWaiterIdentity::new(
            expected_receiver_tid,
            self.task_asid(expected_receiver_tid.0).unwrap_or(Asid(0)),
        );
        self.with_ipc_state_mut(|ipc| {
            // Re-verify waiter slot by full identity: defence-in-depth under the global kernel lock.
            if ipc.endpoint_waiter_identity(endpoint_idx) != Some(expected) {
                return Err(KernelError::WrongObject);
            }
            if !recv_v2_completed {
                // Legacy path: deliver message via endpoint queue.
                let Some(endpoint_storage) =
                    ipc.endpoints.get_mut(endpoint_idx).and_then(Option::as_mut)
                else {
                    return Err(KernelError::WrongObject);
                };
                let endpoint = kernel_mut(endpoint_storage);
                if endpoint.mode() != EndpointMode::Synchronous {
                    return Err(KernelError::WrongObject);
                }
                crate::yarm_log!(
                    "IPC_RECV_DELIVER_TO_WAITER tid={} endpoint={}",
                    expected_receiver_tid.0,
                    endpoint_idx
                );
                endpoint
                    .send(msg)
                    .map_err(|_| KernelError::EndpointQueueFull)?;
            }
            ipc.clear_endpoint_waiter_if_identity(endpoint_idx, expected);
            ipc.telemetry.rendezvous_handoffs = ipc.telemetry.rendezvous_handoffs.saturating_add(1);
            crate::yarm_log!(
                "IPC_SEND_SYNC_CLEAR_WAITER receiver_tid={}",
                expected_receiver_tid.0
            );
            Ok(super::SchedulerWakePlan::Wake(expected_receiver_tid))
        })
    }

    pub(crate) fn note_split_recv_v2_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.split_recv_v2_deliveries =
                ipc.telemetry.split_recv_v2_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_ipc_call_split_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.ipc_call_split_deliveries =
                ipc.telemetry.ipc_call_split_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_ipc_reply_split_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.ipc_reply_split_deliveries =
                ipc.telemetry.ipc_reply_split_deliveries.saturating_add(1);
        });
    }

    pub(crate) fn note_cap_transfer_recv_v2_delivery(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.cap_transfer_recv_v2_deliveries = ipc
                .telemetry
                .cap_transfer_recv_v2_deliveries
                .saturating_add(1);
        });
    }

    pub(crate) fn note_cap_transfer_stage4e_enqueued(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.cap_transfer_stage4e_enqueued = ipc
                .telemetry
                .cap_transfer_stage4e_enqueued
                .saturating_add(1);
        });
    }

    /// Stage 104 / D1: count a recv-side cap materialization serviced through
    /// the phase-separated split router (`cap_transfer_split`). Incremented
    /// only for the supported case (transfer-cap, non-reply, non-shared-region);
    /// fallback materializations keep this counter unchanged, which lets tests
    /// assert the routing decision itself.
    pub(crate) fn note_d1_split_materialize(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d1_split_materializations =
                ipc.telemetry.d1_split_materializations.saturating_add(1);
        });
    }

    /// Stage 105 / D5: count a reply-cap recv-side materialization serviced
    /// through the phase-separated split engine. Incremented only for the
    /// supported FLAG_REPLY_CAP case routed through `cap_transfer_split`;
    /// fallback materializations (sender-waiter cap-transfer, etc.) keep
    /// this counter unchanged.
    pub(crate) fn note_d5_split_reply_materialize(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d5_split_reply_materializations = ipc
                .telemetry
                .d5_split_reply_materializations
                .saturating_add(1);
        });
    }

    /// Stage 105 / D5: count a reply-cap split materialization that hit the
    /// mint→record race window and rolled back the mint. Incremented once
    /// per Phase B' rollback regardless of the stale subtype
    /// (`IndexOutOfRange` / `GenerationMismatch` / `SlotEmpty`).
    pub(crate) fn note_d5_split_reply_rollback(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d5_split_reply_rollbacks =
                ipc.telemetry.d5_split_reply_rollbacks.saturating_add(1);
        });
    }

    /// Stage 105 / D2 audit helper — atomic recv-waiter publish under
    /// `ipc_state_lock` (rank 3) only.
    ///
    /// VALIDATION: D2_HELPER_ONLY — not called from any live path.
    ///
    /// Semantics:
    /// - Returns `QueueNonEmpty` if the endpoint queue is non-empty (the
    ///   caller must dequeue rather than block).
    /// - Returns `ReceiverAlreadyWaiting` if `endpoint_waiters[idx]` is
    ///   already set.
    /// - Returns `InvalidEndpoint` for an out-of-range index or a vacant
    ///   endpoint slot.
    /// - Returns `Published` after writing
    ///   `endpoint_waiters[idx] = Some(receiver_tid)`.
    ///
    /// All four outcomes are decided inside the same critical section as the
    /// write, which is the contract the D2 live split would rely on: any
    /// sender enqueuing AFTER `Published` is observed sees the waiter, and
    /// any sender enqueuing BEFORE sees `None` and writes to the queue (which
    /// flips this helper from `Published` to `QueueNonEmpty` on the next
    /// attempt — i.e. the receiver is steered to dequeue, no wake lost).
    pub(crate) fn try_publish_recv_waiter_audit_only(
        &mut self,
        endpoint_idx: usize,
        receiver: ReceiverWaiterIdentity,
        recv_cap: CapId,
    ) -> crate::kernel::recv_waiter_split::PublishWaiterOutcome {
        use crate::kernel::recv_waiter_split::PublishWaiterOutcome;
        self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return PublishWaiterOutcome::InvalidEndpoint;
            }
            let endpoint = match ipc.endpoints[endpoint_idx].as_ref() {
                Some(e) => e,
                None => return PublishWaiterOutcome::InvalidEndpoint,
            };
            if endpoint.queued() > 0 {
                return PublishWaiterOutcome::QueueNonEmpty;
            }
            if ipc.endpoint_waiter_present(endpoint_idx) {
                return PublishWaiterOutcome::ReceiverAlreadyWaiting;
            }
            // Stage 198E3B2B2: store the COMPLETE generation-bearing identity.
            ipc.set_endpoint_waiter(endpoint_idx, receiver);
            crate::yarm_log!(
                "D2_RECV_WAITER_PUBLISH_AUDIT endpoint={} tid={} asid={} recv_cap={}",
                endpoint_idx,
                receiver.tid.0,
                receiver.asid.0,
                recv_cap.0
            );
            PublishWaiterOutcome::Published
        })
    }

    /// Stage 106 / D2 LIVE primitive — atomic queue-recheck + waiter publish
    /// under `ipc_state_lock` (rank 3) only.
    ///
    /// VALIDATION: D2_LIVE_SPLIT — called from
    /// `block_current_on_receive_with_deadline` (the canonical blocking-recv
    /// path) since Stage 106.
    ///
    /// Differences from the Stage 105 audit primitive
    /// ([`Self::try_publish_recv_waiter_audit_only`]):
    ///
    /// - **Overwrite semantics for a pre-existing waiter** — byte-identical
    ///   to the pre-Stage-106 unconditional
    ///   `endpoint_waiters[idx] = Some(tid)` write. A displaced waiter is
    ///   logged (`D2_RECV_WAITER_DISPLACED`, additive marker) but the
    ///   behavior matches the canonical path exactly: last receiver wins.
    ///   This primitive therefore NEVER returns `ReceiverAlreadyWaiting`.
    /// - Returns `QueueNonEmpty` (without publishing) when a sender's message
    ///   landed in the queue — the caller drives the no-lost-wakeup unwind.
    ///   Under the serialized global lock this outcome is unreachable; it is
    ///   the future-split correctness branch.
    ///
    /// Telemetry: increments `d2_recv_waiter_publishes` on `Published`.
    pub(crate) fn publish_recv_waiter_live(
        &mut self,
        endpoint_idx: usize,
        receiver: ReceiverWaiterIdentity,
        recv_cap: CapId,
    ) -> crate::kernel::recv_waiter_split::PublishWaiterOutcome {
        use crate::kernel::recv_waiter_split::PublishWaiterOutcome;
        let receiver_tid = receiver.tid;
        let outcome = self.with_ipc_state_mut(|ipc| {
            if endpoint_idx >= ipc.endpoints.len() {
                return PublishWaiterOutcome::InvalidEndpoint;
            }
            let endpoint = match ipc.endpoints[endpoint_idx].as_ref() {
                Some(e) => e,
                None => return PublishWaiterOutcome::InvalidEndpoint,
            };
            if endpoint.queued() > 0 {
                return PublishWaiterOutcome::QueueNonEmpty;
            }
            // Stage 198E3B2B2: store the COMPLETE generation-bearing identity (canonical overwrite
            // semantics preserved — a displaced waiter is logged, last receiver wins).
            if let Some(displaced) = ipc.set_endpoint_waiter(endpoint_idx, receiver) {
                crate::yarm_log!(
                    "D2_RECV_WAITER_DISPLACED endpoint={} old_tid={} new_tid={}",
                    endpoint_idx,
                    displaced.tid.0,
                    receiver_tid.0
                );
            }
            crate::yarm_log!(
                "D2_RECV_WAITER_PUBLISH endpoint={} tid={} asid={} recv_cap={}",
                endpoint_idx,
                receiver_tid.0,
                receiver.asid.0,
                recv_cap.0
            );
            // Stage 193B: if this is the send-plain oracle loopback E1, push a
            // deterministic "receiver blocked" signal into the coordination
            // endpoint E2 WITHIN this same `ipc_state_lock` section (atomic with
            // the waiter publish) so init plain-sends only after the receiver is
            // provably a waiter — no enqueue race. Strict no-op off the sub-knob.
            if let Some(e2_idx) = super::proof_send_plain_oracle_coordination_target(endpoint_idx) {
                super::proof_send_plain_oracle_push_coordination_locked(
                    ipc,
                    e2_idx,
                    receiver_tid.0,
                );
            }
            PublishWaiterOutcome::Published
        });
        if matches!(outcome, PublishWaiterOutcome::Published) {
            self.note_d2_recv_waiter_publish();
        }
        outcome
    }

    /// Stage 106 / D2: count a live waiter publish through
    /// [`Self::publish_recv_waiter_live`].
    pub(crate) fn note_d2_recv_waiter_publish(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d2_recv_waiter_publishes =
                ipc.telemetry.d2_recv_waiter_publishes.saturating_add(1);
        });
    }

    /// Stage 106 / D2: count a no-lost-wakeup unwind (publish returned
    /// `QueueNonEmpty` after the scheduler block — the future-split race
    /// branch). Always 0 under the serialized global lock; the counter exists
    /// so post-seam-split smoke can detect the branch being taken.
    pub(crate) fn note_d2_publish_race_unwind(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d2_publish_race_unwinds =
                ipc.telemetry.d2_publish_race_unwinds.saturating_add(1);
        });
    }

    /// Stage 107 / D3: count a routed VmBrk shrink invocation through
    /// `vm_brk_shrink_two_phase`. The counter is keyed off the IPC telemetry
    /// struct because that is the kernel's shared telemetry surface; no IPC
    /// state is touched by this helper. `pages_unmapped` and `shootdowns`
    /// are also accumulated for smoke-grep parity.
    pub(crate) fn note_d3_vm_brk_shrink(&mut self, pages_unmapped: usize, shootdowns: usize) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d3_vm_brk_shrink_calls =
                ipc.telemetry.d3_vm_brk_shrink_calls.saturating_add(1);
            ipc.telemetry.d3_vm_brk_shrink_pages_unmapped = ipc
                .telemetry
                .d3_vm_brk_shrink_pages_unmapped
                .saturating_add(pages_unmapped as u64);
            ipc.telemetry.d3_vm_brk_shrink_shootdowns = ipc
                .telemetry
                .d3_vm_brk_shrink_shootdowns
                .saturating_add(shootdowns as u64);
        });
    }

    /// Stage 107 / D6: count a local-CPU dispatch through the typed helper
    /// `local_dispatch_step_split`. Same telemetry-keying rationale as
    /// `note_d3_vm_brk_shrink`.
    pub(crate) fn note_d6_local_dispatch(&mut self) {
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.d6_local_dispatch_calls =
                ipc.telemetry.d6_local_dispatch_calls.saturating_add(1);
        });
    }

    /// Stage 106 / D2 test hook: expose the private `wake_tid_to_runnable`
    /// so the executable no-lost-wakeup unwind proof
    /// (`stage106_d2_no_lost_wakeup_unwind_sequence_drains_message`) can
    /// replicate the exact unwind sequence the live path performs.
    #[cfg(test)]
    pub(crate) fn wake_tid_to_runnable_for_test(
        &mut self,
        tid: ThreadId,
    ) -> Result<(), KernelError> {
        self.wake_tid_to_runnable(tid)
    }

    fn handle_restart_control_kernel_ipc(&mut self, msg: Message) -> Result<(), KernelError> {
        if msg.opcode != PROC_OP_EXECUTE_RESTART {
            return Err(KernelError::WrongObject);
        }
        let args = ExecuteRestartRequest::decode(msg.as_slice())
            .map_err(|_| KernelError::UserMemoryFault)?;
        let status = match self.restart_task(args.tid, args.restart_token) {
            Ok(()) => ExecuteRestartReply::STATUS_OK,
            Err(KernelError::TaskMissing) => ExecuteRestartReply::STATUS_NOT_FOUND,
            Err(KernelError::WrongObject) => ExecuteRestartReply::STATUS_TOKEN_MISMATCH,
            Err(KernelError::MissingRight) => ExecuteRestartReply::STATUS_PERMISSION_DENIED,
            Err(_) => ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED,
        };
        let reply_cap = msg.transferred_cap().ok_or(KernelError::MissingRight)?;
        let reply = Message::with_header(
            0,
            PROC_OP_EXECUTE_RESTART,
            0,
            None,
            &ExecuteRestartReply::new(status).encode(),
        )
        .map_err(|_| KernelError::UserMemoryFault)?;
        self.ipc_reply(CapId(reply_cap.0), reply)
    }

    pub fn ipc_send_fastpath(
        &mut self,
        send_cap: CapId,
        msg: Message,
    ) -> Result<IpcFastpathResult, KernelError> {
        let capability = self.resolve_send_cap_task_local(send_cap)?;
        if !capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        let (endpoint_mode, waiter_tid) = self.with_ipc_state(|ipc| {
            let mode = ipc
                .endpoints
                .get(endpoint_idx)
                .and_then(Option::as_ref)
                .map(|e| e.mode());
            let waiter = ipc.endpoint_waiter_tid(endpoint_idx);
            (mode, waiter)
        });
        let endpoint_mode = endpoint_mode.ok_or(KernelError::WrongObject)?;
        let inline_sync_handoff =
            endpoint_mode == EndpointMode::Synchronous && waiter_tid.is_some();
        if !inline_sync_handoff {
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.fastpath_attempts = ipc.telemetry.fastpath_attempts.saturating_add(1);
            });
        }

        self.ipc_send(send_cap, msg)?;

        let switched = if inline_sync_handoff {
            true
        } else if waiter_tid.is_some() {
            self.apply_scheduler_handoff_plan(super::SchedulerHandoffPlan::YieldTo(
                waiter_tid.expect("checked is_some"),
            ))?
        } else {
            false
        };

        if switched && !inline_sync_handoff {
            self.with_ipc_state_mut(|ipc| {
                ipc.telemetry.fastpath_switches = ipc.telemetry.fastpath_switches.saturating_add(1);
                ipc.telemetry.scheduler_fastpath_handoffs =
                    ipc.telemetry.scheduler_fastpath_handoffs.saturating_add(1);
            });
        }

        Ok(IpcFastpathResult {
            switched_to_waiter: switched,
        })
    }

    pub fn ipc_send_with_cap_transfer(
        &mut self,
        send_cap: CapId,
        sender_tid: ThreadId,
        opcode: u16,
        transfer_cap: CapId,
        payload: &[u8],
    ) -> Result<(), KernelError> {
        // Resolve all capabilities in the sender's cspace to keep authorization
        // task-local even for kernel-internal transfer staging paths.
        let _ = self.resolve_capability_for_task(sender_tid.0, transfer_cap)?;
        let send_capability = self.resolve_capability_for_task(sender_tid.0, send_cap)?;
        if !send_capability.has_right(CapRights::SEND) {
            return Err(KernelError::MissingRight);
        }
        let endpoint_idx = self.resolve_endpoint_index(send_capability.object)?;
        let waiter_tid = self
            .with_ipc_state(|ipc| ipc.endpoint_waiter_tid(endpoint_idx))
            .ok_or(KernelError::WouldBlock)?;
        let transfer_handle = self
            .stash_transfer_envelope(
                sender_tid,
                transfer_cap,
                send_capability.object,
                Some(waiter_tid),
                None,
            )
            .ok_or(KernelError::EndpointQueueFull)?;
        let msg = Message::with_header(
            sender_tid.0,
            opcode,
            Message::FLAG_CAP_TRANSFER,
            Some(transfer_handle),
            payload,
        )
        .map_err(map_ipc_error)?;
        if let Err(err) = self.ipc_send(send_cap, msg) {
            let _ =
                self.take_transfer_envelope(transfer_handle, send_capability.object, sender_tid);
            return Err(err);
        }
        Ok(())
    }

    /// Atomically dequeue one message from the endpoint and compact sender-waiter queue.
    ///
    /// All mutations happen under `ipc_state_lock` (rank 3).  Returns the message (if any)
    /// and a deferred wake plan to apply after the lock is released.
    ///
    /// Protocol:
    ///   1a. Dequeue from endpoint queue (scoped borrow released before step 1b).
    ///   1b. Compact sender-waiter queue: take head, shift remainder left.
    ///   2.  If message + waiter: refill endpoint with waiter's message → WakeSender.
    ///       If message, no waiter: return message → None wake.
    ///       If no message + waiter: direct delivery of waiter's message → WakeSender.
    ///       If neither: return None → None wake.
    pub(crate) fn ipc_recv_endpoint_take(
        &mut self,
        endpoint_idx: usize,
    ) -> Result<(Option<Message>, super::SchedulerWakePlan), KernelError> {
        self.with_ipc_state_mut(|ipc| {
            // 1a: Dequeue from endpoint queue; scoped block releases the borrow.
            let opt_msg = {
                let Some(ep_storage) = ipc.endpoints[endpoint_idx].as_mut() else {
                    return Err(KernelError::WrongObject);
                };
                kernel_mut(ep_storage).recv()
            };
            // 1b: Compact sender-waiter queue.
            //
            // Scan for the first live sender rather than always taking slot[0]:
            // process_ipc_timeout_deadlines nulls expired slots in-place without
            // compacting, creating sparse queues ([None, Some(B), ...]). Taking
            // only slot[0] would miss live senders at positions > 0, permanently
            // stranding them.  Full left-compaction after the take keeps the queue
            // dense for subsequent operations.
            let opt_waiter = {
                let queue = &mut ipc.endpoint_sender_waiters[endpoint_idx];
                if let Some(idx) = queue.iter().position(Option::is_some) {
                    let head = queue[idx].take().expect("position guarantees Some");
                    // Full left-compact: move remaining Some entries to the front.
                    let mut write = 0;
                    for read in 0..queue.len() {
                        if queue[read].is_some() {
                            queue[write] = queue[read].take();
                            write += 1;
                        }
                    }
                    Some(head)
                } else {
                    None
                }
            };
            match (opt_msg, opt_waiter) {
                (Some(msg), Some(waiter)) => {
                    // Endpoint slot just freed: refill with waiter's message.
                    let ep_storage = ipc.endpoints[endpoint_idx]
                        .as_mut()
                        .expect("endpoint must exist after recv");
                    kernel_mut(ep_storage)
                        .send(waiter.msg)
                        .map_err(|_| KernelError::EndpointQueueFull)?;
                    Ok((Some(msg), super::SchedulerWakePlan::Wake(waiter.tid)))
                }
                (Some(msg), None) => Ok((Some(msg), super::SchedulerWakePlan::None)),
                (None, Some(waiter)) => {
                    // Direct delivery: bypass endpoint queue.
                    Ok((Some(waiter.msg), super::SchedulerWakePlan::Wake(waiter.tid)))
                }
                (None, None) => Ok((None, super::SchedulerWakePlan::None)),
            }
        })
    }

    pub fn try_ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        // Probe path resolves receive capability in the current task cspace.
        let capability = self.resolve_recv_cap_task_local(recv_cap)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        if let CapObject::Notification { .. } = capability.object {
            let notif_idx = self.resolve_notification_index(capability.object)?;
            let result = self
                .with_ipc_state_mut(|ipc| ipc.notifications[notif_idx].as_mut().map(|n| n.recv()));
            return result.ok_or(KernelError::WrongObject);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        self.apply_scheduler_wake_plan(wake_plan)?;
        Ok(msg)
    }

    pub fn ipc_recv(&mut self, recv_cap: CapId) -> Result<Option<Message>, KernelError> {
        self.ipc_recv_with_optional_deadline(recv_cap, None)
    }

    pub fn ipc_recv_with_deadline(
        &mut self,
        recv_cap: CapId,
        timeout_ticks: u64,
    ) -> Result<Option<Message>, KernelError> {
        let deadline = if timeout_ticks == 0 {
            None
        } else {
            Some(self.scheduler_tick_now().wrapping_add(timeout_ticks))
        };
        self.ipc_recv_with_optional_deadline(recv_cap, deadline)
    }

    pub fn ipc_recv_until_deadline(
        &mut self,
        recv_cap: CapId,
        deadline_tick: u64,
    ) -> Result<Option<Message>, KernelError> {
        self.ipc_recv_with_optional_deadline(recv_cap, Some(deadline_tick))
    }

    fn ipc_recv_with_optional_deadline(
        &mut self,
        recv_cap: CapId,
        deadline: Option<u64>,
    ) -> Result<Option<Message>, KernelError> {
        let capability = self.resolve_recv_cap_task_local(recv_cap)?;
        if !capability.has_right(CapRights::RECEIVE) {
            return Err(KernelError::MissingRight);
        }
        if let CapObject::Notification { .. } = capability.object {
            let notif_idx = self.resolve_notification_index(capability.object)?;
            // Try immediate recv under ipc_state_lock.
            let immediate = self
                .with_ipc_state_mut(|ipc| ipc.notifications[notif_idx].as_mut().map(|n| n.recv()));
            match immediate {
                None => return Err(KernelError::WrongObject),
                Some(Some(msg)) => return Ok(Some(msg)),
                Some(None) => {}
            }
            // Nothing available — block.
            let blocked_tid = self.block_current_cpu().ok_or(KernelError::TaskMissing)?;
            self.with_tcbs_mut(|tcbs| {
                let tcb = tcbs
                    .iter_mut()
                    .flatten()
                    .find(|tcb| tcb.tid.0 == blocked_tid)
                    .ok_or(KernelError::TaskMissing)?;
                tcb.status = TaskStatus::Blocked(WaitReason::EndpointReceive(recv_cap));
                tcb.ipc_timeout_deadline = deadline;
                tcb.ipc_timeout_fired = false;
                Ok::<_, KernelError>(())
            })?;
            // Publish waiter under ipc_state_lock after TCB is Blocked.
            self.with_ipc_state_mut(|ipc| {
                ipc.notification_waiters[notif_idx] = Some(ThreadId(blocked_tid));
            });
            let _ = self.dispatch_next_task()?;
            return Ok(None);
        }

        let endpoint_idx = self.resolve_endpoint_index(capability.object)?;
        // Stage 168 (D2-GENUINE-RECV): rank-clean phase instrumentation for the
        // canonical blocking-recv path, default-off behind `yarm.d2_recv_genuine=1`
        // (settable on x86_64 only). The phase boundaries below are the SAME
        // rank-ordered scheduler(1) → task(2) → ipc(3) → dispatch steps the path
        // already runs; the knob only exposes them live (and, with
        // `yarm.d6_genuine=1`, routes the block's dispatch through the Stage 168
        // out-of-global-lock seam where eligible). Immediate / NoWait / timeout /
        // rollback semantics are byte-identical whether the knob is on or off.
        let d2_recv_genuine = crate::kernel::boot::d2_recv_genuine_enabled();
        let d2_recv_tid = self.current_tid().unwrap_or(0);
        if d2_recv_genuine {
            crate::yarm_log!(
                "D2_RECV_GENUINE_CANDIDATE tid={} endpoint={}",
                d2_recv_tid,
                endpoint_idx
            );
            crate::yarm_log!("D2_RECV_GENUINE_PHASE_CAP_OK tid={}", d2_recv_tid);
        }
        // Phase 1: try immediate recv under ipc_state_lock.
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        if d2_recv_genuine {
            crate::yarm_log!("D2_RECV_GENUINE_PHASE_IPC_LOCK tid={}", d2_recv_tid);
        }
        self.apply_scheduler_wake_plan(wake_plan)?;
        if msg.is_some() {
            if d2_recv_genuine {
                crate::yarm_log!("D2_RECV_GENUINE_IMMEDIATE_OK tid={}", d2_recv_tid);
                crate::yarm_log!("D2_RECV_GENUINE_DONE result=immediate tid={}", d2_recv_tid);
            }
            return Ok(msg);
        }

        let blocked_tid =
            self.block_current_on_receive_with_deadline(endpoint_idx, recv_cap, deadline)?;
        let timed_out = self.consume_ipc_timeout_fired_for_tid(blocked_tid.0)?;
        if timed_out {
            if d2_recv_genuine {
                crate::yarm_log!("D2_RECV_GENUINE_TIMEOUT_OK tid={}", blocked_tid.0);
                crate::yarm_log!("D2_RECV_GENUINE_DONE result=timeout tid={}", blocked_tid.0);
            }
            return Ok(None);
        }
        // Phase 2: post-wake recv under ipc_state_lock (sender may have delivered directly).
        let (msg, wake_plan) = self.ipc_recv_endpoint_take(endpoint_idx)?;
        self.apply_scheduler_wake_plan(wake_plan)?;
        if d2_recv_genuine {
            crate::yarm_log!("D2_RECV_GENUINE_BLOCKED_OK tid={}", blocked_tid.0);
            crate::yarm_log!(
                "D2_RECV_GENUINE_DONE result={} tid={}",
                if msg.is_some() {
                    "delivered"
                } else {
                    "woken_empty"
                },
                blocked_tid.0
            );
        }
        Ok(msg)
    }
}
