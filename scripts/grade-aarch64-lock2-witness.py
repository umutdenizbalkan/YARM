#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK2 — the independent grader for the AArch64 contention witness on the production VM
# address-space lock, with the reschedule SGI deferred through the holder's masked hold.
#
# It re-derives every round from the raw sealed `LOCK2_REC` lines (never from the kernel's summary).
# The architecture-neutral half — transport accounting, the same-boot SMP2 seal, the ownership model
# and contention credited by value — is `lock_witness_core`, shared with the LOCK1 grader. On top of
# it, this grader holds the SGI chain to the GICv2's own facts:
#
#   * PUBLICATION: the waiter's production `send_reschedule_sgi` to the holder (the `GICD_SGIR` value
#     written: target list = the holder's interface, INTID = the reschedule SGI), recorded strictly
#     inside the holder's masked acquisition — after the holder's acquire record — and before the hold
#     record that observed the waiter's contention.
#   * OUTSTANDING UNDER THE MASK: the holder's two reads of its own banked `GICD_SPENDSGIR` byte inside
#     that acquisition: the waiter's source bit CLEAR when the hold began and SET after the
#     contention, with the SGI not active. The controller held a request from that source that was
#     made during this hold. Repeat sends from the same source coalesce into the one pending bit, so
#     the discharge covers all of them; one physical interrupt per publication is never assumed.
#   * CLAIM: the first `GICC_IAR` token on the holder after arming whose INTID is the reschedule SGI
#     and whose source field is the waiter's interface. It must follow the release record of that
#     acquisition (release intent -> unlocking store -> the guard's IRQ restore, a no-op inside an
#     exception -> ERET to EL0 or the idle window's `daifclr` -> the IRQ exception -> the claim), and
#     no claim by the holder may fall inside the acquisition.
#   * COMPLETION: exactly one `GICC_EOIR` write of exactly that token on the holder before its next
#     claim; and per CPU, every non-special claim since arming matched by one completion.
#
# A round whose controller views do not show the waiter's request appearing under the mask is a
# valid UNCREDITED round; absent, contradictory or out-of-order evidence fails the boot. The SMP2
# seal on the same boot (results, preserved context, its own SGI send/arrival/completion
# population, the broadcast-invalidation contract) corroborates progress; SGIs get no credit for the
# invalidation, which SMP2 grades on its own.
#
# Usage: grade-aarch64-lock2-witness.py <boot.log> <artifact-identity>
#        grade-aarch64-lock2-witness.py --self-test
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lock_witness_core import (  # noqa: E402
    check_events, contention_credit, contention_outside_own, corroborating_seal, emit, fnv1a,
    inside, ownership, rec_body, transport)

VM_LOCK_ID = 1
ROUNDS = 12
SGI = 1  # the reschedule SGI's INTID (arch::gicv2_sgi::RESCHEDULE_SGI_INTID)
MIN_PER_DIRECTION = 4  # credited contended rounds, and attributed SGI deliveries, per direction
KINDS = {"acquire", "contended", "release", "hold", "sgi", "claim", "pending", "gate", "done",
         "linklost", "complete"}
LOCK_KINDS = {"acquire", "contended", "release", "hold"}
IFACE = {0: 0x1, 1: 0x2}  # the fixtures' published interface bits (the live ones come from META)


# ── synthetic fixtures (self-test) ───────────────────────────────────────────────────────────
def _view(sources=0, active=0, pending=None):
    pending = (1 if sources else 0) if pending is None else pending
    return sources | (active << 8) | (pending << 9) | (1 << 10)


