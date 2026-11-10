#!/usr/bin/env python3
"""IPC-SENDER-ID1 production patch (applied once to a 16e5c04 tree). Usage: isi1_patch.py <wt>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:90], s.count(old))
    p.write_text(s.replace(old, new))
def replace_between(rel, start, end, new, after=None):
    p = wt / rel; s = p.read_text()
    base = s.index(after) if after else 0
    if after:
        assert s.count(after) == 1, (rel, after)
    else:
        assert s.count(start) == 1, (rel, start)
    a = s.index(start, base); b = s.index(end, a) + len(end)
    p.write_text(s[:a] + new + s[b:])

I = "src/kernel/boot/ipc_state.rs"
R = "src/runtime.rs"

patch("src/kernel/boot/mod.rs", "pub(crate) mod reap_claim;\n", "pub(crate) mod reap_claim;\npub(crate) mod sender_waiter;\n")

LOG_TAIL = '''        let asid_log = target.asid.map_or(0, |a| a.0);
        if published {
            crate::yarm_log!(
                "U6_SEND_COMPLETION_PUBLISHED tid={} asid={} send_generation={} result={} result=ok",
                target.tid.0,
                asid_log,
                target.send_generation,
                result
            );
        } else {
            crate::yarm_log!(
                "U6_SEND_COMPLETION_REFUSED_STALE tid={} asid={} send_generation={}",
                target.tid.0,
                asid_log,
                target.send_generation
            );
        }
        published
    }
'''

# ── 1. broad publication: THE rank-2 claim ─────────────────────────────────────────────────────
replace_between(I,
    "        let Some((tid, asid, send_generation)) = target.exact() else {\n            // An ASID-less sender has no incarnation to bind a completion to. It is woken\n            // exactly as it was before U6 — this is the pre-U6 behaviour, preserved.\n            return false;\n        };\n        let published = self.with_tcbs_mut(|tcbs| {",
    "        published\n    }\n",
    '''        // IPC-SENDER-ID1: THE claim of this exact cycle (`sender_waiter`), the same one the
        // timeout drain makes. Claiming also makes the sender `Runnable`, so a timeout that runs
        // after this can no longer claim the cycle and overwrite the completion.
        let published = self.with_tcbs_mut(|tcbs| {
            super::sender_waiter::claim_blocked_send_cycle_locked(
                tcbs,
                target,
                super::sender_waiter::SendCycleEnding::Consumed { result },
            )
        });
''' + LOG_TAIL)

patch(I, '''    /// The publication is refused, with no mutation at all, unless the waiter's recorded
    /// `{tid, asid, send_generation}` still names a live incarnation blocked in
    /// `EndpointSend`. A replacement task that reused the numeric TID, a sender that has
    /// already been completed by another site, or one that has since been woken and re-blocked
    /// under a newer generation are all different cycles, and none of them is this waiter's.
    ///
    /// Returns `true` iff a completion was published.''', '''    /// The publication is refused, with no mutation at all, unless the waiter's recorded
    /// `{tid, asid, send_generation}` still names a live incarnation blocked in
    /// `EndpointSend`. A replacement task that reused the numeric TID, a sender that has
    /// already been completed by another site, or one that has since been woken and re-blocked
    /// under a newer generation are all different cycles, and none of them is this waiter's.
    ///
    /// IPC-SENDER-ID1: the publication IS the claim of the cycle
    /// (`sender_waiter::claim_blocked_send_cycle_locked`): on success the sender is also made
    /// `Runnable` and its deadline disarmed, in the same acquisition. An ASID-less sender is
    /// claimed without a completion.
    ///
    /// Returns `true` iff this caller claimed the cycle. Only then may it wake the sender.''')

# ── 2. the two in-lock wake seams wake only a claimed cycle ────────────────────────────────────
patch(I, '''        self.publish_blocking_send_completion(target, result);
        self.wake_tid_to_runnable(target.tid)
    }''', '''        // IPC-SENDER-ID1: wake only the cycle this call claimed. A refused claim means the
        // cycle already ended (or was replaced by a newer one); waking the bare TID then would
        // wake whatever the task is doing now.
        if self.publish_blocking_send_completion(target, result) {
            self.wake_tid_to_runnable(target.tid)
        } else {
            Ok(())
        }
    }''')
patch(I, '''        self.publish_blocking_send_completion(target, Self::SEND_COMPLETION_OK);
        self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(target.tid))
    }''', '''        // IPC-SENDER-ID1: wake only the cycle this call claimed.
        if self.publish_blocking_send_completion(target, Self::SEND_COMPLETION_OK) {
            self.apply_scheduler_wake_plan(super::SchedulerWakePlan::Wake(target.tid))
        } else {
            Ok(())
        }
    }''')

# ── 3. task-scoped removal: one owner for broad exit/death and the reap/exit claim body ───────
patch(I, '''            ipc.clear_endpoint_waiters_for_identity(identity);
            for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
                for slot in queue.iter_mut() {
                    if slot.as_ref().is_some_and(|w| w.tid == tid_id) {
                        if let Some(removed) = slot.take()
                            && removed.msg.transferred_cap().is_some()
                            && orphaned_n < MAX_ENDPOINT_SENDER_WAITERS
                        {
                            orphaned[orphaned_n] = Some((removed, endpoint_idx));
                            orphaned_n += 1;
                        }
                        *slot = None;
                    }
                }
            }
            // NOTIF-WAITER1: the notification waiter too, by complete identity.''', '''            ipc.clear_endpoint_waiters_for_identity(identity);
            // IPC-SENDER-ID1: the task-scoped removal owner (numeric TID by proof — see
            // `sender_waiter`), compacting each queue it touches.
            orphaned_n =
                super::sender_waiter::remove_sender_waiters_for_task_locked(ipc, tid_id, &mut orphaned);
            // NOTIF-WAITER1: the notification waiter too, by complete identity.''')
patch(I, '''        // sender waiter structures keep their numeric-TID sweep (not receive-waiters).''', '''        // sender waiters are removed by numeric TID through `sender_waiter`, which states why that
        // is the task-incarnation predicate (a TID names one TCB for the life of the system).''')
patch("src/kernel/boot/reap_claim.rs", '''    ipc.clear_endpoint_waiters_for_identity(identity);
    let mut orphaned_n = 0usize;
    for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {
        for slot in queue.iter_mut() {
            if slot.as_ref().is_some_and(|w| w.tid == identity.tid) {
                if let Some(removed) = slot.take()
                    && removed.msg.transferred_cap().is_some()
                    && orphaned_n < orphaned.len()
                {
                    orphaned[orphaned_n] = Some((removed, endpoint_idx));
                    orphaned_n += 1;
                }
                *slot = None;
            }
        }
    }
    // NOTIF-WAITER1''', '''    ipc.clear_endpoint_waiters_for_identity(identity);
    // IPC-SENDER-ID1: the task-scoped removal owner. The claim proved this TCB is the target; a
    // TID names one TCB for the life of the system, so its numeric TID is the incarnation here.
    let orphaned_n =
        super::sender_waiter::remove_sender_waiters_for_task_locked(ipc, identity.tid, orphaned);
    // NOTIF-WAITER1''')
patch("src/kernel/boot/reap_claim.rs", '''/// Detach every waiter the target owns: its endpoint RECEIVE waiter by exact identity, its
/// endpoint SEND waiters by numeric TID, and its notification waiters.''', '''/// Detach every waiter the target owns: its endpoint RECEIVE waiter by exact identity, its
/// endpoint SEND waiters through `sender_waiter::remove_sender_waiters_for_task_locked` (numeric
/// TID, which names exactly this TCB), and its notification waiters.''')

# ── 4. split publication: the same claim ───────────────────────────────────────────────────────
replace_between(R,
    "        use crate::kernel::task::{TaskStatus, WaitReason};\n        let Some((tid, asid, send_generation)) = target.exact() else {\n            return false;\n        };\n        let published = self.with_task_tcbs_split_mut(|tcbs| {",
    "        published\n    }\n",
    '''        // IPC-SENDER-ID1: THE claim of this exact cycle, the in-lock body's own.
        let published = self.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::boot::sender_waiter::claim_blocked_send_cycle_locked(
                tcbs,
                target,
                crate::kernel::boot::sender_waiter::SendCycleEnding::Consumed { result },
            )
        });
''' + LOG_TAIL)
patch(R, '''    /// Byte-for-byte the same decision as the in-lock body: refused with NO mutation unless the
    /// target's `{tid, asid, send_generation}` still names a live incarnation blocked in
    /// `EndpointSend`. One acquisition, no other domain touched.''', '''    /// Byte-for-byte the same decision as the in-lock body: refused with NO mutation unless the
    /// target's `{tid, asid, send_generation}` still names a live incarnation blocked in
    /// `EndpointSend`. One acquisition, no other domain touched. IPC-SENDER-ID1: it is the claim
    /// of the cycle, so it also makes the sender `Runnable`; `true` is the licence to wake.''')

# ── 5. the split wake seam wakes only a claimed cycle ─────────────────────────────────────────
patch(R, '''        self.publish_blocking_send_completion_split(
            target,
            crate::kernel::boot::KernelState::SEND_COMPLETION_OK,
        );
        // (3) the shared wake body — `apply_scheduler_wake_plan(Wake(tid))`.
        self.wake_tid_to_runnable_split(cpu, target.tid)''', '''        // IPC-SENDER-ID1: the publication is the claim; only a claimed cycle is woken.
        if !self.publish_blocking_send_completion_split(
            target,
            crate::kernel::boot::KernelState::SEND_COMPLETION_OK,
        ) {
            return Ok(());
        }
        // (3) the shared wake body — `apply_scheduler_wake_plan(Wake(tid))`.
        self.wake_tid_to_runnable_split(cpu, target.tid)''')

# ── 6. the send-timeout drain: the same claim, then the exact-cycle removal ───────────────────
replace_between(R,
    "            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────\n            let claimed = self.with_task_tcbs_split_mut(|tcbs| {\n                let Some(tcb) = tcbs\n                    .iter_mut()\n                    .flatten()\n                    .find(|t| t.tid.0 == work.tid && t.asid == work.asid)",
    "                tcb.ipc_timeout_fired = true;\n                true\n            });\n",
    '''            // ── (1) rank 2: the exact claim + the TimedOut publication ──────────────────────
            // IPC-SENDER-ID1: THE claim of this exact cycle — the receiver-side publication
            // makes the same one, so exactly one of them ends the cycle. An ASID-less sender is
            // claimed without a completion, as before.
            let target = crate::kernel::ipc::SenderWakeTarget {
                tid: crate::kernel::ipc::ThreadId(work.tid),
                asid: work.asid,
                send_generation: work.send_generation,
            };
            let claimed = self.with_task_tcbs_split_mut(|tcbs| {
                crate::kernel::boot::sender_waiter::claim_blocked_send_cycle_locked(
                    tcbs,
                    target,
                    crate::kernel::boot::sender_waiter::SendCycleEnding::TimedOut {
                        deadline: work.deadline,
                        now,
                    },
                )
            });
''', after="    pub(crate) fn drain_send_timeout_post_work(")
replace_between(R,
    "                let mut owed: Option<(u64, usize, crate::kernel::ipc::ThreadId)> = None;\n                for (endpoint_idx, queue) in ipc.endpoint_sender_waiters.iter_mut().enumerate() {",
    "                owed\n            });\n",
    '''                // IPC-SENDER-ID1: remove exactly this cycle's waiter (compacting its queue).
                let (removed, endpoint_idx) =
                    crate::kernel::boot::sender_waiter::remove_sender_waiter_for_cycle_locked(
                        ipc, target,
                    )?;
                let handle = removed.msg.transferred_cap()?;
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
''')
patch(R, '''        use crate::kernel::boot::{KernelState, MAX_ENDPOINT_SENDER_WAITERS, SenderWaiter};
        use crate::kernel::capabilities::CapObject;
        use crate::kernel::task::{TaskStatus, WaitReason};
        let cpu_idx = cpu.0 as usize;
        while let Some(work) = crate::kernel::boot::send_timeout_work_drain_next(cpu_idx) {''', '''        use crate::kernel::boot::KernelState;
        use crate::kernel::capabilities::CapObject;
        let cpu_idx = cpu.0 as usize;
        while let Some(work) = crate::kernel::boot::send_timeout_work_drain_next(cpu_idx) {''')
print("isi1 patch applied")
