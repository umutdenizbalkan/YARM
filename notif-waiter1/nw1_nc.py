#!/usr/bin/env python3
"""NOTIF-WAITER1 negative controls. Usage: nw1_nc.py <worktree> <ncN>
Each control removes ONE protection from the candidate; the hosted suites must fail."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
NW = "src/kernel/boot/notification_waiter.rs"
IPC = "src/kernel/boot/ipc_state.rs"
RC = "src/kernel/boot/reap_claim.rs"
RT = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
C = {
 # NC1 publication ignores the incarnation's generation (N3)
 "nc1": [(NW, "    if i >= MAX_NOTIFICATIONS\n        || ipc.notification_generations[i] != incarnation.generation\n        || ipc.notifications[i].is_none()\n    {\n        return NotificationPublishOutcome::IncarnationGone;",
               "    if i >= MAX_NOTIFICATIONS || ipc.notifications[i].is_none() {\n        return NotificationPublishOutcome::IncarnationGone;")],
 # NC2 publication does not re-check the queue (N2)
 "nc2": [(NW, "    if let Some(msg) = ipc.notifications[i].as_mut().and_then(|n| n.recv()) {\n        return NotificationPublishOutcome::EventTaken(msg);\n    }\n", "")],
 # NC3 publication overwrites any existing record (N1)
 "nc3": [(NW, "    if let Some(current) = ipc.notification_waiters[i]\n        && Some(current) != stale\n", "    if let Some(current) = ipc.notification_waiters[i]\n        && false\n        && Some(current) != stale\n")],
 # NC4 no post-publication retract (timeout in the window leaves a stale record)
 "nc4": [(IPC, "                if !self.notification_record_is_parked(record) {", "                if false && !self.notification_record_is_parked(record) {")],
 # NC5 the reap/exit claim body clears notification waiters by bare TID
 "nc5": [(RC, "    ipc.clear_notification_waiters_for_identity(identity);\n    orphaned_n",
              "    for waiter in ipc.notification_waiters.iter_mut() {\n        if waiter.is_some_and(|w| w.receiver.tid == identity.tid) {\n            *waiter = None;\n        }\n    }\n    orphaned_n")],
 # NC6 the broad task teardown clears notification waiters by bare TID
 "nc6": [(IPC, "            // NOTIF-WAITER1: the notification waiter too, by complete identity.\n            ipc.clear_notification_waiters_for_identity(identity);",
               "            for waiter in ipc.notification_waiters.iter_mut() {\n                if waiter.is_some_and(|w| w.receiver.tid == tid_id) {\n                    *waiter = None;\n                }\n            }")],
 # NC7 the immediate take ignores the generation
 "nc7": [(NW, "    if ipc.notification_generations[i] != incarnation.generation {\n        return Err(KernelError::StaleCapability);\n    }\n", "")],
 # NC8 a stale competing record is never reclaimed
 "nc8": [(IPC, "        if let P::ReceiverAlreadyWaiting(current) = outcome\n            && !self.notification_record_is_parked(current)",
               "        if let P::ReceiverAlreadyWaiting(current) = outcome\n            && false\n            && !self.notification_record_is_parked(current)")],
 # NC9 the split receive-timeout settle clears notification waiters by bare TID
 "nc9": [(RT, "                ipc.clear_notification_waiters_for_identity(identity);\n                ipc.any_endpoint_waiter_is(identity) || ipc.any_notification_waiter_is(identity)",
              "                for w in ipc.notification_waiters.iter_mut() {\n                    if w.is_some_and(|w| w.receiver.tid == identity.tid) {\n                        *w = None;\n                    }\n                }\n                ipc.any_endpoint_waiter_is(identity) || ipc.any_notification_waiter_is(identity)")],
 # NC10 a refused receiver is not restored (no unwind)
 "nc10": [(IPC, "        self.wake_tid_to_runnable(ThreadId(blocked_tid))?;\n        let _ = self.dispatch_next_task()?;\n        answer\n",
                "        let _ = self.dispatch_next_task()?;\n        answer\n")],
 # NC11 same-receiver supersession admits an equal or older wait generation
 "nc11": [(NW, "current.wait_generation >= record.wait_generation", "current.wait_generation > record.wait_generation + 1_000_000")],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(which, "applied")