def _round(r, ids, k, variant):
    """One realistic round, in the order a live boot records it. Returns [(kind, hart, f)]."""
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    a, b, c, d = ids
    m = IFACE[W]
    token = (W << 10) | SGI
    sgir = (IFACE[H] << 16) | SGI
    ev = [
        ("gate", H, [r, H, 0, 1, 1]),
        ("acquire", H, [1, r, a, 0, 0]),
        ("gate", W, [r, H, 1, 1, 0]),
        ("sgi", W, [r, H, 1, sgir, r]),
        ("contended", W, [1, r, k, 0, 0]),
        ("hold", H, [1, r, a, k, (k - 1) << 8]),
        ("pending", H, [r, W, _view() | (_view(m) << 16) | (m << 32), 1, a]),
        ("release", H, [1, r, a, 0, 0]),
        ("acquire", W, [1, r, b, 0, 0]), ("release", W, [1, r, b, 0, 0]),
        ("acquire", W, [1, r, c, 0, 0]), ("release", W, [1, r, c, 0, 0]),
        ("acquire", H, [1, r, d, 0, 0]), ("release", H, [1, r, d, 0, 0]),
    ]
    if variant == "interleaved":
        # A timer claim and the holder's own (self-sourced) SGI are taken first; neither discharges.
        ev += [("claim", H, [r, 30, m, 0, 1]), ("claim", H, [r, (H << 10) | SGI, m, 0, 0])]
    claim = [("claim", H, [r, token, m, 1, 0]), ("complete", H, [r, token, token, 0, 0])]
    if variant == "late_claim":
        ev += [("done", W, [r, 0, 0, 0, 0])] + claim + [("done", H, [r, 0, 0, 0, 0])]
    else:
        ev += claim + [("done", H, [r, 0, 0, 0, 0]), ("done", W, [r, 0, 0, 0, 0])]
    return ev


def _counts(logical, coalesced=False):
    """Per-CPU controller counts consistent with the records (plus background timer traffic)."""
    sgi = {0: 0, 1: 0}
    other = {0: 40, 1: 0}
    sent = {0: 0, 1: 0}
    for kind, hart, f in logical:
        if kind == "claim":
            if f[4] == 0:
                sgi[hart] += 1
            elif f[4] == 1:
                other[hart] += 1
        if kind == "sgi":
            sent[hart] += 1
    if coalesced:
        sent[1] += 1  # one extra production send merged into a pending request at the target
    return ["LOCK2_COUNTS cpu=%d sgi_claim=%d other_claim=%d special_claim=0 sgi_eoi=%d "
            "other_eoi=%d special_eoi=0 sgi_sent=%d settled=1"
            % (c, sgi[c], other[c], sgi[c], other[c], sent[c]) for c in (0, 1)]


def _synth(variant=None, mutate=None, omit=None, lines_mut=None, counts_mut=None,
           meta_count=None, conflict=False):
    logical = []
    nid, k = 1, 0
    for r in range(1, ROUNDS + 1):
        k += 1
        logical += [list(e) for e in _round(r, (nid, nid + 1, nid + 2, nid + 3), k, variant)]
        nid += 4
    if mutate:
        logical = mutate(logical)
    omitted = omit(logical) if omit else set()
    n = len(logical)
    meta = ("LOCK2_META vm_lock_id=1 rounds=%d slots_used=%d overflow=0 dump_cpu=1 sgi_intid=%d "
            "if0=0x%x if1=0x%x" % (ROUNDS, n if meta_count is None else meta_count, SGI,
                                   IFACE[0], IFACE[1]))
    counts = _counts(logical, coalesced=variant == "coalesced")
    if counts_mut:
        counts = counts_mut(counts)
    out = ["AARCH64 boot"] + [emit("SMP2_VERDICT result=ok reason=none at=0", p) for p in (1, 2)]
    for p in (1, 2):
        out.append(emit(meta, p))
        for seq, (kind, hart, f) in enumerate(logical):
            if seq not in omitted:
                out.append(emit(rec_body("LOCK2", seq, kind, hart, f), p))
        out += [emit(c, p) for c in counts]
        out.append(emit("LOCK2_DUMP_DONE records=%d" % n, p))
    if conflict:
        kind, hart, f = logical[0]
        out.append(emit(rec_body("LOCK2", 0, kind, hart, [f[0], f[1], f[2], f[3] ^ 1, f[4]]), 2))
    if lines_mut:
        out = lines_mut(out)
    return "\n".join(out)


