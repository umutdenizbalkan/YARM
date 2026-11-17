#!/usr/bin/env python3
"""FUTEX-ABS-BITSET1 negative controls: apply one mutation to src/kernel/boot/futex.rs (cwd = the
worktree). Each mutation must occur exactly once; the caller restores the file with git checkout."""
import sys
P = "src/kernel/boot/futex.rs"
REKEY = """    if let Some(r) = table.records[slot].as_mut() {
        r.key = key;
        if let Some(va) = va {
            r.va = va;
        }
    }"""
NC = {
    # NC1 — wrong clock after rekey: a moved record's instant is reinterpreted in scheduler-tick
    # units (10 ms), i.e. judged on another clock than the one it was admitted on.
    "nc1_wrong_clock_after_rekey": [(REKEY, """    if let Some(r) = table.records[slot].as_mut() {
        r.key = key;
        if let Some(va) = va {
            r.va = va;
        }
        r.deadline = r.deadline.map(|d| FutexDeadline::MonotonicNs(d.raw() / 10_000_000));
    }""")],
    # NC2 — ignored wake mask: every wake matches every waiter.
    "nc2_ignored_wake_mask": [("""pub const fn futex_masks_match(wait: u32, wake: u32) -> bool {
    wait & wake != 0
}""", """pub const fn futex_masks_match(wait: u32, wake: u32) -> bool {
    let _ = (wait, wake);
    true
}""")],
    # NC3a — the requeue resets the mask.
    "nc3a_reset_mask_on_requeue": [(REKEY, """    if let Some(r) = table.records[slot].as_mut() {
        r.key = key;
        if let Some(va) = va {
            r.va = va;
        }
        r.mask = FUTEX_BITSET_MATCH_ANY;
    }""")],
    # NC3b — the requeue drops the deadline.
    "nc3b_reset_deadline_on_requeue": [(REKEY, """    if let Some(r) = table.records[slot].as_mut() {
        r.key = key;
        if let Some(va) = va {
            r.va = va;
        }
        if !matches!(key, FutexKey::Expired { .. }) {
            r.deadline = None;
        }
    }""")],
    # NC4 — duplicate settlement: the expiry claims the cycle for the timeout but leaves the record
    # on its user key, so a wake can settle it a second time.
    "nc4_duplicate_settlement": [("""    let prev = table.prev_of(slot);
    rekey(
        table,
        slot,
        prev,
        FutexKey::Expired { tid: candidate.tid },
        None,
    );
    true""", """    let _ = table.prev_of(slot);
    true""")],
}
name = sys.argv[1]
s = open(P).read()
for a, b in NC[name]:
    assert s.count(a) == 1, (name, a[:60], s.count(a))
    s = s.replace(a, b)
open(P, "w").write(s)
print(f"applied {name}")
