#!/usr/bin/env python3
"""Build the IPC-SENDER-ID1 baseline-reproducer tree: 16e5c04 + only the regression tests that use
production functions (no new owner module). Usage: isi1_baseline_tree.py <wt> <scratch>"""
import pathlib, re, sys
wt = pathlib.Path(sys.argv[1]); sc = pathlib.Path(sys.argv[2])
def drop_fn(src, name):
    i = src.index(f"fn {name}(")
    start = src.rfind("    #[test]", 0, i)
    # include the doc comment block above #[test]
    while True:
        prev = src.rfind("\n", 0, start - 1) + 1
        if src[prev:start].lstrip().startswith("///"):
            start = prev
        else:
            break
    j = src.index("{", i); depth = 0
    while True:
        if src[j] == "{": depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0: break
        j += 1
    return src[:start] + src[j + 1:]
m = (sc / "isi1_tests.rs").read_text()
m = m.replace('''    use crate::kernel::boot::sender_waiter::{
        SendCycleEnding, claim_blocked_send_cycle_locked, remove_sender_waiter_for_cycle_locked,
        remove_sender_waiters_for_task_locked,
    };
''', "")
for f in ["the_claim_refuses_every_other_cycle_without_mutation",
          "the_task_scoped_removal_takes_every_waiter_of_that_tcb_and_nothing_else",
          "the_cycle_removal_matches_the_whole_triple"]:
    m = drop_fn(m, f)
T = wt / "src/kernel/boot/tests.rs"; s = T.read_text()
a = s.index("        mod shared_region {\n"); i = s.index("{", a); d = 0
while True:
    if s[i] == "{": d += 1
    elif s[i] == "}":
        d -= 1
        if d == 0: break
    i += 1
ls = s.rfind("\n", 0, i) + 1
s = s[:ls] + (sc / "isi1_tests_sr.rs").read_text().lstrip("\n") + s[ls:]
s = s.rstrip("\n") + "\n" + m
T.write_text(s)
print("baseline tree ready")
