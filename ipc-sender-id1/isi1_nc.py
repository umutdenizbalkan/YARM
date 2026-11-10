#!/usr/bin/env python3
"""IPC-SENDER-ID1 negative controls. Usage: isi1_nc.py <worktree> <ncN>
Each control reintroduces ONE ownership defect into the candidate; the hosted suites must fail."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
SW = "src/kernel/boot/sender_waiter.rs"
I = "src/kernel/boot/ipc_state.rs"
R = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
C = {
 # NC1 the receiver-side publication is not a claim: it parks the completion but leaves the
 # sender Blocked (the baseline shape), so a later timeout can claim the same cycle
 "nc1": [(SW, "    tcb.status = TaskStatus::Runnable;\n",
              "    if timed_out {\n        tcb.status = TaskStatus::Runnable;\n    }\n")],
 # NC2 the broad seam wakes the bare TID whatever the claim said
 "nc2": [(I, "        if self.publish_blocking_send_completion(target, result) {\n            self.wake_tid_to_runnable(target.tid)",
             "        if {\n            self.publish_blocking_send_completion(target, result);\n            true\n        } {\n            self.wake_tid_to_runnable(target.tid)")],
 # NC3 the in-lock split seam wakes the bare TID whatever the claim said
 "nc3": [(I, "        if self.publish_blocking_send_completion(target, Self::SEND_COMPLETION_OK) {",
             "        if {\n            self.publish_blocking_send_completion(target, Self::SEND_COMPLETION_OK);\n            true\n        } {")],
 # NC4 the off-lock seam wakes the bare TID whatever the claim said
 "nc4": [(R, "            crate::kernel::boot::KernelState::SEND_COMPLETION_OK,\n        ) {\n            return Ok(());\n        }",
             "            crate::kernel::boot::KernelState::SEND_COMPLETION_OK,\n        ) {\n        }")],
 # NC5 the claim ignores the send generation (an older cycle's target claims a newer cycle)
 "nc5": [(SW, "    if tcb.blocked_send_generation != target.send_generation {\n        return false;\n    }\n", "")],
 # NC6 the claim matches the numeric TID alone (another incarnation / an absent ASID claims)
 "nc6": [(SW, ".find(|t| t.tid == target.tid && t.asid == target.asid)", ".find(|t| t.tid == target.tid)")],
 # NC7 the timeout claim ignores the armed deadline
 "nc7": [(SW, "            if tcb.ipc_timeout_deadline != Some(deadline) || now < deadline {",
              "            if false && (tcb.ipc_timeout_deadline != Some(deadline) || now < deadline) {")],
 # NC8 the cycle removal matches the numeric TID alone
 "nc8": [(SW, ".position(|s| s.is_some_and(|w| w.wake_target() == target))",
              ".position(|s| s.is_some_and(|w| w.tid == target.tid))")],
 # NC9 the cycle removal leaves a hole (no compaction)
 "nc9": [(SW, "        let removed = queue[slot].take().expect(\"position guarantees Some\");\n        compact_sender_queue(queue);",
              "        let removed = queue[slot].take().expect(\"position guarantees Some\");")],
 # NC10 the task-scoped removal leaves holes (no compaction)
 "nc10": [(SW, "        if removed_any {\n            compact_sender_queue(queue);\n        }", "        let _ = removed_any;")],
 # NC11 the task-scoped removal drops cap-bearing waiters without reporting them (envelope and
 # pin leak on exit, death and reap)
 "nc11": [(SW, "                && orphaned_n < orphaned.len()\n", "                && orphaned_n < orphaned.len()\n                && false\n")],
 # NC12 the timeout drain claims but never removes the waiter (stranded waiter, unsettled envelope)
 "nc12": [(R, "                    crate::kernel::boot::sender_waiter::remove_sender_waiter_for_cycle_locked(\n                        ipc, target,\n                    )?;",
              "                    crate::kernel::boot::sender_waiter::remove_sender_waiter_for_cycle_locked(\n                        ipc, target,\n                    )\n                    .filter(|_| false)?;")],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
