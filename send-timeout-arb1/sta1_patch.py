#!/usr/bin/env python3
"""SEND-TIMEOUT-ARB1 production patch (applied once to a 414afb86 tree). Usage: sta1_patch.py <wt> <scratch>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); sc = pathlib.Path(sys.argv[2])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:90], s.count(old))
    p.write_text(s.replace(old, new))
def replace_between(rel, start, end, new, after=None):
    p = wt / rel; s = p.read_text()
    base = s.index(after) if after else 0
    a = s.index(start, base); b = s.index(end, a) + len(end)
    p.write_text(s[:a] + new + s[b:])

SW = "src/kernel/boot/sender_waiter.rs"
I = "src/kernel/boot/ipc_state.rs"
R = "src/runtime.rs"
X = "src/kernel/syscall/ipc.rs"

# ── 1. the owner module: the arbitration contract, the timeout ending, the hooks ─────────────
patch(SW, '''//! An ASID-less sender is a cycle with `asid: None`. It is claimed only by a TCB whose ASID is
//! also `None`, and it is woken without a completion (there is no incarnation to bind one to).
''', '''//! An ASID-less sender is a cycle with `asid: None`. It is claimed only by a TCB whose ASID is
//! also `None`, and it is woken without a completion (there is no incarnation to bind one to).
//!
//! # SEND-TIMEOUT-ARB1 — the waiter decides
//!
//! A cycle's message travels with its waiter, so the waiter is the cycle's arbitration token. The
//! one irrevocable step is the rank-3 removal of the exact waiter, and whoever performs it owns the
//! cycle — its ending AND its message's obligations:
//!
//! * a receiver's take (consume or refill) owns delivery, then claims `Consumed`;
//! * the timeout withdraws the waiter first ([`remove_sender_waiter_for_cycle_locked`]), then
//!   claims `TimedOut` and settles the transfer the message still carried;
//! * an endpoint's destruction removes its waiters, so it ends each of their cycles;
//! * a blocker whose waiter could not be published owns its cycle outright (no waiter exists).
//!
//! The rank-2 claim that follows a removal therefore has no competitor: no other actor holds that
//! cycle's waiter, and nothing ends a cycle without one. A timeout that finds no waiter — not yet
//! published, already taken by a receiver, or the cycle already over — changes nothing and leaves
//! the deadline armed, so it can neither report a delivered message as timed out nor end a cycle
//! whose waiter is still to appear. A task's death ends its cycle without a claim.
''')
patch(SW, '''    /// The armed deadline `deadline` expired; `now` is the clock it was compared against. The
    /// claim also requires the deadline to still be the armed one and still due.
    TimedOut { deadline: u64, now: u64 },''', '''    /// The timeout withdrew the cycle's waiter (SEND-TIMEOUT-ARB1): ownership comes from that
    /// removal, and the drain checked the deadline was due before making it.
    TimedOut,''')
patch(SW, '''        SendCycleEnding::TimedOut { deadline, now } => {
            if tcb.ipc_timeout_deadline != Some(deadline) || now < deadline {
                return false;
            }
            (KernelState::SEND_COMPLETION_TIMED_OUT, true)
        }''', '''        SendCycleEnding::TimedOut => (KernelState::SEND_COMPLETION_TIMED_OUT, true),''')
p = wt / SW; p.write_text(p.read_text() + (sc / "sta1_hookmod.rs").read_text())

# ── 2. the send-timeout drain: withdraw the exact waiter, THEN claim ─────────────────────────
replace_between(R,
    "            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────\n",
    "            // ── (4) rank 1: the wake, last ─────────────────────────────────────────────────\n            self.enqueue_reply_timeout_wake_split(work.tid);\n",
    '''            let target = crate::kernel::ipc::SenderWakeTarget {
                tid: crate::kernel::ipc::ThreadId(work.tid),
                asid: work.asid,
                send_generation: work.send_generation,
            };
            if now < work.deadline {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_NOT_DUE tid={} send_generation={} deadline={} now={}",
                    work.tid,
                    work.send_generation,
                    work.deadline,
                    now
                );
                continue;
            }
            // ── (0) rank 2, read-only: is this cycle still the sender's? ─────────────────────
            // A cycle the sender already left (completed, re-blocked under a newer generation,
            // replaced, dead) is refused here with zero mutation, so stale work never touches
            // any queue.
            let current = self.with_task_tcbs_split_mut(|tcbs| {
                tcbs.iter().flatten().any(|t| {
                    t.tid == target.tid
                        && t.asid == target.asid
                        && t.blocked_send_generation == target.send_generation
                        && matches!(
                            t.status,
                            crate::kernel::task::TaskStatus::Blocked(
                                crate::kernel::task::WaitReason::EndpointSend(_)
                            )
                        )
                })
            });
            if !current {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_STALE tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            }
            // ── (1) rank 3: withdraw exactly this cycle's waiter — THE arbitration point ─────
            // SEND-TIMEOUT-ARB1: the waiter carries the message, so removing it is what decides
            // the cycle. No waiter means a receiver already took the message, the waiter is not
            // published yet, or the cycle is over: nothing is mutated and the deadline stays
            // armed, so this can never report a delivered message as timed out.
            let withdrawn = self.with_ipc_split_mut(|ipc| {
                let (removed, endpoint_idx) =
                    crate::kernel::boot::sender_waiter::remove_sender_waiter_for_cycle_locked(
                        ipc, target,
                    )?;
                let owed = removed.msg.transferred_cap().and_then(|handle| {
                    let idx = usize::try_from(handle.0 & 0xFFFF).ok()?;
                    // Resolve against the envelope's OWN recorded endpoint, never a live
                    // `ipc.endpoints` entry — the endpoint may legitimately be gone.
                    let env = ipc.transfer_envelopes.get(idx).copied().flatten()?;
                    if ipc.transfer_envelope_generations.get(idx).copied() != Some(handle.0 >> 16) {
                        return None;
                    }
                    if !matches!(env.endpoint,
                        CapObject::Endpoint { index, .. } if index == endpoint_idx)
                    {
                        return None;
                    }
                    Some((
                        handle.0,
                        endpoint_idx,
                        env.receiver_tid.unwrap_or(removed.tid),
                    ))
                });
                Some(owed)
            });
            let Some(envelope) = withdrawn else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_NO_WAITER tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            };
            #[cfg(test)]
            crate::kernel::boot::sender_waiter::send_arb_phase_hook::run_between_drain_phases();
            // ── (2) rank 2: the claim — uncontested, since this drain holds the waiter ──────
            // It fails only if the sender is no longer blocked in this cycle (it died), so a
            // failure can strand nobody; the transfer is settled below either way.
            let claimed = self.with_task_tcbs_split_mut(|tcbs| {
                crate::kernel::boot::sender_waiter::claim_blocked_send_cycle_locked(
                    tcbs,
                    target,
                    crate::kernel::boot::sender_waiter::SendCycleEnding::TimedOut,
                )
            });
            if claimed {
                crate::yarm_log!(
                    "U6_SEND_COMPLETION_PUBLISHED tid={} asid={} send_generation={} result={} result=ok",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    KernelState::SEND_COMPLETION_TIMED_OUT
                );
            } else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED tid={} send_generation={}",
                    work.tid,
                    work.send_generation
                );
            }
            // ── (3) rank 3 released → the canonical settle (rank 3, then rank 6) ────────────
            // The withdrawn message was never delivered, so its transfer is this drain's to
            // cancel — exactly once, before any wake.
            if let Some((handle, endpoint_idx, cleanup_tid)) = envelope {
                let taken =
                    self.settle_blocked_send_envelope_split(handle, endpoint_idx, cleanup_tid);
                crate::yarm_log!(
                    "U6_BLOCKED_SEND_ENVELOPE_SETTLED tid={} endpoint={} handle={} result={}",
                    work.tid,
                    endpoint_idx,
                    handle,
                    if taken { "ok" } else { "already_settled" }
                );
            }
            if !claimed {
                continue;
            }
            // ── (4) rank 1: the wake, last ─────────────────────────────────────────────────
            self.enqueue_reply_timeout_wake_split(work.tid);
''', after="    pub(crate) fn drain_send_timeout_post_work(")
patch(R, '''    /// 1. **rank 2 — the exact claim.** The item is settled only if `{tid, asid,
    ///    send_generation}` still names a live incarnation `Blocked(EndpointSend)` whose armed
    ///    deadline is still the collected one and is still due.''', '''    /// SEND-TIMEOUT-ARB1 reversed the first two steps: the drain first WITHDRAWS the cycle's
    /// exact waiter at rank 3 (the arbitration point — no waiter, no action, deadline left armed)
    /// and only then claims at rank 2, uncontested. The description below is the U7 original.
    ///
    /// 1. **rank 2 — the exact claim.** The item is settled only if `{tid, asid,
    ///    send_generation}` still names a live incarnation `Blocked(EndpointSend)` whose armed
    ///    deadline is still the collected one and is still due.''')

# ── 3. the broad block: a refused publication ends its own cycle ─────────────────────────────
patch(I, '''        #[cfg(test)]
        super::sender_waiter::send_arb_phase_hook::run_between_block_and_publish(self);
''', '', count=0)
patch(I, '''            crate::yarm_log!("D2_SEND_GENUINE_PHASE_TASK_BLOCK tid={}", blocked_tid);
        }
        // Phase C (ipc rank 3): publish the sender-waiter (its message rides with
        // it, so the receiver-side wake/handoff and the Stage 163P sender-wake
        // oracle are unchanged).
        self.enqueue_sender_waiter(
            endpoint_idx,
            SenderWaiter {
                tid: ThreadId(blocked_tid),
                msg,
                asid: blocked_asid,
                send_generation,
            },
        )?;''', '''            crate::yarm_log!("D2_SEND_GENUINE_PHASE_TASK_BLOCK tid={}", blocked_tid);
        }
        #[cfg(test)]
        super::sender_waiter::send_arb_phase_hook::run_between_block_and_publish(self);
        // Phase C (ipc rank 3): publish the sender-waiter (its message rides with
        // it, so the receiver-side wake/handoff and the Stage 163P sender-wake
        // oracle are unchanged).
        //
        // SEND-TIMEOUT-ARB1: between Phase B and here no actor can end this cycle — every ending
        // needs its waiter — so the waiter is always published for a live cycle. If it cannot be
        // published, no waiter will ever exist and nothing else can end the cycle: this blocker
        // owns it, ends it with the refusal's own error, and puts the sender back to run.
        if let Err(err) = self.enqueue_sender_waiter(
            endpoint_idx,
            SenderWaiter {
                tid: ThreadId(blocked_tid),
                msg,
                asid: blocked_asid,
                send_generation,
            },
        ) {
            let target = crate::kernel::ipc::SenderWakeTarget {
                tid: ThreadId(blocked_tid),
                asid: blocked_asid,
                send_generation,
            };
            let result = crate::kernel::syscall::SyscallError::from(err).code() as u64;
            crate::yarm_log!(
                "SEND_ARB_PUBLICATION_REFUSED_UNWIND tid={} send_generation={} err={:?}",
                blocked_tid,
                send_generation,
                err
            );
            self.complete_and_wake_blocked_sender(target, result)?;
            let _ = self.dispatch_next_task()?;
            return Err(err);
        }''')

# ── 4. the in-lock route reports that it parked a waiter ─────────────────────────────────────
patch(I, '''    /// A blocking-send commit is STASHED and not yet performed. The caller must leave the
    /// frame alone and let the post-lock drain decide; the drain's typed disposition is what
    /// finally determines the syscall's result.
    PublicationPending,
}''', '''    /// A blocking-send commit is STASHED and not yet performed. The caller must leave the
    /// frame alone and let the post-lock drain decide; the drain's typed disposition is what
    /// finally determines the syscall's result.
    PublicationPending,
    /// SEND-TIMEOUT-ARB1: the in-lock route PARKED the sender — its waiter is published and owns
    /// the message and any transfer envelope. The caller reports `WouldBlock` but must not
    /// release the envelope: the cycle's winner (a receiver or a timeout) owns it.
    BlockedInLock,
}''')
patch(I, '''        let _ = self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
        });
        Err(KernelError::WouldBlock)''', '''        let _ = self.block_current_on_send_with_deadline(endpoint_idx, send_cap, msg, deadline)?;
        self.with_ipc_state_mut(|ipc| {
            ipc.telemetry.blocked_sends = ipc.telemetry.blocked_sends.saturating_add(1);
        });
        Ok(BlockingSendProducerOutcome::BlockedInLock)''')
patch(I, '''            BlockingSendProducerOutcome::CompletedImmediately => Ok(()),
            BlockingSendProducerOutcome::PublicationPending => {''', '''            BlockingSendProducerOutcome::CompletedImmediately => Ok(()),
            BlockingSendProducerOutcome::BlockedInLock => Err(KernelError::WouldBlock),
            BlockingSendProducerOutcome::PublicationPending => {''')
patch(X, '''    if let Err(err) = send_result {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0) {''', '''    // SEND-TIMEOUT-ARB1: a parked sender's waiter owns its transfer envelope; only a send that
    // parked nothing gives the envelope back here.
    let (send_result, parked) = match send_result {
        Ok(crate::kernel::boot::BlockingSendProducerOutcome::BlockedInLock) => {
            (Err(KernelError::WouldBlock), true)
        }
        other => (other, false),
    };
    if let Err(err) = send_result {
        if let Some(handle) = msg.transferred_cap().map(|c| c.0).filter(|_| !parked) {''')

# ── 5. endpoint destruction ends the cycles of the waiters it removes ────────────────────────
patch(I, '''            for (i, slot) in ipc.endpoint_sender_waiters[endpoint_idx]
                .iter_mut()
                .enumerate()
            {
                if let Some(removed) = slot.take()
                    && removed.msg.transferred_cap().is_some()
                    && i < MAX_ENDPOINT_SENDER_WAITERS
                {
                    orphaned_senders[i] = Some(removed);
                }
            }''', '''            // SEND-TIMEOUT-ARB1: EVERY removed waiter is kept — removing it makes this
            // destruction the owner of its cycle, which it ends below.
            for (i, slot) in ipc.endpoint_sender_waiters[endpoint_idx]
                .iter_mut()
                .enumerate()
            {
                if let Some(removed) = slot.take()
                    && i < MAX_ENDPOINT_SENDER_WAITERS
                {
                    orphaned_senders[i] = Some(removed);
                }
            }''')
patch(I, '''        for removed in orphaned_senders.iter().flatten() {
            let _ = self.settle_blocked_sender_envelope(removed, endpoint_idx);
        }''', '''        for removed in orphaned_senders.iter().flatten() {
            let _ = self.settle_blocked_sender_envelope(removed, endpoint_idx);
        }
        // SEND-TIMEOUT-ARB1: the waiters are gone, so no receiver or timeout can end their cycles;
        // this destruction does, after the settle, with the error a send to a destroyed endpoint
        // reports. A stale record (the cycle already over) is refused by the claim.
        let gone = crate::kernel::syscall::SyscallError::from(KernelError::WrongObject).code() as u64;
        for removed in orphaned_senders.iter().flatten() {
            let _ = self.complete_and_wake_blocked_sender(removed.wake_target(), gone);
        }''')
print("sta1 patch applied")
