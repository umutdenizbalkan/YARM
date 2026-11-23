#!/usr/bin/env python3
"""FUTEX-SEAL1 negative controls (cwd = the worktree). Each mutation names its file and must
match exactly the stated number of times; the caller restores with `git checkout -- <file>`."""
import sys
NC = {
    # A — wake-all substituted in the arm labelled rq: the paired broadcast's rq rounds run the
    # witness's wake-all control while still reporting arm=rq. Application correctness holds;
    # the grader's PB rule must reject the requeue claim.
    "ncA_wakeall_in_rq": ("crates/yarm-control-plane-servers/src/control_plane/init/futex_seal_witness.rs", [
        ('if arm == "rq" {', 'if arm == "never" {', 1),
    ]),
    # B — a condition-variable timeout returns WITHOUT reacquiring the mutex (FUTEX-SYNC-TIME1 NC2):
    # the RB cell's timed condition wait must be caught by the RB rule.
    "ncB_condvar_no_reacquire": ("crates/yarm-user-rt/src/lib.rs", [
        ("""            let result = ops.wait(&self.seq, seen, self.scope, Some(deadline_ns));""",
         """            let result = ops.wait(&self.seq, seen, self.scope, Some(deadline_ns));
            if matches!(result, Ok(FutexTimedWait::TimedOut)) {
                return WaitOutcome::TimedOut;
            }""", 1),
    ]),
    # C — duplicate settlement (FUTEX-ABS-BITSET1 NC4): the timeout claim no longer rekeys the
    # record to Expired, so a cycle can be settled again. Hosted futex suites must fail.
    "ncC_duplicate_settlement": ("src/kernel/boot/futex.rs", [
        ("""    let prev = table.prev_of(slot);
    rekey(
        table,
        slot,
        prev,
        FutexKey::Expired { tid: candidate.tid },
        None,
    );
    true""", """    let _ = table.prev_of(slot);
    true""", 1),
    ]),
}
name = sys.argv[1]
path, edits = NC[name]
if len(sys.argv) > 2 and sys.argv[2] == "--path":
    print(path)
    sys.exit(0)
s = open(path).read()
for a, b, n in edits:
    assert s.count(a) == n, (name, a[:60], s.count(a))
    s = s.replace(a, b)
open(path, "w").write(s)
print(f"applied {name} to {path}")
