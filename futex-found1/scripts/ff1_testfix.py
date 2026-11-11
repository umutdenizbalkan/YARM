#!/usr/bin/env python3
"""FUTEX-FOUND1: migrate the existing tests to the keyed futex contract.
Usage: ff1_testfix.py <worktree>"""
import pathlib, re, sys
wt = pathlib.Path(sys.argv[1])
T = wt / "src/kernel/boot/tests.rs"
O = wt / "src/kernel/boot/overtaken_tests/tests.rs"
PRIV = "crate::kernel::boot::futex::FutexKind::Private"
TOKEN0 = "crate::kernel::boot::futex::FutexWaitToken { slot: 0, generation: 0 }"

def calls(text, needle, transform):
    """Rewrite every call `needle(args)` (needle includes the opening paren) via transform(args)."""
    out = []; i = 0
    while True:
        j = text.find(needle, i)
        if j < 0:
            out.append(text[i:]); return "".join(out)
        # skip occurrences inside string literals on the same line
        line_start = text.rfind("\n", 0, j) + 1
        if text[line_start:j].count('"') % 2 == 1:
            out.append(text[i:j + len(needle)]); i = j + len(needle); continue
        k = j + len(needle); depth = 1; m = k
        while depth:
            c = text[m]
            if c in "([{": depth += 1
            elif c in ")]}": depth -= 1
            m += 1
        args = []; cur = ""; d = 0
        for c in text[k:m - 1]:
            if c in "([{": d += 1
            if c in ")]}": d -= 1
            if c == "," and d == 0:
                args.append(cur.strip()); cur = ""
            else:
                cur += c
        if cur.strip(): args.append(cur.strip())
        new = transform(args)
        out.append(text[i:k]); out.append(new if new is not None else text[k:m - 1]); out.append(")")
        i = m

def fix(text):
    # broad wait: (addr, expected, observed) -> (addr, expected, Private)
    text = calls(text, ".futex_wait_current(", lambda a: f"{a[0]}, {a[1]}, {PRIV}" if len(a) == 3 else None)
    # broad wake: (addr, n) -> (addr, n, Private)
    text = calls(text, ".futex_wake(", lambda a: f"{a[0]}, {a[1]}, {PRIV}" if len(a) == 2 else None)
    # split park: add the kind
    text = calls(text, ".futex_wait_park_exact_split(", lambda a: ", ".join(a + [PRIV]) if len(a) == 5 else None)
    # split decide: (tid, addr, expected, observed) -> (tid, addr, expected, Private)
    text = calls(text, ".futex_wait_decide_split_read(", lambda a: f"{a[0]}, {a[1]}, {a[2]}, {PRIV}" if len(a) == 4 else None)
    # A status that only has to BE futex-blocked (no wake must find it): any token.
    text = re.sub(r"WaitReason::Futex\(\s*(crate::kernel::vm::)?VirtAddr\(\s*0x[0-9a-fA-F_]+\s*,?\s*\)\s*,?\s*\)",
                  f"WaitReason::Futex({TOKEN0})", text)
    return text

for p in [T, O]:
    p.write_text(fix(p.read_text()))
print("testfix (mechanical) applied")