def _rnd(e):
    k, _, f = e
    return f[1] if k in LOCK_KINDS else f[0]


def _at(r, pred, fn):
    def m(ev):
        for i, e in enumerate(ev):
            if _rnd(e) == r and pred(e):
                return fn(ev, i)
        raise AssertionError("fixture anchor missing")
    return m


def _edit(r, pred, f):
    def fn(ev, i):
        ev[i] = f(list(ev[i][:2]) + [list(ev[i][2])])
        return ev
    return _at(r, pred, fn)


def _drop(r, pred):
    return _at(r, pred, lambda ev, i: ev[:i] + ev[i + 1:])


def _move_after(r, pred, anchor):
    """Move round r's first `pred` event to just after its first `anchor` event."""
    def m(ev):
        i = next(j for j, e in enumerate(ev) if _rnd(e) == r and pred(e))
        e = ev.pop(i)
        j = next(j for j, x in enumerate(ev) if _rnd(x) == r and anchor(x))
        ev.insert(j + 1, e)
        return ev
    return m


def _set(i, v):
    def f(e):
        e[2][i] = v
        return e
    return f


def _all_rounds(make):
    def m(ev):
        for r in range(1, ROUNDS + 1):
            ev = make(r)(ev)
        return ev
    return m


def _last_pair(r):
    def o(ev):
        a = max(i for i, e in enumerate(ev) if e[0] == "acquire" and e[2][1] == r)
        aid = ev[a][2][2]
        rel = next(i for i in range(a + 1, len(ev)) if ev[i][0] == "release" and ev[i][2][2] == aid)
        return {a, rel}
    return o


def _damage(pred):
    def d(out):
        return [l[:-1] + ("0" if l[-1] != "0" else "1") if pred(l) else l for l in out]
    return d


K = lambda kind, hart=None: (lambda e: e[0] == kind and (hart is None or e[1] == hart))
H5, W5 = 0, 1  # round 5's holder / waiter


