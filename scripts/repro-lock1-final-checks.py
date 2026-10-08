#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""QEMU-LOCK1 final checks — reproduce two checker false passes against a given LOCK1 grader.

Both are CHECKER defects: the logs below are checksum-valid edits of genuine recordings (the
grader's own passing fixture and, optionally, a real qualified boot log). They are not observed
kernel behaviour.

  1. "dropped pair": every dump copy of one complete acquire/release pair is deleted. The remaining
     records keep their original sequence numbers (leaving gaps) and the dump's metadata and
     completion records still declare the original record count.
  2. "early arrival": the attributed arrival of one round is moved after that round's masked-pending
     observation but before the holder's matching release record (records renumbered, checksums
     recomputed, metadata/completion counts unchanged).

Usage: repro-lock1-final-checks.py <grader.py> [<live boot.log> <artifact-identity>]
   e.g. `git show 0fa9ec96:scripts/grade-riscv64-lock1-witness.py > g.py`
Exit status is 0; each case prints the grader's verdict line.
"""
import os
import re
import subprocess
import sys
import tempfile

G = sys.argv[1]
src = open(G).read()
ns = {}
exec(src.split('if len(sys.argv) > 1 and sys.argv[1] == "--self-test":')[0], ns)
fnv1a, _synth = ns["fnv1a"], ns["_synth"]

REC = re.compile(r"^LOCK1_REC seq=(\d+) kind=(\w+) hart=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) "
                 r"f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+) pass=(\d) crc=0x[0-9a-f]+$")
LOCK = ("acquire", "contended", "release", "hold")


def parse(log):
    """-> (lines, {seq: (kind, hart, f)}) from intact pass-1 copies."""
    lines = log.split("\n")
    recs = {}
    for l in lines:
        m = REC.match(l.strip())
        if m and m.group(9) == "1":
            recs[int(m.group(1))] = (m.group(2), int(m.group(3)),
                                     [int(m.group(i), 16) for i in range(4, 9)])
    return lines, recs


def rnd(k, f):
    return f[1] if k in LOCK else f[0]


def body(seq, k, h, f):
    return "LOCK1_REC seq=%d kind=%s hart=%d f0=0x%x f1=0x%x f2=0x%x f3=0x%x f4=0x%x" % (
        seq, k, h, *f)


def dropped_pair(log, r=5):
    """Delete every copy of the LAST complete acquire/release pair of round r (the waiter's or the
    holder's install), keeping sequence numbers and the declared counts unchanged."""
    lines, recs = parse(log)
    acq = [s for s, (k, h, f) in sorted(recs.items()) if k == "acquire" and f[1] == r]
    a = acq[-1]
    aid = recs[a][2][2]
    rel = next(s for s, (k, h, f) in sorted(recs.items())
               if s > a and k == "release" and f[2] == aid)
    gone = {a, rel}
    out = [l for l in lines
           if not (REC.match(l.strip()) and int(REC.match(l.strip()).group(1)) in gone)]
    return "\n".join(out), "deleted seq %d (acquire id %d) and seq %d (its release)" % (a, aid, rel)


def early_arrival(log, r=5):
    """Move round r's arrival to just after its pending observation (before the holder's release)."""
    lines, recs = parse(log)
    order = [recs[s] for s in sorted(recs)]
    ia = next(i for i, (k, h, f) in enumerate(order) if k == "arrival" and f[0] == r)
    arr = order.pop(ia)
    ip = next(i for i, (k, h, f) in enumerate(order) if k == "pending" and f[0] == r)
    aid = order[ip][2][4]
    irel = next(i for i, (k, h, f) in enumerate(order) if k == "release" and f[2] == aid)
    assert ip < irel
    order.insert(ip + 1, arr)
    first = min(recs)
    new = {}
    for i, (k, h, f) in enumerate(order):
        new[first + i] = (k, h, f)
    out, emitted = [], set()
    for l in lines:
        m = REC.match(l.strip())
        if not m:
            out.append(l)
            continue
        p = m.group(9)
        if p in emitted:
            continue
        emitted.add(p)
        for s in sorted(new):
            b = body(s, *new[s])
            out.append("%s pass=%s crc=0x%08x" % (b, p, fnv1a(b)))
    return "\n".join(out), "arrival of round %d moved between its pending (acq %d) and that " \
        "acquisition's release" % (r, aid)


def run(log, ident=None):
    fd, p = tempfile.mkstemp()
    os.write(fd, log.encode())
    os.close(fd)
    if ident is None:
        fi, pi = tempfile.mkstemp()
        os.write(fi, b"firmware_sha256=" + b"0" * 64 + b"\n")
        os.close(fi)
        args = [p, pi, "/dev/null"]
    else:
        pi = None
        args = [p, ident]
    r = subprocess.run([sys.executable, G] + args, capture_output=True, text=True)
    os.unlink(p)
    if pi:
        os.unlink(pi)
    seal = [l for l in r.stdout.splitlines() if l.startswith("LOCK1_WITNESS_SEAL")]
    fails = [l for l in r.stdout.splitlines() if "[fail]" in l]
    return r.returncode, (seal[-1] if seal else ""), fails


sources = [("delivered fixture", _synth(), None)]
if len(sys.argv) > 3:
    sources.append(("live boot " + sys.argv[2], open(sys.argv[2], errors="replace").read(),
                    sys.argv[3]))
for label, log, ident in sources:
    rc, seal, _ = run(log, ident)
    print("%s — unmodified: rc=%d\n    %s" % (label, rc, seal))
    for name, fn in (("dropped pair", dropped_pair), ("early arrival", early_arrival)):
        mlog, what = fn(log)
        rc, seal, fails = run(mlog, ident)
        print("%s — %s (%s): rc=%d %s\n    %s" % (label, name, what, rc,
                                                 "ACCEPTED" if rc == 0 else "rejected", seal))
        for f in fails[:3]:
            print("    " + f)
