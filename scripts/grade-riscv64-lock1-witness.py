#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK1-ACCEPTANCE — the independent grader for the real subdomain-lock contention witness.
#
# It re-derives every credited contended round from the raw sealed `LOCK1_REC` lines (never from the
# kernel's own summary) by validating the COMPLETE relevant event history of each round against the
# ownership-interval contract, and it establishes the §3 interrupt chain by linked identity in a
# single shared record sequence:
#
#   * the production IPI was published to the designated holder (`ipi`), before the holder left the
#     witnessed ownership interval;
#   * the holder held the one VM lock (`vm_lock_id`) with supervisor interrupts masked (`hold`,
#     sstatus.SIE = 0) while the contender actually observed it held (`contended`);
#   * the holder released, the contender then acquired, completed and released (handover established
#     by the contender's own successful acquisition, not by assuming an observer ordering);
#   * the holder, once unmasked, consumed that pending software interrupt through the production
#     arrival owner (`arrival`), after the interval.
#
# Broken obligations FAIL the boot — they never silently become "uncredited" and hide behind the
# coverage threshold. A round earns no credit only when its complete, valid history shows it was not
# contended. The SMP3 seal on the same boot corroborates progress/results/context; it does not
# substitute for any obligation above. It never retries.
#
# Usage: grade-riscv64-lock1-witness.py <boot.log> <artifact-identity> [<firmware.pin>]
import re
import sys

VM_LOCK_ID = 1
ROUNDS = 12
MIN_PER_DIRECTION = 4
# Rounds whose masked holder additionally saw the IPI PENDING (sip.SSIP = 1) — the firmware's M→S
# reflection is cold on a boot's first rounds, so this count varies while the masked-publication
# proof (required every credited round) does not.
MIN_SSIP_ROUNDS = 2
# Rounds whose pending IPI was then consumed through the production arrival owner after the holder
# unmasked — the delivery end of the chain.
MIN_ARRIVAL_ROUNDS = 2

KINDS = {"acquire", "contended", "release", "hold", "ipi", "arrival"}
LOCK_KINDS = {"acquire", "contended", "release", "hold"}


def fnv1a(s: str) -> int:
    h = 0x811C9DC5
    for b in s.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


# ── synthetic fixtures (self-test / control generation) ──────────────────────────────────────
def _body(seq, kind, hart, f):
    return "LOCK1_REC seq=%d kind=%s hart=%d f0=0x%x f1=0x%x f2=0x%x f3=0x%x f4=0x%x" % (
        seq, kind, hart, f[0], f[1], f[2], f[3], f[4])


def _line(seq, kind, hart, f, pass_, crc=None):
    b = _body(seq, kind, hart, f)
    return "%s pass=%d crc=0x%08x" % (b, pass_, crc if crc is not None else fnv1a(b))


