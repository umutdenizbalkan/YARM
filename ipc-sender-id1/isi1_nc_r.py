#!/usr/bin/env python3
"""IPC-SENDER-ID1 revised control nc1r (added after the frozen run; disclosed). nc1 also
disarmed the deadline on a receiver claim, which by itself keeps the timeout from firing, so it did
not restore the baseline shape. nc1r does: a receiver claim parks the completion and writes nothing
else (status stays Blocked, deadline stays armed). Usage: isi1_nc_r.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); p = wt / "src/kernel/boot/sender_waiter.rs"; s = p.read_text()
old = "    tcb.status = TaskStatus::Runnable;\n    tcb.ipc_timeout_deadline = None;\n    tcb.ipc_timeout_fired = timed_out;\n"
new = "    if timed_out {\n        tcb.status = TaskStatus::Runnable;\n        tcb.ipc_timeout_deadline = None;\n        tcb.ipc_timeout_fired = timed_out;\n    }\n"
assert s.count(old) == 1
p.write_text(s.replace(old, new)); print("nc1r applied")