def _self_test():
    import subprocess
    import tempfile

    def run(log):
        fd, path = tempfile.mkstemp()
        os.write(fd, log.encode())
        os.close(fd)
        idfd, idpath = tempfile.mkstemp()
        os.write(idfd, b"features=aarch64-lock2-witness\n")
        os.close(idfd)
        r = subprocess.run([sys.executable, os.path.abspath(sys.argv[0]), path, idpath],
                           capture_output=True, text=True)
        os.unlink(path)
        os.unlink(idpath)
        return r.returncode, r.stdout

    tok_w5 = (W5 << 10) | SGI
    cases = [
        # ── positives ──
        ("good", _synth(), 0, None),
        ("valid: claim after the waiter's completion record", _synth(variant="late_claim"), 0,
         None),
        ("valid: timer and self-sourced SGI claims first", _synth(variant="interleaved"), 0, None),
        ("valid: coalesced publications (more sends than claims)", _synth(variant="coalesced"), 0,
         None),
        ("valid: one damaged copy recovered", _synth(lines_mut=_damage(
            lambda l: l.startswith("LOCK2_REC seq=40 ") and " pass=2 " in l)), 0, None),
        ("valid: one uncontended, undelivered round (no credit)", _synth(mutate=lambda ev: _edit(
            3, K("pending"), lambda e: [e[0], e[1], [e[2][0], e[2][1], _view() | (_view() << 16) |
                                                       (IFACE[1] << 32), 0, e[2][4]]])(
            _edit(3, K("hold"), _set(3, 0))(_drop(3, K("complete"))(_drop(3, K("claim"))(ev))))),
         0, None),
        # ── the corrected LOCK1 controls, on the shared core ──
        ("dropped acquire/release pair (both copies)", _synth(omit=_last_pair(5)), 1,
         "missing records"),
        ("overlapping owners", _synth(mutate=_at(5, K("hold"), lambda ev, i: ev[:i + 1] + [
            ["acquire", W5, [1, 5, 999, 0, 0]]] + ev[i + 1:])), 1, "overlapping owners"),
        ("release without acquisition", _synth(mutate=_at(5, K("gate"), lambda ev, i: ev[:i] + [
            ["release", W5, [1, 5, 998, 0, 0]]] + ev[i:])), 1,
         "release with no preceding acquisition"),
        ("deleted scheduled round", _synth(mutate=lambda ev: [e for e in ev if _rnd(e) != 7]), 1,
         "has no complete record"),
        ("conflicting checksum-valid copies", _synth(conflict=True), 1, "conflicting"),
        ("metadata and completion counts disagree", _synth(meta_count=7), 1,
         "inconsistent counts"),
        ("holder not masked", _synth(mutate=_edit(7, K("hold"), _set(4, 1))), 1, "not masked"),
        ("insufficient genuine contention", _synth(mutate=_all_rounds(
            lambda r: _edit(r, K("hold"), _set(3, 0)))), 1, "contended rounds"),
        ("hold value no waiter produced", _synth(mutate=_edit(5, K("contended"), _set(2, 77))), 1,
         "no waiter contention record produced"),
        ("SMP2 seal missing", _synth(lines_mut=lambda out: [
            l for l in out if not l.startswith("SMP2_VERDICT")]), 1, "no intact SMP2 verdict"),
        # ── AArch64 SGI chain controls ──
        ("wrong sender (claim sourced by the holder itself)", _synth(mutate=_edit(
            5, K("claim"), lambda e: [e[0], e[1], [5, (H5 << 10) | SGI, IFACE[W5], 0, 0]])), 1,
         "never discharged"),
        ("wrong target (the waiter records the claim)", _synth(mutate=_edit(
            5, K("claim"), lambda e: [e[0], W5, e[2]])), 1, "never armed"),
        ("wrong SGI (another INTID discharges)", _synth(mutate=_edit(
            5, K("claim"), lambda e: [e[0], e[1], [5, (W5 << 10) | 2, IFACE[W5], 1, 0]])), 1,
         "is not the reschedule SGI from the linked source"),
        ("mismatched acknowledgement token", _synth(mutate=_edit(
            5, K("complete"), lambda e: [e[0], e[1], [5, (H5 << 10) | SGI, tok_w5, 0, 0]])), 1,
         "mismatched acknowledgement token"),
        ("stale round (claim names an earlier round)", _synth(mutate=_edit(
            5, K("claim"), _set(0, 3))), 1, "stale"),
        ("claim naming a never-armed round", _synth(mutate=_edit(5, K("claim"), _set(0, 4))), 1,
         "never armed"),
        ("premature arrival (claim inside the masked acquisition)", _synth(mutate=_move_after(
            5, K("claim"), K("pending"))), 1, "inside the masked acquisition"),
        ("claim between the hold acquisition's release and pending", _synth(mutate=lambda ev: (
            _move_after(5, K("complete"), K("claim"))(_move_after(5, K("claim"), K("pending"))(ev)))),
         1, "inside the masked acquisition"),
        ("missing completion", _synth(mutate=_drop(5, K("complete"))), 1, "completion"),
        ("duplicate completion", _synth(mutate=_at(5, K("complete"), lambda ev, i: ev[:i + 1] + [
            ev[i]] + ev[i + 1:])), 1, "completion"),
        ("unbalanced controller obligations", _synth(counts_mut=lambda c: [
            c[0].replace("sgi_eoi=6", "sgi_eoi=5"), c[1]]), 1, "unbalanced"),
        ("other CPU never settled before the dump", _synth(counts_mut=lambda c: [
            c[0], c[1].replace("settled=1", "settled=0")]), 1, "settled"),
        ("publication before the holder's acquisition", _synth(mutate=_move_after(
            5, K("sgi"), lambda e: e[0] == "gate" and e[1] == H5)), 1,
         "outside the holder's masked acquisition"),
        ("publication to the wrong target", _synth(mutate=_edit(5, K("sgi"), _set(1, W5))), 1,
         "no production publication"),
        ("publication refused", _synth(mutate=_edit(5, K("sgi"), _set(2, 0))), 1,
         "no production publication"),
        ("request already pending when the hold began", _synth(mutate=_edit(
            5, K("pending"), lambda e: [e[0], e[1], [5, W5, _view(IFACE[W5]) | (
                _view(IFACE[W5]) << 16) | (IFACE[W5] << 32), 1, e[2][4]]])), 1,
         "contradicts its controller views"),
        ("pending names the wrong source interface", _synth(mutate=_edit(
            5, K("pending"), lambda e: [e[0], e[1], [5, W5, e[2][2] & 0xffffffff | (
                IFACE[H5] << 32), 1, e[2][4]]])), 1, "source interface"),
        ("missing publication", _synth(mutate=_drop(5, K("sgi"))), 1, "no production publication"),
        ("overwritten unresolved link", _synth(mutate=_at(5, K("pending"), lambda ev, i: ev[:i + 1] + [
            ["linklost", H5, [3, 5, 0, 0, 0]]] + ev[i + 1:])), 1, "overwrote"),
        ("suppressed SGI on every round (contention only)", _synth(mutate=_all_rounds(
            lambda r: (lambda ev: _edit(r, K("pending"), lambda e: [e[0], e[1], [
                r, e[2][1], _view() | (_view() << 16) | (e[2][2] >> 32 << 32), 0, e[2][4]]])(
                _drop(r, K("complete"))(_drop(r, K("claim"))(_drop(r, K("sgi"))(ev))))))), 1,
         "attributed SGI deliveries"),
    ]
    bad = 0
    for name, log, want, why in cases:
        rc, out = run(log)
        ok = rc == want and (why is None or why in out)
        if not ok:
            bad += 1
        print("[self-test] %-58s rc=%d want=%d %s" % (name, rc, want, "PASS" if ok else "FAIL"))
        if not ok:
            print("            expected reason %r; got:\n%s" % (why, out[-700:]))
    print("[self-test] %s (%d cases)" % ("ALL PASS" if not bad else "FAILURES", len(cases)))
    sys.exit(1 if bad else 0)


