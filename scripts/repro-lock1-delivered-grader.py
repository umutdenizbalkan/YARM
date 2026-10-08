#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""QEMU-LOCK1-SEAL §1 — reproduce three false passes against the UNCHANGED delivered grader.

Builds checksum-valid mutations of the delivered grader's own passing fixture (its `_synth()`),
in the delivered record schema, and runs the delivered grader on each. Also re-checks the four
LOCK1-ACCEPTANCE reproductions and the positive fixture so the baseline is visible.
Usage: repro-lock1-delivered-grader.py <grader.py>   (e.g. `git show a828a41a:scripts/grade-riscv64-lock1-witness.py > g.py`)
Preserved as evidence: against the delivered grader the three NEW cases are accepted (rc=0).
"""
import importlib.util
import os
import re
import subprocess
import sys
import tempfile

G = sys.argv[1]
spec = importlib.util.spec_from_file_location("dg", G)
src = open(G).read()
# Import only the fixture helpers (everything before the --self-test dispatch / inputs).
ns = {}
exec(src.split('if len(sys.argv) > 1 and sys.argv[1] == "--self-test":')[0], ns)
fnv1a, _synth = ns["fnv1a"], ns["_synth"]

REC = re.compile(r"^LOCK1_REC seq=(\d+) kind=(\w+) hart=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) "
                 r"f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+) pass=(\d) crc=0x[0-9a-f]+$")


def split(log):
    head, logical = [], []
    for l in log.split("\n"):
        m = REC.match(l)
        if not m:
            head.append(l)
            continue
        if m.group(9) != "1":
            continue  # rebuild both passes from pass-1 copies
        logical.append([m.group(2), int(m.group(3))] + [[int(m.group(i), 16) for i in range(4, 9)]])
    return head, logical


def emit(head, logical):
    out = list(head)
    for p in (1, 2):
        for seq, (k, h, f) in enumerate(logical):
            b = "LOCK1_REC seq=%d kind=%s hart=%d f0=0x%x f1=0x%x f2=0x%x f3=0x%x f4=0x%x" % (
                seq, k, h, *f)
            out.append("%s pass=%d crc=0x%08x" % (b, p, fnv1a(b)))
    return "\n".join(out)


def rnd(e):
    k, _, f = e
    return f[1] if k in ("acquire", "contended", "release", "hold") else f[0]


def overlap(logical, r=5):
    holder = 0 if r % 2 else 1
    out = []
    for e in logical:
        out.append(e)
        if e[0] == "hold" and rnd(e) == r:
            out.append(["acquire", 1 - holder, [1, r, holder, 0, 0]])  # waiter acquires; holder unreleased
    return out


def orphan_release(logical, r=5):
    holder = 0 if r % 2 else 1
    out, done = [], False
    for e in logical:
        if not done and rnd(e) == r:
            out.append(["release", 1 - holder, [1, r, 0, 0, 0]])  # release with no acquisition
            done = True
        out.append(e)
    return out


def delete_round(logical, r=7):
    return [e for e in logical if rnd(e) != r]


def run(log):
    fd, p = tempfile.mkstemp()
    os.write(fd, log.encode())
    os.close(fd)
    fi, pi = tempfile.mkstemp()
    os.write(fi, b"firmware_sha256=" + b"0" * 64 + b"\n")
    os.close(fi)
    r = subprocess.run([sys.executable, G, p, pi, "/dev/null"], capture_output=True, text=True)
    os.unlink(p)
    os.unlink(pi)
    seal = [l for l in r.stdout.splitlines() if l.startswith("LOCK1_WITNESS_SEAL")]
    return r.returncode, (seal[-1] if seal else "")


head, base = split(_synth())
cases = [
    ("positive fixture", emit(head, base)),
    ("NEW overlapping owners", emit(head, overlap(base))),
    ("NEW release without acquisition", emit(head, orphan_release(base))),
    ("NEW deleted round 7", emit(head, delete_round(base))),
    ("prior: lock 99", _synth(event_lock=99)),
    ("prior: waiter release before acquire", _synth(rel_w_before_acq=True)),
    ("prior: IPI after releases", _synth(ipi_after=True)),
    ("prior: one holder unmasked", _synth(unmasked_round=7)),
]
for name, log in cases:
    rc, seal = run(log)
    print("%-40s rc=%d  %s" % (name, rc, "PASS(accepted)" if rc == 0 else "FAIL(rejected)"))
    print("    " + seal)
