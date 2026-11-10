#!/usr/bin/env python3
"""SEND-TIMEOUT-ARB1 negative controls. Usage: sta1_nc.py <worktree> <ncN>
Each control reintroduces ONE arbitration or ownership defect into the candidate."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
SW = "src/kernel/boot/sender_waiter.rs"
I = "src/kernel/boot/ipc_state.rs"
R = "src/runtime.rs"
X = "src/kernel/syscall/ipc.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
PRECHECK = '''            let current = self.with_task_tcbs_split_mut(|tcbs| {
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
            });'''
C = {
 # NC1 timeout-first rank-2 claiming: the drain claims TimedOut BEFORE it withdraws the waiter
 "nc1": [(R, PRECHECK, '''            let current = self.with_task_tcbs_split_mut(|tcbs| {
                crate::kernel::boot::sender_waiter::claim_blocked_send_cycle_locked(
                    tcbs,
                    target,
                    crate::kernel::boot::sender_waiter::SendCycleEnding::TimedOut,
                )
            });''')],
 # NC2 the Phase B->C protection removed: a missing waiter is treated as timeout-eligible
 "nc2": [(R, '''            let Some(envelope) = withdrawn else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_REFUSED_NO_WAITER tid={} asid={} send_generation={} deadline={}",
                    work.tid,
                    work.asid.map(|a| a.0).unwrap_or(0),
                    work.send_generation,
                    work.deadline
                );
                continue;
            };''', '''            let envelope = withdrawn.unwrap_or(None);''')],
 # NC3 receiver delivery after timeout won: the timeout's withdrawal leaves the waiter queued
 "nc3": [(SW, '''        let removed = queue[slot].take().expect("position guarantees Some");''',
              '''        let removed = queue[slot].expect("position guarantees Some");''')],
 # NC4 timeout after delivery won: the receiver's take leaves the waiter queued
 "nc4": [(I, '''        queue[idx] = None;
        // Full left-compaction: the taken entry''', '''        // Full left-compaction: the taken entry''')],
 # NC5 the newer-generation check disabled in the drain's precheck and withdrawal
 "nc5": [(R, '''                        && t.blocked_send_generation == target.send_generation
''', ''), (SW, '''.position(|s| s.is_some_and(|w| w.wake_target() == target))''',
              '''.position(|s| s.is_some_and(|w| w.tid == target.tid && w.asid == target.asid))''')],
 # NC6 one transfer-envelope settlement removed (the timeout's cancellation of its withdrawal)
 "nc6": [(R, '''            if let Some((handle, endpoint_idx, cleanup_tid)) = envelope {
                let taken =''', '''            if let Some((handle, endpoint_idx, cleanup_tid)) = envelope.filter(|_| false) {
                let taken =''')],
 # NC7 a duplicated sender wake on the receiver side
 "nc7": [(I, '''        if self.publish_blocking_send_completion(target, result) {
            self.wake_tid_to_runnable(target.tid)
        } else {''', '''        if self.publish_blocking_send_completion(target, result) {
            let _ = self.wake_tid_to_runnable(target.tid);
            self.wake_tid_to_runnable(target.tid)
        } else {''')],
 # NC8 a waiter hole after the timeout's removal (no compaction)
 "nc8": [(SW, '''        let removed = queue[slot].take().expect("position guarantees Some");
        compact_sender_queue(queue);''', '''        let removed = queue[slot].take().expect("position guarantees Some");''')],
 # NC9 a forced claim failure after the withdrawal (on a live sender)
 "nc9": [(R, '''                    crate::kernel::boot::sender_waiter::SendCycleEnding::TimedOut,
                )
            });
            if claimed {''', '''                    crate::kernel::boot::sender_waiter::SendCycleEnding::TimedOut,
                )
            }) && false;
            if claimed {''')],
 # NC10 a refused publication no longer ends its own cycle (the blocked sender is stranded)
 "nc10": [(I, '''            self.complete_and_wake_blocked_sender(target, result)?;
            let _ = self.dispatch_next_task()?;
            return Err(err);''', '''            let _ = (target, result);
            return Err(err);''')],
 # NC11 endpoint destruction no longer ends the cycles it removes
 "nc11": [(I, '''            let _ = self.complete_and_wake_blocked_sender(removed.wake_target(), gone);''',
               '''            let _ = (removed, gone);''')],
 # NC12 the IpcSend syscall releases the envelope of a parked send again
 "nc12": [(X, '''        if let Some(handle) = msg.transferred_cap().map(|c| c.0).filter(|_| !parked) {''',
               '''        if let Some(handle) = msg.transferred_cap().map(|c| c.0).filter(|_| parked || !parked) {''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