def _synth(
    rounds=12,
    lock_id=VM_LOCK_ID,
    event_lock=VM_LOCK_ID,
    drop=None,            # per-round kind to omit: acquire/contended/release/hold/ipi/arrival/acq_w/rel_w
    unmasked_round=None,  # round whose holder records SIE=1
    wrong_holder=False,   # contended names the other CPU
    ipi_after=False,      # IPI published after both releases
    rel_w_before_acq=False,  # waiter release emitted before its acquire
    ipi_wrong_target=False,  # IPI names the wrong holder as target
    arrival_wrong_round=False,  # arrival names another round
    conflict_seq=False,   # emit one seq twice with two different crc-valid bodies
    with_guard_query=True,  # include an extra uncontended holder acquire/release (multiple acquisitions)
):
    """Build a synthetic boot log in the acceptance schema, each record emitted in both dump passes."""
    header = [
        "OpenSBI v1.3",
        "SMP3_VERDICT result=ok reason=none at=0 pass=1 crc=0x0",
        "LOCK1_META vm_lock_id=%d rounds=%d slots_used=0 overflow=0 dump_cpu=0 pass=1 crc=0x0"
        % (lock_id, rounds),
    ]
    logical = []  # (kind, hart, f)
    for r in range(1, rounds + 1):
        holder = 0 if r % 2 == 1 else 1
        waiter = 1 - holder
        el = event_lock
        # The waiter publishes its production IPI to the holder before it contends.
        if drop != "ipi":
            tgt = (holder ^ 1) if ipi_wrong_target else holder
            if not ipi_after:
                logical.append(("ipi", waiter, [r, tgt, 1, waiter, 0]))
        # An uncontended guard-query acquisition by the holder (a second legitimate acquisition).
        if with_guard_query:
            logical.append(("acquire", holder, [el, r, holder, 0, 0]))
            logical.append(("release", holder, [el, r, 0, 0, 0]))
        # The contended, extended ownership interval.
        if drop != "acquire":
            logical.append(("acquire", holder, [el, r, holder, 0, 0]))
        if drop != "contended":
            ch = (holder ^ 1) if wrong_holder else holder
            logical.append(("contended", waiter, [el, r, ch, r, 0]))
        if drop != "hold":
            sie = 1 if unmasked_round == r else 0
            logical.append(("hold", holder, [el, r, sie, 1, holder]))
        if drop != "release":
            logical.append(("release", holder, [el, r, 0, 0, 0]))
        # Delivery: the holder consumes its pending IPI after the interval.
        if drop != "arrival":
            ar = (r + 1) if arrival_wrong_round else r
            logical.append(("arrival", holder, [ar, holder, 1, 0, 0]))
        # The contender's handover: acquire then (after) release.
        if rel_w_before_acq:
            if drop != "rel_w":
                logical.append(("release", waiter, [el, r, 0, 0, 0]))
            if drop != "acq_w":
                logical.append(("acquire", waiter, [el, r, holder, 0, 0]))
        else:
            if drop != "acq_w":
                logical.append(("acquire", waiter, [el, r, holder, 0, 0]))
            if drop != "rel_w":
                logical.append(("release", waiter, [el, r, 0, 0, 0]))
        if ipi_after and drop != "ipi":
            tgt = (holder ^ 1) if ipi_wrong_target else holder
            logical.append(("ipi", waiter, [r, tgt, 1, waiter, 0]))
    out = list(header)
    for pass_ in (1, 2):
        for seq, (kind, hart, f) in enumerate(logical):
            out.append(_line(seq, kind, hart, f, pass_))
    if conflict_seq:
        # A second, checksum-valid copy of seq 0 carrying different fields (a genuine conflict, not a
        # transport-redundant identical pass copy).
        k, h, f = logical[0]
        bad = list(f)
        bad[2] ^= 1
        out.append(_line(0, k, h, bad, 2))
    return "\n".join(out)


def _self_test():
    import subprocess
    import tempfile
    import os

    def run(log):
        fd, path = tempfile.mkstemp()
        os.write(fd, log.encode())
        os.close(fd)
        idfd, idpath = tempfile.mkstemp()
        os.write(idfd, b"firmware_sha256=" + b"0" * 64 + b"\n")
        os.close(idfd)
        r = subprocess.run(
            [sys.executable, sys.argv[0], path, idpath, "/dev/null"],
            capture_output=True, text=True)
        os.unlink(path)
        os.unlink(idpath)
        return r.returncode, r.stdout

    cases = [
        ("good", _synth(), 0),
        # §1 — the four reproductions, each must now fail for its specific violation.
        ("lock-99", _synth(event_lock=99), 1),
        ("waiter-release-before-acquire", _synth(rel_w_before_acq=True), 1),
        ("ipi-after-releases", _synth(ipi_after=True), 1),
        ("one-holder-unmasked", _synth(unmasked_round=7), 1),
        # §2 — ownership-interval obligations.
        ("no-acquire", _synth(drop="acquire"), 1),
        ("no-contended", _synth(drop="contended"), 1),
        ("no-holder-release", _synth(drop="release"), 1),
        ("no-waiter-acquire", _synth(drop="acq_w"), 1),
        ("no-waiter-release", _synth(drop="rel_w"), 1),
        ("no-hold", _synth(drop="hold"), 1),
        ("wrong-holder", _synth(wrong_holder=True), 1),
        ("meta-lock-not-1", _synth(lock_id=99), 1),
        ("too-few-rounds", _synth(rounds=6), 1),
        ("conflicting-copies", _synth(conflict_seq=True), 1),
        # §3 — causal IPI chain.
        ("no-ipi", _synth(drop="ipi"), 1),
        ("ipi-wrong-target", _synth(ipi_wrong_target=True), 1),
        ("no-arrival", _synth(drop="arrival"), 1),
        ("arrival-wrong-round", _synth(arrival_wrong_round=True), 1),
    ]
    bad = 0
    for name, log, want in cases:
        rc, _ = run(log)
        verdict = "PASS" if rc == want else "FAIL"
        if rc != want:
            bad += 1
        print("[self-test] %-30s rc=%d want=%d %s" % (name, rc, want, verdict))
    print("[self-test] %s" % ("ALL PASS" if not bad else "FAILURES"))
    sys.exit(1 if bad else 0)


if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
    _self_test()

# ── inputs ───────────────────────────────────────────────────────────────────────────────────
LOG = sys.argv[1]
IDENT = sys.argv[2] if len(sys.argv) > 2 else None
PIN = sys.argv[3] if len(sys.argv) > 3 else "scripts/firmware/riscv64-opensbi.pin"