if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
    _self_test()

# ── inputs ───────────────────────────────────────────────────────────────────────────────────
LOG = sys.argv[1]
IDENT = sys.argv[2] if len(sys.argv) > 2 else None
lines = open(LOG, errors="replace").read().split("\n")
fails = []


def fail(msg):
    fails.append(msg)


def round_fail(r, msg):
    fail("round %d: %s" % (r, msg))


ident = open(IDENT).read() if IDENT and os.path.exists(IDENT) else ""
if "features=aarch64-lock2-witness" not in ident:
    fail("the artifact identity does not name the aarch64-lock2-witness build")

corroborating_seal(lines, "SMP2_VERDICT", fail)
META_RX = re.compile(r"^LOCK2_META vm_lock_id=(?P<lock>\d+) rounds=(?P<rounds>\d+) "
                     r"slots_used=(?P<slots_used>\d+) overflow=(?P<overflow>\d+) "
                     r"dump_cpu=(?P<dump_cpu>\d+) sgi_intid=(?P<intid>\d+) "
                     r"if0=0x(?P<if0>[0-9a-f]+) if1=0x(?P<if1>[0-9a-f]+)$")
COUNTS_RX = re.compile(r"^LOCK2_COUNTS cpu=(?P<cpu>\d+) sgi_claim=(?P<sc>\d+) "
                       r"other_claim=(?P<oc>\d+) special_claim=(?P<pc>\d+) sgi_eoi=(?P<se>\d+) "
                       r"other_eoi=(?P<oe>\d+) special_eoi=(?P<pe>\d+) sgi_sent=(?P<sent>\d+) "
                       r"settled=(?P<settled>\d+)$")
