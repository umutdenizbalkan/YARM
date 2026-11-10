#!/usr/bin/env python3
"""SEND-TIMEOUT-ARB1: add the reproducers/regressions. Usage: sta1_add_tests.py <wt> <scratch>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); sc = pathlib.Path(sys.argv[2])
T = wt / "src/kernel/boot/tests.rs"; s = T.read_text()
def close_of(s, opener):
    a = s.index(opener); i = s.index("{", a); d = 0
    while True:
        if s[i] == "{": d += 1
        elif s[i] == "}":
            d -= 1
            if d == 0: return i
        i += 1
i = close_of(s, "        mod shared_region {\n")
ls = s.rfind("\n", 0, i) + 1
s = s[:ls] + (sc / "sta1_tests_sr.rs").read_text() + s[ls:]
i = close_of(s, "\nmod ipc_sender_id1 {\n")
ls = s.rfind("\n", 0, i) + 1
s = s[:ls] + (sc / "sta1_tests.rs").read_text().lstrip("\n") + s[ls:]
T.write_text(s)
print("sta1 tests added")