text = open(LOG, errors="replace").read()
lines = text.split("\n")
fails = []


def fail(msg):
    fails.append(msg)


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

# ── the SMP3 seal must pass on this same boot (corroborates progress / results / context) ──
smp3 = [l for l in lines if l.startswith("SMP3_VERDICT ")]
if not smp3:
    fail("no SMP3 verdict on this boot")
elif not any("result=ok" in l for l in smp3):
    fail("the SMP3 seal did not pass: " + smp3[-1][:120])

# ── metadata ──
meta = next((l for l in lines if l.startswith("LOCK1_META ")), None)
vm_lock_id = None
if not meta:
    fail("no LOCK1_META line")
else:
    mo = re.search(r"overflow=(\d+)", meta)
    if mo and int(mo.group(1)) != 0:
        fail("the LOCK1 record overflowed its ring")
    ml = re.search(r"vm_lock_id=(\d+)", meta)
    vm_lock_id = int(ml.group(1)) if ml else None
    if vm_lock_id != VM_LOCK_ID:
        fail("LOCK1_META names vm_lock_id=%s, not the expected %d" % (vm_lock_id, VM_LOCK_ID))

# ── parse LOCK1 records; dedup identical dump-pass copies; reject conflicting copies of one seq ──
rx = re.compile(
    r"^(LOCK1_REC seq=(\d+) kind=(\w+) hart=(\d+) "
    r"f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+))"
    r" pass=\d+ crc=0x([0-9a-f]+)$"
)
recs = {}
conflict = False
for l in lines:
    mm = rx.match(l.strip())
    if not mm:
        continue
    body = mm.group(1)
    if fnv1a(body) != int(mm.group(10), 16):
        continue  # torn copy; the other pass carries an intact one
    seq = int(mm.group(2))
    rec = dict(
        seq=seq,
        kind=mm.group(3),
        hart=int(mm.group(4)),
        f=[int(mm.group(i), 16) for i in range(5, 10)],
        body=body,
    )
    if seq in recs:
        if recs[seq]["body"] != body:
            conflict = True
            fail("conflicting checksum-valid copies of seq %d" % seq)
    else:
        recs[seq] = rec
ev = [recs[k] for k in sorted(recs)]
for e in ev:
    if e["kind"] not in KINDS:
        fail("unknown event kind %r at seq %d" % (e["kind"], e["seq"]))


def round_of(e):
    # Lock events carry (lock_id, round, ...); ipi/arrival carry (round, target, ...).
    return e["f"][1] if e["kind"] in LOCK_KINDS else e["f"][0]


# ── per-round validation ───────────────────────────────────────────────────────────────────
credited = {0: 0, 1: 0}
ssip_rounds = 0
arrival_rounds = 0


def round_fail(r, holder, msg):
    fail("round %d (holder hart %d): %s" % (r, holder, msg))