meta, recs, sums = transport(lines, "LOCK2", META_RX, fail, summary=(COUNTS_RX, "cpu"))
iface = {}
if meta is not None:
    if int(meta.group("lock")) != VM_LOCK_ID:
        fail("LOCK2_META names vm_lock_id=%s, not %d" % (meta.group("lock"), VM_LOCK_ID))
    if int(meta.group("rounds")) != ROUNDS:
        fail("LOCK2_META schedules %s rounds, not %d" % (meta.group("rounds"), ROUNDS))
    if int(meta.group("dump_cpu")) not in (0, 1):
        fail("LOCK2_META names dump CPU %s" % meta.group("dump_cpu"))
    if int(meta.group("intid")) != SGI:
        fail("LOCK2_META names SGI %s, not the reschedule SGI %d" % (meta.group("intid"), SGI))
    iface = {0: int(meta.group("if0"), 16), 1: int(meta.group("if1"), 16)}
    if (any(m == 0 or m & (m - 1) or m > 0xff for m in iface.values())
            or iface[0] == iface[1]):
        fail("LOCK2_META interface masks %s are not two distinct single bits" % iface)
cpu_of_iface = {m: c for c, m in iface.items()}

# ── controller obligations, per CPU (claims and completions since arming) ──
requests = claims_sgi = eois_sgi = claims_other = 0
for cpu in ("0", "1"):
    body = sums.get(cpu)
    if body is None:
        fail("no intact LOCK2_COUNTS line for CPU %s" % cpu)
        continue
    c = {k: int(v) for k, v in COUNTS_RX.match(body).groupdict().items()}
    if c["settled"] != 1:
        fail("CPU %s had not settled its claimed interrupt when the dump was taken" % cpu)
    if c["sc"] != c["se"] or c["oc"] != c["oe"] or c["pe"] != 0:
        fail("unbalanced controller obligations on CPU %s: SGI claims %d / completions %d, other "
             "claims %d / completions %d, special completions %d"
             % (cpu, c["sc"], c["se"], c["oc"], c["oe"], c["pe"]))
    requests += c["sent"]
    claims_sgi += c["sc"]
    eois_sgi += c["se"]
    claims_other += c["oc"]

ev = [recs[k] for k in sorted(recs)]
check_events(ev, KINDS, (0, 1), VM_LOCK_ID, fail)
by = lambda kind: [e for e in ev if e["kind"] == kind]
acqs = ownership(ev, ROUNDS, fail)
contention_outside_own(ev, acqs, ROUNDS, fail)
for lk in by("linklost"):
    fail("seq %d: CPU %d overwrote round %d's unresolved delivery link"
         % (lk["seq"], lk["hart"], lk["f"][0]))


def views(p):
    f2 = p["f"][2]
    return f2 & 0xffff, (f2 >> 16) & 0xffff, f2 >> 32


def outstanding(pre, post, mask):
    valid = (pre >> 10) & (post >> 10) & 1
    return int(valid == 1 and mask != 0 and not pre & mask and bool(post & mask)
               and not (post >> 8) & 1)


