#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK1 — the independent grader for the real subdomain-lock contention witness.
#
# It re-derives every credited contended round from the raw sealed `LOCK1_REC` lines (never from the
# kernel's own summary), requires the complete causal chain per round — the holder acquired the VM
# lock, the contender actually observed it HELD, the holder released, the contender then acquired —
# plus the §3 interrupt evidence (the holder was masked, an IPI was published to it during the
# window, and in enough rounds that software interrupt was seen pending under the mask), and finally
# requires the SMP3 seal from the same boot to report `result=ok` (progress, correct results, intact
# context). It never retries. Usage: grade-riscv64-lock1-witness.py <boot.log> <artifact-identity>
#                                   [<firmware.pin>]
import re
import sys

LOG = sys.argv[1]
IDENT = sys.argv[2] if len(sys.argv) > 2 else None
PIN = sys.argv[3] if len(sys.argv) > 3 else "scripts/firmware/riscv64-opensbi.pin"

ROUNDS = 12
MIN_PER_DIRECTION = 4
MIN_SSIP_ROUNDS = 4

text = open(LOG, errors="replace").read()
lines = text.split("\n")
fails = []


def fail(msg):
    fails.append(msg)


def fnv1a(s: str) -> int:
    h = 0x811C9DC5
    for b in s.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


# ── firmware identity (same pin the SMP3 gate uses) ──
try:
    pin = dict(
        l.split("=", 1)
        for l in open(PIN).read().splitlines()
        if "=" in l and not l.startswith("#")
    )
except OSError:
    pin = {}
ident = ""
if IDENT:
    try:
        ident = open(IDENT).read()
    except OSError:
        ident = ""
m = re.search(r"^firmware_sha256=([0-9a-f]{64})$", ident, re.M)
if not m:
    fail("the run recorded no firmware identity")
elif pin and m.group(1) != pin.get("sha256"):
    fail("the booted firmware %s is not the pinned %s" % (m.group(1), pin.get("sha256")))
osbi = next((l for l in lines if re.search(r"OpenSBI v\d", l)), None)
if not osbi:
    fail("the OpenSBI banner is missing")
elif pin and pin.get("banner") and pin["banner"] not in osbi:
    fail("the banner names %r, not the pinned %r" % (osbi.strip(), pin["banner"]))

# ── the SMP3 seal must pass on this same boot (progress / correct results / context) ──
smp3 = [l for l in lines if l.startswith("SMP3_VERDICT ")]
if not smp3:
    fail("no SMP3 verdict on this boot")
elif not any("result=ok" in l for l in smp3):
    fail("the SMP3 seal did not pass: " + smp3[-1][:120])

# ── parse LOCK1 records, taking an intact (crc-valid) copy of each seq ──
meta = next((l for l in lines if l.startswith("LOCK1_META ")), None)
if not meta:
    fail("no LOCK1_META line")
else:
    mo = re.search(r"overflow=(\d+)", meta)
    if mo and int(mo.group(1)) != 0:
        fail("the LOCK1 record overflowed its ring")

recs = {}
rx = re.compile(
    r"^(LOCK1_REC seq=(\d+) kind=(\w+) hart=(\d+) "
    r"f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+))"
    r" pass=\d+ crc=0x([0-9a-f]+)$"
)
for l in lines:
    mm = rx.match(l.strip())
    if not mm:
        continue
    body = mm.group(1)
    if fnv1a(body) != int(mm.group(10), 16):
        continue  # torn copy; the other pass carries an intact one
    seq = int(mm.group(2))
    recs[seq] = dict(
        seq=seq,
        kind=mm.group(3),
        hart=int(mm.group(4)),
        f=[int(mm.group(i), 16) for i in range(5, 10)],
    )
ev = [recs[k] for k in sorted(recs)]


def has(kind, round_, pred):
    return [e for e in ev if e["kind"] == kind and e["f"][1] == round_ and pred(e)]


credited = {0: 0, 1: 0}
ssip_rounds = 0
for r in range(1, ROUNDS + 1):
    holder = 0 if r % 2 == 1 else 1
    waiter = 1 - holder
    # IPI published to the holder during the window, by the waiter, successfully (f0=round here).
    ipi = [
        e
        for e in ev
        if e["kind"] == "ipi"
        and e["f"][0] == r
        and e["f"][1] == holder
        and e["f"][2] == 1
        and e["hart"] == waiter
    ]
    acq_h = has("acquire", r, lambda e: e["hart"] == holder)
    con = has("contended", r, lambda e: e["hart"] == waiter and e["f"][2] == holder)
    rel_h = has("release", r, lambda e: e["hart"] == holder)
    acq_w = has("acquire", r, lambda e: e["hart"] == waiter)
    rel_w = has("release", r, lambda e: e["hart"] == waiter)
    hold = has("hold", r, lambda e: e["hart"] == holder)
    why = None
    if not acq_h:
        why = "holder never acquired"
    elif not con:
        why = "contender never observed the lock held by the holder"
    elif not rel_h:
        why = "holder never released"
    elif not acq_w:
        why = "contender never acquired after release"
    elif not rel_w:
        why = "contender never released"
    elif not hold:
        why = "no hold record"
    elif any(e["f"][2] != 0 for e in hold):
        why = "holder was not masked (sstatus.SIE set) inside the critical section"
    elif not ipi:
        why = "no production IPI was published to the holder during the window"
    else:
        # The causal order, from the records' own sequence: a holder-acquire precedes a
        # contender-observation (naming the holder), which precedes a holder-release, which
        # precedes the contender's acquire.
        a = min(e["seq"] for e in acq_h)
        c = min((e["seq"] for e in con if e["seq"] > a), default=None)
        rl = min((e["seq"] for e in rel_h if c is not None and e["seq"] > c), default=None)
        aw = min((e["seq"] for e in acq_w if rl is not None and e["seq"] > rl), default=None)
        if c is None or rl is None or aw is None:
            why = "the acquire/contend/release/acquire chain is not ordered"
    if why:
        print("[lock1-witness] round %d (holder hart %d): %s" % (r, holder, why))
        continue
    credited[waiter] += 1
    if any(e["f"][3] == 1 for e in hold):
        ssip_rounds += 1

if credited[1] < MIN_PER_DIRECTION:
    fail(
        "contended rounds with S (hart 1) waiting: %d, need >= %d"
        % (credited[1], MIN_PER_DIRECTION)
    )
if credited[0] < MIN_PER_DIRECTION:
    fail(
        "contended rounds with C (hart 0) waiting: %d, need >= %d"
        % (credited[0], MIN_PER_DIRECTION)
    )
if ssip_rounds < MIN_SSIP_ROUNDS:
    fail(
        "rounds that observed the IPI pending (sip.SSIP) under the mask: %d, need >= %d"
        % (ssip_rounds, MIN_SSIP_ROUNDS)
    )

for f in fails:
    print("[lock1-witness][fail] " + f)
ok = not fails
print(
    "LOCK1_WITNESS_SEAL rounds=%d credited_c_waits=%d credited_s_waits=%d ssip_rounds=%d result=%s"
    % (ROUNDS, credited[0], credited[1], ssip_rounds, "ok" if ok else "fail")
)
sys.exit(0 if ok else 1)