for r in range(1, ROUNDS + 1):
    holder = 0 if r % 2 == 1 else 1
    waiter = 1 - holder
    er = [e for e in ev if round_of(e) == r]
    if not er:
        continue  # a round with no records at all contributes nothing; coverage is checked below

    # Every lock event of this round must name the one VM lock.
    wrong = [e for e in er if e["kind"] in LOCK_KINDS and e["f"][0] != VM_LOCK_ID]
    if wrong:
        round_fail(r, holder, "a lock event names lock %d, not the VM lock %d"
                   % (wrong[0]["f"][0], VM_LOCK_ID))
        continue

    holds = [e for e in er if e["kind"] == "hold" and e["hart"] == holder and e["f"][4] == holder]
    contended = [e for e in er if e["kind"] == "contended"]
    is_contended = bool(contended)

    if not is_contended:
        # A legitimately uncontended attempt: require a benign, complete history, else fail.
        bad_hold = [e for e in er if e["kind"] == "hold" and e["f"][2] != 0]
        if bad_hold:
            round_fail(r, holder, "an uncontended round's holder was not masked (SIE set)")
            continue
        for h in (holder, waiter):
            acq = [e for e in er if e["kind"] == "acquire" and e["hart"] == h]
            rel = [e for e in er if e["kind"] == "release" and e["hart"] == h]
            for a in acq:
                if not any(x["seq"] > a["seq"] for x in rel):
                    round_fail(r, holder, "hart %d acquired without a following release" % h)
                    break
        continue

    # ── a contended round: the complete ownership-interval chain is mandatory ──
    if len(holds) != 1:
        round_fail(r, holder, "expected exactly one holder hold record, found %d" % len(holds))
        continue
    hold = holds[0]
    if hold["f"][2] != 0:
        round_fail(r, holder, "holder was not masked (sstatus.SIE set) inside the critical section")
        continue
    # The contention must be the waiter observing THIS holder.
    con = [e for e in contended if e["hart"] == waiter and e["f"][2] == holder]
    if not con:
        round_fail(r, holder, "contention was not recorded by the waiter observing this holder")
        continue
    # Holder acquisition that owns this hold: the holder acquire immediately preceding it.
    acq_h = [e for e in er if e["kind"] == "acquire" and e["hart"] == holder
             and e["f"][2] == holder and e["seq"] < hold["seq"]]
    if not acq_h:
        round_fail(r, holder, "no holder acquisition precedes the hold")
        continue
    a_h = max(acq_h, key=lambda e: e["seq"])
    # The hold must belong to that acquisition: the holder must still own the lock at the hold, with
    # no release between — otherwise this is an unrelated earlier acquisition (e.g. the guard-page
    # query) and the real owning acquire is missing.
    if any(e["kind"] == "release" and e["hart"] == holder and a_h["seq"] < e["seq"] < hold["seq"]
           for e in er):
        round_fail(r, holder, "the hold is not covered by an un-released holder acquisition")
        continue
    # The observed contention falls inside this ownership interval (after acquire, up to the hold).
    c = [e for e in con if a_h["seq"] < e["seq"] <= hold["seq"]]
    if not c:
        round_fail(r, holder, "the contention was not observed during this ownership interval")
        continue
    c = min(c, key=lambda e: e["seq"])
    # Holder release-intent after the hold (its actual unlock follows; the handover is proven by the
    # contender's own successful acquisition below, not by assuming this marker is the atomic unlock).
    rel_h = [e for e in er if e["kind"] == "release" and e["hart"] == holder and e["seq"] > hold["seq"]]
    if not rel_h:
        round_fail(r, holder, "holder never released after the hold")
        continue
    r_h = min(rel_h, key=lambda e: e["seq"])
    # The production IPI was published to this holder, before the holder left the interval.
    ipi = [e for e in er if e["kind"] == "ipi" and e["f"][1] == holder and e["f"][2] == 1
           and e["hart"] == waiter and e["seq"] < r_h["seq"]]
    if not ipi:
        round_fail(r, holder, "no production IPI was published to the holder during the interval")
        continue
    ipi = min(ipi, key=lambda e: e["seq"])
    # Handover: the contender acquires after the holder's release-intent, then releases after that.
    acq_w = [e for e in er if e["kind"] == "acquire" and e["hart"] == waiter and e["seq"] > r_h["seq"]]
    if not acq_w:
        round_fail(r, holder, "the contender never acquired after the holder released")
        continue
    a_w = min(acq_w, key=lambda e: e["seq"])
    rel_w = [e for e in er if e["kind"] == "release" and e["hart"] == waiter and e["seq"] > a_w["seq"]]
    if not rel_w:
        round_fail(r, holder, "the contender never released after it acquired")
        continue

    credited[waiter] += 1

    # §3 delivery: the masked holder saw it pending, then consumed it after the interval.
    if hold["f"][3] == 1:
        ssip_rounds += 1
    arr = [e for e in er if e["kind"] == "arrival" and e["hart"] == holder
           and e["f"][1] == holder and e["seq"] > r_h["seq"]]
    if arr and hold["f"][3] == 1:
        arrival_rounds += 1

if credited[1] < MIN_PER_DIRECTION:
    fail("contended rounds with S (hart 1) waiting: %d, need >= %d"
         % (credited[1], MIN_PER_DIRECTION))
if credited[0] < MIN_PER_DIRECTION:
    fail("contended rounds with C (hart 0) waiting: %d, need >= %d"
         % (credited[0], MIN_PER_DIRECTION))
if ssip_rounds < MIN_SSIP_ROUNDS:
    fail("rounds that observed the IPI pending (sip.SSIP) under the mask: %d, need >= %d"
         % (ssip_rounds, MIN_SSIP_ROUNDS))
if arrival_rounds < MIN_ARRIVAL_ROUNDS:
    fail("rounds whose pending IPI was consumed after unmask (arrival): %d, need >= %d"
         % (arrival_rounds, MIN_ARRIVAL_ROUNDS))

for f in fails:
    print("[lock1-witness][fail] " + f)
ok = not fails
print(
    "LOCK1_WITNESS_SEAL rounds=%d credited_c_waits=%d credited_s_waits=%d ssip_rounds=%d "
    "arrival_rounds=%d result=%s"
    % (ROUNDS, credited[0], credited[1], ssip_rounds, arrival_rounds, "ok" if ok else "fail")
)
sys.exit(0 if ok else 1)
