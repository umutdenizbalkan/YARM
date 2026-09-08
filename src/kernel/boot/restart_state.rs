// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::{KernelError, KernelState};
use crate::kernel::ipc::Message;
use crate::kernel::task::{RestartToken, TaskStatus, ThreadDetachState};
use yarm_ipc_abi::process_abi::{KERNEL_OP_PM_TASK_EXITED, KernelPmTaskExitedPayload};
use yarm_ipc_abi::supervisor_abi::{
    SUPERVISOR_OP_TASK_EXITED, SUPERVISOR_OP_TRANSFER_REVOKED, encode_task_exited_event,
    encode_transfer_revoked_event,
};

impl KernelState {
    pub fn report_task_exit_to_supervisor(
        &mut self,
        tid: u64,
        code: u64,
        restart_token: u64,
    ) -> Result<(), KernelError> {
        crate::yarm_log!("TASK_EXITED_REPORT_BEGIN tid={}", tid);
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            crate::yarm_log!(
                "TASK_EXITED_REPORT_FAIL tid={} reason=no-supervisor-endpoint",
                tid
            );
            return Ok(());
        };
        let msg = Message::with_header(
            0,
            SUPERVISOR_OP_TASK_EXITED,
            0,
            None,
            &encode_task_exited_event(tid, code, restart_token),
        )
        .map_err(|_| KernelError::WrongObject)?;
        match self.send_message_to_endpoint_and_wake(endpoint_idx, msg) {
            Ok(()) => {
                crate::yarm_log!("TASK_EXITED_REPORT_SENT tid={} target=supervisor", tid);
                Ok(())
            }
            Err(err) => {
                crate::yarm_log!("TASK_EXITED_REPORT_FAIL tid={} reason={:?}", tid, err);
                Err(err)
            }
        }
    }

    pub fn report_transfer_revoke_to_supervisor(
        &mut self,
        owner_pid: u64,
        cap: u64,
        base: u64,
        len: u64,
    ) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.supervisor_endpoint) else {
            return Ok(());
        };
        let msg = Message::with_header(
            0,
            SUPERVISOR_OP_TRANSFER_REVOKED,
            0,
            None,
            &encode_transfer_revoked_event(owner_pid, cap, base, len),
        )
        .map_err(|_| KernelError::WrongObject)?;
        self.send_message_to_endpoint_and_wake(endpoint_idx, msg)
    }

    /// Stage 77+78: deliver a task-exit notification to PM's `pm_task_exit_endpoint`.
    ///
    /// Silent no-op when `pm_task_exit_endpoint` is `None` (not yet registered).
    /// Sends `KERNEL_OP_PM_TASK_EXITED` with a 16-byte LE `KernelPmTaskExitedPayload`.
    pub fn report_task_exit_to_pm(&mut self, tid: u64, code: u64) -> Result<(), KernelError> {
        let Some(endpoint_idx) = self.with_fault_state(|faults| faults.pm_task_exit_endpoint)
        else {
            return Ok(());
        };
        let payload = KernelPmTaskExitedPayload::new(tid, code).encode();
        let msg = Message::with_header(0, KERNEL_OP_PM_TASK_EXITED, 0, None, &payload)
            .map_err(|_| KernelError::WrongObject)?;
        self.send_message_to_endpoint_and_wake(endpoint_idx, msg)
    }

    pub fn exit_task(&mut self, tid: u64, code: u64) -> Result<u64, KernelError> {
        let token = self.with_restart_state_mut(|restart| {
            let token = restart.next_restart_token;
            restart.next_restart_token = restart.next_restart_token.checked_add(1).unwrap_or(1);
            token
        });

        let robust = self.robust_futex_state(tid);
        let detached = self.thread_detach_state(tid) == Some(ThreadDetachState::Detached);
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            // U9-EXIT1 §4 — THE self-exit writes, delegated to the one body the split route's
            // claim also runs. Canonical 199E-R1D's `async_preempted` clear moved with them; the
            // order, the log line and the four fields are byte-for-byte what this closure always
            // performed, and now neither route can drift from the other.
            //
            // The status PRECONDITION stays a difference between the two routes, deliberately:
            // `claim_self_exit_locked` admits `Running` and nothing else, because a split exit is
            // a task's first terminal edge, while this path is also reached for a task some other
            // owner has already moved. Sharing the writes does not share a precondition neither
            // route wants the other's.
            crate::kernel::boot::exit_claim::apply_self_exit_writes_locked(
                tcb,
                code,
                RestartToken(token),
            );
            Ok::<_, KernelError>(())
        })?;
        // Stage 173 (CAP-CNODE): default-off on-exit cap-revoke markers. Diagnostic
        // only — the reply-cap sweep + waiter cleanup below is UNCHANGED.
        let cap_cnode = crate::kernel::boot::cap_cnode_enabled();
        if cap_cnode {
            let count = self
                .snapshot_live_capabilities_for_task(tid)
                .map(|v| v.len())
                .unwrap_or(0);
            crate::yarm_log!("CAP_CNODE_REVOKE_ON_EXIT tid={} count={}", tid, count);
        }
        // Stage 199A2B1: capture the exiting task's AUTHORITATIVE identity while its
        // TCB is still live (this is the exiting incarnation), then clean up reply
        // records by that exact `{tid, asid}` — never by a numeric TID re-resolved
        // later, so a replacement task reusing the numeric TID is untouched.
        let exit_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(exit_identity);
        // ── Stage 200D-2A: SERVER-DEATH caller liveness, DEFERRED ────────────────────
        //
        // This is the BROAD-LOCK phase and it deliberately does very little. It captures
        // the exact exiting incarnation, reserves one bounded deferred slot, detaches the
        // exact reverse link and publishes an immutable generation-bearing work item.
        //
        // It does NOT claim PeerDeath, publish any caller result, make the caller Runnable
        // or enqueue anything — all of that moves to the post-lock drain, which is the
        // whole point of this stage. The task's status was already set to `Exited` above
        // (inside its own scoped `with_tcbs_mut`), which is what blocks any new reverse-link
        // registration targeting this incarnation, so the snapshot cannot race a fresh one.
        //
        // ORDER MATTERS: the queue slot is reserved BEFORE the link is detached. A detached
        // link with no deferred owner would strand the blocked caller forever, so if no
        // capacity exists we leave the link attached and detach nothing — the record keeps
        // an exact owner and a later exit path can still find it.
        let cpu_idx = self.current_cpu().0 as usize;
        let death_link = match crate::kernel::boot::server_death_work_reserve(cpu_idx) {
            Some(reservation) => {
                // Stage 200D-2B1A (§2): the reservation succeeded — attest it BEFORE the
                // detach, because the detach is the irreversible step and the order
                // (reserve, then detach, then publish) is exactly what prevents a stranded
                // caller. In-lock: `exit_task` runs under the broad guard, so these three
                // markers deliberately carry no `broad_lock=0`.
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_DEFERRED_RESERVED server_tid={} server_asid={} cpu={} slots=1 result=ok",
                    tid,
                    exit_identity.asid.0,
                    cpu_idx
                );
                // 199D-SD3 — arm the ServerDies scenario scope from the DEATH identity, here,
                // where `{record index, generation}` is exactly the scenario's and the link is
                // still attached to observe. It used to be armed at reply-deadline registration
                // time, which tied the whole audit to the caller having asked for a finite
                // timeout; arming it from the terminal arm instead was no better, because the
                // one-shot latch then claimed whichever reply wait happened to be FIRST in the
                // boot (record generation 1) rather than the one that dies (generation 18), and
                // the real detach was counted as a foreign close.
                #[cfg(feature = "ipc-reply-timeout-oracle-core")]
                {
                    if let Some(link) = self.server_reply_link_for(tid, exit_identity.asid) {
                        self.arm_server_dies_link_scope(
                            link.reply_record_index,
                            link.reply_record_generation,
                        );
                    }
                    // 199D-SD3 (§4) — attribute the reservation we are HOLDING to the armed
                    // scenario, by the dying server's identity. The reservation itself carries
                    // no record identity (it is taken before the link is read), so this is the
                    // first point at which it can be attributed at all, and it is attributed
                    // from a live token rather than inferred.
                    //
                    // This is what stops an unrelated earlier exit — a task that owns no
                    // reverse link and takes no part in the scenario — from incrementing the
                    // scenario's reserve class and failing its audit with `count=2 expected=1`.
                    // A REPEATED exit attempt for the armed server still counts again, so a
                    // genuine duplicate reservation stays detectable.
                    crate::kernel::boot::server_dies_counters::note_deferred_reserved(
                        tid,
                        exit_identity.asid.0,
                    );
                }
                match self.take_server_reply_link(tid, exit_identity.asid) {
                    Some(link) => {
                        // The EXACT link this server owned, detached by full incarnation
                        // (`take_server_reply_link` matches {tid, asid}), carrying the exact
                        // reply-record coordinates it was attached to.
                        crate::yarm_log!(
                            "IPC_SERVER_DEATH_LINK_CAPTURED server_tid={} server_asid={} record_index={} record_generation={} detached=1 result=ok",
                            tid,
                            exit_identity.asid.0,
                            link.reply_record_index,
                            link.reply_record_generation
                        );
                        let work = crate::kernel::boot::DeferredServerDeathCompletion {
                            exiting_server: exit_identity,
                            reply_record_index: link.reply_record_index,
                            reply_record_generation: link.reply_record_generation,
                        };
                        // A duplicate exit notification collapses to ONE owner: publish
                        // returns false and releases the slot rather than queueing twice.
                        let published =
                            crate::kernel::boot::server_death_work_publish(reservation, work);
                        crate::yarm_log!(
                            "IPC_SERVER_DEATH_DEFERRED server_tid={} server_asid={} record_index={} record_generation={} published={} result=ok",
                            tid,
                            exit_identity.asid.0,
                            link.reply_record_index,
                            link.reply_record_generation,
                            u32::from(published)
                        );
                        // One published item per exiting server. A duplicate exit collapses
                        // here: `server_death_work_publish` returns false and releases the
                        // slot rather than queueing a second owner for the same record.
                        if published {
                            crate::yarm_log!(
                                "IPC_SERVER_DEATH_DEFERRED_PUBLISHED server_tid={} server_asid={} record_index={} record_generation={} cpu={} items=1 result=ok",
                                tid,
                                exit_identity.asid.0,
                                link.reply_record_index,
                                link.reply_record_generation,
                                cpu_idx
                            );
                        } else {
                            crate::yarm_log!(
                                "IPC_SERVER_DEATH_DUPLICATE_DEFERRED server_tid={} server_asid={} record_index={} record_generation={} result=fail",
                                tid,
                                exit_identity.asid.0,
                                link.reply_record_index,
                                link.reply_record_generation
                            );
                        }
                        Some(link)
                    }
                    None => {
                        // Nothing owed: release the reservation so the slot is not held.
                        crate::kernel::boot::server_death_work_release(reservation);
                        None
                    }
                }
            }
            None => {
                // Queue full: keep the link attached (no irreversible detach) so the
                // record still has an exact owner. Never silently drop the work.
                crate::yarm_log!(
                    "IPC_SERVER_DEATH_DEFER_FULL server_tid={} link_retained=1 result=degraded",
                    tid
                );
                None
            }
        };
        // The replier revoke sweep must still exclude ONLY the exact detached record
        // generation — the deferred drain invalidates that record itself as part of its
        // terminal commit, and a reused slot (different generation) is swept normally.
        let _ = self.revoke_reply_caps_for_replier_identity_except(exit_identity, death_link);
        if cap_cnode {
            crate::yarm_log!("CAP_CNODE_REVOKE_ON_EXIT_OK tid={}", tid);
        }
        // Stage 174 (FAULT-DELIVERY): default-off cleanup markers around the IPC
        // waiter sweep for an exiting (possibly faulted) task. The sweep itself is
        // UNCHANGED — this only exposes that a faulting task's queued/waiting IPC
        // references are cleared so no dangling fault-channel reference remains.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_CLEANUP_BEGIN tid={}", tid);
        }
        self.clear_ipc_waiters_for_tid(tid);
        if fault_delivery {
            crate::yarm_log!("FAULT_DELIVERY_TASK_CLEANUP_OK tid={}", tid);
        }
        self.report_task_exit_to_supervisor(tid, code, token)?;
        self.report_task_exit_to_pm(tid, code)?;
        if let Some(robust) = robust {
            // Use futex_wake_on_exit: the addresses come from the task's own
            // robust list and are trusted user-space, but current_tid() may be
            // a different task (e.g. supervisor) when exit is externally driven.
            let stride = core::mem::size_of::<usize>();
            let mut offset = 0usize;
            while offset < robust.len {
                let addr = robust.head.saturating_add(offset.saturating_mul(stride));
                let _ = self.futex_wake_on_exit(addr);
                offset += 1;
            }
        }
        let _ = self.wake_joiners_for(tid)?;

        if self.current_tid() == Some(tid) {
            let _ = self.block_current_cpu();
            let _ = self.dispatch_next_task()?;
        }
        if detached {
            self.reap_if_detached(tid)?;
        }

        Ok(token)
    }

    pub fn restart_task(&mut self, tid: u64, token: u64) -> Result<(), KernelError> {
        let token_matches = self.with_tcbs(|tcbs| {
            tcbs.iter()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .map(|tcb| tcb.restart.token == Some(RestartToken(token)))
        });
        let token_matches = token_matches.ok_or(KernelError::TaskMissing)?;
        if !token_matches {
            return Err(KernelError::WrongObject);
        }

        // Stage 174 (FAULT-DELIVERY): default-off supervisor-restart markers. The
        // restart sequence (cap revoke → token clear → runnable → re-enqueue) is
        // UNCHANGED. The token was already validated above, so the fault channel
        // (endpoint index/generation) remains valid across the restart — the
        // restarted task rebinds to the same channel without a stale sender/reply
        // cap or orphaned waiter.
        let fault_delivery = crate::kernel::boot::fault_delivery_enabled();
        if fault_delivery {
            crate::yarm_log!(
                "FAULT_DELIVERY_SUPERVISOR_RESTART_BEGIN old_tid={} new_tid={}",
                tid,
                tid
            );
            crate::yarm_log!(
                "FAULT_DELIVERY_RESTART_TOKEN_OK tid={} token={}",
                tid,
                token
            );
        }

        let _ = self.revoke_driver_runtime_caps(tid);

        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.restart.token = None;
            tcb.status = TaskStatus::Runnable;
            Ok::<_, KernelError>(())
        })?;
        // Stage 199A2B1: restart keeps the same TCB/ASID; capture the authoritative
        // identity and clear any caller-side reply records by exact `{tid, asid}`.
        let restart_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(restart_identity);
        let result = match self.enqueue_task(tid) {
            Ok(_) | Err(KernelError::WouldBlock) => Ok(()),
            Err(err) => Err(err),
        };
        if fault_delivery && result.is_ok() {
            crate::yarm_log!("FAULT_DELIVERY_CHANNEL_REBIND_OK tid={}", tid);
            crate::yarm_log!("FAULT_DELIVERY_SUPERVISOR_RESTART_OK");
        }
        result
    }

    pub fn mark_task_dead(&mut self, tid: u64) -> Result<(), KernelError> {
        let process_pid = self
            .thread_group_id(tid)
            .map(|group| group.0)
            .ok_or(KernelError::TaskMissing)?;
        self.with_tcbs_mut(|tcbs| {
            let tcb = tcbs
                .iter_mut()
                .flatten()
                .find(|tcb| tcb.tid.0 == tid)
                .ok_or(KernelError::TaskMissing)?;
            tcb.status = TaskStatus::Dead;
            tcb.restart.token = None;
            Ok::<_, KernelError>(())
        })?;
        // Stage 199A2B1: authoritative-identity reply-record cleanup at task death.
        let dead_identity = crate::kernel::boot::ReceiverWaiterIdentity::new(
            crate::kernel::ipc::ThreadId(tid),
            self.task_asid(tid).unwrap_or(crate::kernel::vm::Asid(0)),
        );
        let _ = self.revoke_reply_caps_for_caller_identity(dead_identity);
        let _ = self.revoke_reply_caps_for_replier_identity(dead_identity);
        // Canonical 199E: a dead task can never consume a reply, so its reply deadline is over.
        // Return the bounded-store slot here — the exit path does not go through the wake seam
        // that retires it on every other terminal outcome, and a leaked Armed slot would both
        // exhaust the registry and leave a registration naming a task that no longer exists.
        self.retire_reply_deadline_for_tid(tid);
        self.clear_ipc_waiters_for_tid(tid);
        let _ = self.release_kernel_context(tid);
        let _ = self.revoke_driver_runtime_caps(tid);
        self.maybe_cleanup_process_cnode_for_pid(process_pid);
        Ok(())
    }

    /// U9-REAP1 §3 — the broad NR31 owner, now a thin caller of THE reap transaction.
    ///
    /// Every step this used to perform inline — the rank-2 mark-dead, the two reply-record
    /// sweeps, the waiter detach, the kernel-context release and the last-thread process teardown
    /// — now lives in `syscall::reap_txn::run_reap_transaction`, which the split NR31 route drives
    /// through its own owners. The order, the conditions and the no-allocation discipline are
    /// therefore stated exactly once, and the two routes cannot diverge.
    ///
    /// Two things changed shape, and both are deliberate:
    ///
    /// * The status read and the status write are now ONE rank-2 acquisition (the claim). Under
    ///   the broad lock that is indistinguishable from the split pair it replaces; off it, it is
    ///   what makes the reap linearizable against restart, exit and a duplicate reap.
    /// * A target that is ALREADY `Dead` loses the claim instead of re-running the cleanup. The
    ///   syscall's return is unchanged (`Ok`), and the skipped work was a no-op sweep of records
    ///   a previous reap had already retired — a duplicate reap now costs nothing and mutates
    ///   nothing. The process-wide teardown is not lost with it: the last thread of a group to
    ///   die always reaches the same last-thread rule through its own death path.
    pub fn reap_faulted_task_noalloc_cleanup(&mut self, tid: u64) -> Result<(), KernelError> {
        use crate::kernel::boot::reap_claim::ReapRefusal;
        use crate::kernel::syscall::reap_txn::{BroadReapOwners, run_reap_transaction};

        crate::yarm_log!("TASK_REAP_FAULTED_NOALLOC_CLEANUP_BEGIN target_tid={}", tid);
        let mut owners = BroadReapOwners { kernel: self };
        match run_reap_transaction(&mut owners, tid) {
            Ok(outcome) => {
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=mark_dead_clear_restart",
                    tid
                );
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=reply_caps caller={} replier={} links={}",
                    tid,
                    outcome.caller_reply_records_revoked,
                    outcome.replier_reply_records_revoked,
                    outcome.reverse_links_detached
                );
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=ipc_waiters settled={}",
                    tid,
                    outcome.orphaned_senders_settled
                );
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=kernel_context",
                    tid
                );
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_CLEANUP_STEP target_tid={} step=process_cnode_noalloc reaped={} asids={} mappings={}",
                    tid,
                    u8::from(outcome.process_reaped),
                    outcome.address_spaces_destroyed,
                    outcome.transfer_mappings_unmapped
                );
                crate::yarm_log!("TASK_REAP_FAULTED_NOALLOC_CLEANUP_OK target_tid={}", tid);
                Ok(())
            }
            // Already reaped, or never there: the base disposition for a target that is gone.
            Err(refusal) if refusal.is_already_reaped() => {
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_NOALLOC_CLEANUP_SKIPPED target_tid={} reason={}",
                    tid,
                    refusal.marker()
                );
                match refusal {
                    ReapRefusal::TaskGone => Err(KernelError::TaskMissing),
                    _ => Ok(()),
                }
            }
            Err(refusal) => {
                crate::yarm_log!(
                    "TASK_REAP_FAULTED_NOALLOC_CLEANUP_REFUSED target_tid={} reason={} task_mutation=none",
                    tid,
                    refusal.marker()
                );
                Err(KernelError::WrongObject)
            }
        }
    }
}
