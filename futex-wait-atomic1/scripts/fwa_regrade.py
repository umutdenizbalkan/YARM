#!/usr/bin/env python3
"""Supplementary (not a gate): find wait-witness marker lines damaged by serial interleaving, and
re-grade each log with the UNCHANGED grade() after repairing only the damaged marker prefixes.
Usage: fwa_regrade.py <grader.py> <boot.norm.log>..."""
import importlib.util, re, sys

spec = importlib.util.spec_from_file_location("g", sys.argv[1]); g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
CANON = re.compile(r'FUTEX_WAIT_WITNESS_(WINDOW_OPEN|WINDOW_CLOSE|IN_WINDOW_WAKE|RESULT|SKIPPED) ')
TAIL = re.compile(r'(WINDOW_OPEN kind=|WINDOW_CLOSE kind=|IN_WINDOW_WAKE cpu=|WITNESS_RESULT wait=|WITNESS_SKIPPED wait=)')
for path in sys.argv[2:]:
    lines = open(path, encoding="latin-1").read().splitlines()
    damaged, repaired = [], []
    for l in lines:
        m = TAIL.search(l)
        if m and not CANON.search(l):
            damaged.append(l.strip()[:120])
            kind = m.group(1).split(" ")[0]
            kind = {"WITNESS_RESULT": "RESULT", "WITNESS_SKIPPED": "SKIPPED"}.get(kind, kind)
            l = f"FUTEX_WAIT_WITNESS_{kind} " + l[m.end() - len(m.group(1).split(' ', 1)[1]):]
        repaired.append(l)
    smp = 2
    f0, facts0 = g.grade(lines, smp)
    f1, facts1 = g.grade(repaired, smp)
    print(f"REGRADE {path} damaged={len(damaged)} as_logged_fails={len(f0)} repaired_fails={len(f1)} "
          f"windows {facts0.get('windows')}->{facts1.get('windows')} waits {facts0.get('waits')}->{facts1.get('waits')}")
    for d in damaged:
        print(f"  damaged: {d}")
    for x in f1:
        print(f"  repaired-fail: {x}")