# ── per-round obligations ────────────────────────────────────────────────────────────────────
credited = {0: 0, 1: 0}
delivered = {0: 0, 1: 0}
armed = {}  # (holder, round) -> (pending record, source mask, acquisition id)
uncredited = []
for r in range(1, ROUNDS + 1):
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    g_h = [e for e in by("gate") if e["f"][0] == r and e["hart"] == H and e["f"][2] == 0]
    g_w = [e for e in by("gate") if e["f"][0] == r and e["hart"] == W and e["f"][2] == 1]
    gates = [e for e in by("gate") if e["f"][0] == r]
    dones = [e for e in by("done") if e["f"][0] == r]
    holds = [e for e in by("hold") if e["f"][1] == r]
    pends = [e for e in by("pending") if e["f"][0] == r]
    if (len(g_h) != 1 or len(g_w) != 1 or len(gates) != 2
            or sorted(d["hart"] for d in dones) != [0, 1] or any(g["f"][1] != H for g in gates)):
        round_fail(r, "has no complete record (gates %d, dones %s)"
                   % (len(gates), sorted(d["hart"] for d in dones)))
        continue
    if len(holds) != 1 or holds[0]["hart"] != H or len(pends) != 1 or pends[0]["hart"] != H:
        round_fail(r, "has no complete record (expected one hold and one pending observation by the "
                   "designated holder %d, found %d / %d)" % (H, len(holds), len(pends)))
        continue
    hold, pend = holds[0], pends[0]
    aid = hold["f"][2]
    if hold["f"][4] & 1:
        round_fail(r, "holder was not masked (PSTATE.I clear) inside the critical section")
        continue
    if not inside(acqs, hold, aid) or acqs[aid]["round"] != r:
        round_fail(r, "the hold falls outside its acquisition %d" % aid)
        continue
    if pend["f"][4] != aid or not inside(acqs, pend, aid) or pend["f"][1] != W:
        round_fail(r, "the pending observation is not the holder's, inside acquisition %d" % aid)
        continue
    got_credit, why = contention_credit(r, W, hold, ev, acqs)
    if why:
        round_fail(r, why)
        continue
    contended = bool(got_credit and g_w[0]["f"][3] == 1)
    if contended:
        credited[W] += 1

    pre, post, mask = views(pend)
    if iface and mask != iface.get(W):
        round_fail(r, "the pending observation's source interface 0x%x is not the waiter's 0x%x"
                   % (mask, iface.get(W, 0)))
        continue
    out = outstanding(pre, post, mask)
    if pend["f"][3] != out:
        round_fail(r, "the masked holder's outstanding flag %d contradicts its controller views "
                   "(at hold start 0x%x, after contention 0x%x, source 0x%x)"
                   % (pend["f"][3], pre, post, mask))
        continue
    if not out:
        uncredited.append(r)
        continue
    armed[(H, r)] = (pend, mask, aid)
    if not contended:
        uncredited.append(r)
        continue
    a = acqs[aid]
    sgir = (iface.get(H, IFACE[H]) << 16) | SGI
    pubs = [e for e in by("sgi") if e["f"][0] == r and e["hart"] == W and e["f"][1] == H
            and e["f"][2] == 1 and e["f"][3] == sgir]
    if len(pubs) != 1:
        round_fail(r, "no production publication by the waiter to the holder (GICD_SGIR 0x%x)"
                   % sgir)
        continue
    p = pubs[0]
    if not a["a"] < p["seq"] < hold["seq"] or p["seq"] < g_w[0]["seq"]:
        round_fail(r, "the publication (seq %d) lies outside the holder's masked acquisition before "
                   "its hold (acquire seq %d, hold seq %d)" % (p["seq"], a["a"], hold["seq"]))
        continue
    delivered[W] += 1

# ── claims and completions: every one must belong to an armed link, in order ──
for c in by("claim") + by("complete"):
    if (c["hart"], c["f"][0]) not in armed:
        fail("seq %d: a %s on CPU %d names round %d's link, which was never armed there"
             % (c["seq"], c["kind"], c["hart"], c["f"][0]))
for (h, r), (pend, mask, aid) in sorted(armed.items(), key=lambda kv: kv[1][0]["seq"]):
    a = acqs[aid]
    mine = [e for e in ev if e["hart"] == h and e["kind"] in ("claim", "complete")
            and e["f"][0] == r]
    # A link lives from its arming to this CPU's next arming; a record naming it outside that window
    # is stale (the kernel names only the CPU's current link).
    rearm = min([p["seq"] for (hh, _), (p, _, _) in armed.items()
                 if hh == h and p["seq"] > pend["seq"]] or [1 << 62])
    stale = [e for e in mine if not pend["seq"] < e["seq"] < rearm]
    if stale:
        round_fail(r, "a stale %s (seq %d) names round %d's link outside its lifetime on CPU %d "
                   "(armed at seq %d, re-armed at seq %s)"
                   % (stale[0]["kind"], stale[0]["seq"], r, h, pend["seq"],
                      rearm if rearm < 1 << 62 else "never"))
        continue
    for e in mine:
        if e["seq"] < a["r"]:
            round_fail(r, "a %s (seq %d) lies inside the masked acquisition %d — before its release "
                       "record (seq %d)" % (e["kind"], e["seq"], aid, a["r"]))
            break
    else:
        claims = [e for e in mine if e["kind"] == "claim"]
        dis = [e for e in claims if e["f"][3] == 1]
        if not dis:
            round_fail(r, "the armed delivery link was never discharged (no claim of the reschedule "
                       "SGI from the waiter's interface)")
            continue
        if len(dis) > 1:
            round_fail(r, "the link was discharged more than once")
            continue
        d = dis[0]
        tok = d["f"][1]
        if (tok & 0x3ff) != SGI or (1 << ((tok >> 10) & 7)) != mask or d["f"][4] != 0:
            round_fail(r, "the discharging claim (token 0x%x) is not the reschedule SGI from the "
                       "linked source 0x%x" % (tok, mask))
            continue
        early = [e for e in claims if e["seq"] < d["seq"] and e["f"][4] == 0
                 and (1 << ((e["f"][1] >> 10) & 7)) == mask]
        if early:
            round_fail(r, "an earlier claim of the linked source (seq %d) did not discharge"
                       % early[0]["seq"])
            continue
        nxt = [e["seq"] for e in ev if e["hart"] == h and e["kind"] == "claim"
               and e["seq"] > d["seq"]]
        bound = min(nxt) if nxt else 1 << 62
        comps = [e for e in mine if e["kind"] == "complete" and d["seq"] < e["seq"] < bound]
        stray = [e for e in mine if e["kind"] == "complete" and not d["seq"] < e["seq"] < bound]
        if len(comps) != 1 or stray:
            round_fail(r, "the discharging claim has %d completion(s) before CPU %d's next claim "
                       "(%d elsewhere); exactly one is owed" % (len(comps), h, len(stray)))
            continue
        cp = comps[0]
        if cp["f"][1] != tok or cp["f"][2] != tok:
            round_fail(r, "mismatched acknowledgement token: completed 0x%x for the claim of 0x%x"
                       % (cp["f"][1], tok))
            continue

for d, who in ((1, "S (CPU 1)"), (0, "C (CPU 0)")):
    if credited[d] < MIN_PER_DIRECTION:
        fail("contended rounds with %s waiting: %d, need >= %d"
             % (who, credited[d], MIN_PER_DIRECTION))
    if delivered[d] < MIN_PER_DIRECTION:
        fail("attributed SGI deliveries with %s publishing: %d, need >= %d"
             % (who, delivered[d], MIN_PER_DIRECTION))

for f in fails:
    print("[lock2-witness][fail] " + f)
ok = not fails
print("LOCK2_WITNESS_SEAL rounds=%d credited_c_waits=%d credited_s_waits=%d delivered_c=%d "
      "delivered_s=%d uncredited=%s sgi_requests=%d sgi_claims=%d sgi_completions=%d "
      "other_claims=%d result=%s"
      % (ROUNDS, credited[0], credited[1], delivered[0], delivered[1],
         ",".join(map(str, uncredited)) or "none", requests, claims_sgi, eois_sgi, claims_other,
         "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
