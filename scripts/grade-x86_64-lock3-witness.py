#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK3 — the independent grader for the x86_64 contention witness on the production VM
# address-space lock, with the reschedule IPI deferred through the holder's masked hold.
#
# It re-derives every round from the raw sealed `LOCK3_REC` lines (never from a kernel summary). The
# architecture-neutral half — transport accounting, the ownership model, contention credited by value
# — is `lock_witness_core`, shared with the LOCK1 and LOCK2 graders. On top of it, this grader holds
# the IPI chain to the local APIC's and the ICR writer's own facts:
#
#   * PUBLICATION: the waiter's production reschedule send to the holder — the one ICR writer's record
#     immediately before the waiter's publication record: destination = the holder's APIC id, low word
#     exactly 0xF1 (fixed delivery, physical, no shorthand, the reschedule vector), the holder's TLB
#     request generation unchanged — strictly inside the holder's masked acquisition, after its hold
#     began and before the observation that saw the request pending; and no other 0xF1 ICR write to the
#     holder inside that window (the IRR has no source field: the ICR record is the attribution).
#   * OUTSTANDING UNDER THE MASK: the holder's two LAPIC views inside that acquisition — the 0xF1 IRR
#     bit CLEAR (and not in service) when the hold began, SET and not in service after the contention,
#     with no TLB request published to it in between — and RFLAGS.IF clear at the hold.
#   * HARDWARE ENTRY: the first 0xF1 handler entry on the holder after that observation (the IRR bit
#     is consumed only by delivery, and later sends coalesce into it until then). It must follow the
#     acquisition's release record (release intent -> unlocking store -> the guard's IF restore, a
#     no-op inside the syscall -> sysretq/iretq restores the user IF -> the interrupt), no handler entry
#     on the holder may fall inside the acquisition, it must show 0xF1 in service, and it must have
#     interrupted ring 3 at exactly the holder's mapping-syscall continuation — before the holder's
#     round-done record (stale arrivals are thereby excluded).
#   * COMPLETION: exactly one EOI by the same handler for that entry (same arrival ordinal, 0xF1 no
#     longer in service), and per CPU every handler entry since arming paired with exactly one EOI,
#     ordinals contiguous up to the production arrival count.
#   * PROGRESS: the holder's round-done record — issued by its program only after the mapping result,
#     its callee-saved registers and its FXSAVE image were checked on that interrupted continuation —
#     after the EOI.
#
# TLB work is never credited to the reschedule: an entry that found a TLB request outstanding is
# counted separately, and remote invalidation is graded by the SMP1 grader from ACK generations.
#
# Usage: grade-x86_64-lock3-witness.py <boot.log> <smp1-seal> <artifact-identity>
#        grade-x86_64-lock3-witness.py --self-test
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lock_witness_core import (  # noqa: E402
    check_events, contention_credit, contention_outside_own, emit, inside, ownership, rec_body,
    transport)

VM_LOCK_ID = 1
ROUNDS = 12
VECTOR = 0xF1
VBIT = 1 << (VECTOR % 32)
MIN_PER_DIRECTION = 4  # credited contended rounds, and attributed deliveries, per direction
KINDS = {"acquire", "contended", "release", "hold", "ipi", "entry", "pending", "gate", "done",
         "eoi", "begin", "icr"}
LOCK_KINDS = {"acquire", "contended", "release", "hold"}
CONT = {"s_gate": 0x200005f7, "s_nr3": 0x2000065f, "c_gate": 0x200005f1, "c_nr3": 0x20000659}


# ── synthetic fixtures (self-test) ───────────────────────────────────────────────────────────
class _Fix:
    """Realistic records: per-CPU arrival ordinals and TLB generations evolve as on a live boot."""

    def __init__(self):
        self.ev = []
        self.ordinal = {0: 1, 1: 3}  # CPU 1 took arrivals before arming
        self.tlb = {0: 2, 1: 2}
        self.ack = {0: 2, 1: 2}

    def add(self, kind, hart, f):
        self.ev.append([kind, hart, list(f)])

    def arrive(self, cpu, rip, origin=2):
        self.ordinal[cpu] += 1
        n = self.ordinal[cpu]
        tlb = int(self.tlb[cpu] != self.ack[cpu])
        req, ack = self.tlb[cpu], self.ack[cpu]
        self.ack[cpu] = self.tlb[cpu]
        self.add("entry", cpu, [origin | (tlb << 8), rip, req | (ack << 32), VBIT, n])
        self.add("eoi", cpu, [0, n, 0, 0, 0])

    def tlb_send(self, sender, target, serviced_in_wait=True):
        self.tlb[target] += 1
        self.add("icr", sender, [target, VECTOR, self.tlb[target], 0, 0])
        if serviced_in_wait:
            self.ack[target] = self.tlb[target]


def _nr3(cpu):
    return CONT["c_nr3"] if cpu == 0 else CONT["s_nr3"]


def _round(fx, r, ids, k, variant):
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    a, b, c, d = ids
    t = fx.tlb[H]
    fx.add("gate", H, [r, H, 0, 1, 1])
    fx.add("acquire", H, [1, r, a, 0, 0])
    fx.add("begin", H, [r, a, 0, t, W])
    fx.add("gate", W, [r, H, 1, 1, 1])
    fx.add("icr", W, [H, VECTOR, t, 0, 0])
    fx.add("ipi", W, [r, H, VECTOR, 1, 0])
    fx.add("contended", W, [1, r, k, 0, 0])
    fx.add("hold", H, [1, r, a, k, (k - 1) << 8])
    fx.add("pending", H, [r, W, VBIT << 32, t | (1 << 32), a])
    fx.add("release", H, [1, r, a, 0, 0])
    fx.add("acquire", W, [1, r, b, 0, 0])
    fx.add("release", W, [1, r, b, 0, 0])
    fx.add("acquire", W, [1, r, c, 0, 0])
    fx.add("release", W, [1, r, c, 0, 0])
    # The waiter's shootdown to the holder coalesces into the pending 0xF1 (answered in the holder's
    # own ACK wait, or — `tlb_at_entry` — left for the handler entry that delivers the reschedule).
    fx.tlb_send(W, H, serviced_in_wait=variant != "tlb_at_entry")
    fx.add("acquire", H, [1, r, d, 0, 0])
    fx.add("release", H, [1, r, d, 0, 0])
    fx.tlb_send(H, W)
    if variant == "late_entry":
        fx.arrive(W, _nr3(W))
        fx.add("done", W, [r, 0, 0, 0, 0])
        fx.arrive(H, _nr3(H))
        fx.add("done", H, [r, 0, 0, 0, 0])
    else:
        fx.arrive(H, _nr3(H))
        fx.arrive(W, _nr3(W))
        fx.add("done", H, [r, 0, 0, 0, 0])
        fx.add("done", W, [r, 0, 0, 0, 0])


def _synth(variant=None, mutate=None, omit=None, lines_mut=None, counts_mut=None,
           meta_count=None, conflict=False, smp1="ok"):
    fx = _Fix()
    # Serial-round traffic before the first mutual round: shootdowns and their arrivals.
    for _ in range(3):
        fx.tlb_send(0, 1, serviced_in_wait=False)
        fx.arrive(1, 0x20000311)
        fx.tlb_send(1, 0, serviced_in_wait=False)
        fx.arrive(0, 0x2000030b)
    nid, k = 1, 0
    for r in range(1, ROUNDS + 1):
        k += 1
        _round(fx, r, (nid, nid + 1, nid + 2, nid + 3), k, variant)
        nid += 4
    logical = fx.ev
    if mutate:
        logical = mutate(logical)
    omitted = omit(logical) if omit else set()
    n = len(logical)
    meta = ("LOCK3_META vm_lock_id=1 rounds=%d slots_used=%d overflow=0 dump_cpu=1 vector=0xf1 "
            "apic0=0 apic1=1 s_gate=0x%x s_nr3=0x%x c_gate=0x%x c_nr3=0x%x"
            % (ROUNDS, n if meta_count is None else meta_count, CONT["s_gate"], CONT["s_nr3"],
               CONT["c_gate"], CONT["c_nr3"]))
    ent = {c: sum(1 for e in logical if e[0] == "entry" and e[1] == c) for c in (0, 1)}
    eoi = {c: sum(1 for e in logical if e[0] == "eoi" and e[1] == c) for c in (0, 1)}
    last = {c: max([e[2][4] for e in logical if e[0] == "entry" and e[1] == c] or [0])
            for c in (0, 1)}
    counts = ["LOCK3_COUNTS cpu=%d entries=%d eois=%d arrivals=%d tlb_req_gen=%d settled=1"
              % (c, ent[c], eoi[c], last[c], fx.tlb[c]) for c in (0, 1)]
    if counts_mut:
        counts = counts_mut(counts)
    out = ["X86_64 boot", "SMP1_WITNESS_SUMMARY result=%s" % smp1]
    for p in (1, 2):
        out.append(emit(meta, p))
        for seq, (kind, hart, f) in enumerate(logical):
            if seq not in omitted:
                out.append(emit(rec_body("LOCK3", seq, kind, hart, f), p))
        out += [emit(c, p) for c in counts]
        out.append(emit("LOCK3_DUMP_DONE records=%d" % n, p))
    if conflict:
        kind, hart, f = logical[0]
        out.append(emit(rec_body("LOCK3", 0, kind, hart, [f[0], f[1], f[2] ^ 1, f[3], f[4]]), 2))
    if lines_mut:
        out = lines_mut(out)
    return "\n".join(out)


ROUND_OF = {"acquire": 1, "contended": 1, "release": 1, "hold": 1, "ipi": 0, "pending": 0,
            "gate": 0, "done": 0, "begin": 0}


def _rnd(e):
    k, _, f = e
    return f[ROUND_OF[k]] if k in ROUND_OF else None


def _idx(ev, r, pred, nth=0):
    hits = [i for i, e in enumerate(ev) if pred(e) and (r is None or _rnd(e) == r
                                                         or _owner_round(ev, i) == r)]
    if len(hits) <= nth:
        raise AssertionError("fixture anchor missing")
    return hits[nth]


def _owner_round(ev, i):
    """The round a non-round record (icr / entry / eoi) belongs to: the last round record before it."""
    for j in range(i, -1, -1):
        rr = _rnd(ev[j])
        if rr is not None:
            return rr
    return None


def _edit(r, pred, f, nth=0):
    def m(ev):
        i = _idx(ev, r, pred, nth)
        ev[i] = f(ev[i])
        return ev
    return m


def _drop(r, pred, nth=0):
    def m(ev):
        i = _idx(ev, r, pred, nth)
        return ev[:i] + ev[i + 1:]
    return m


def _move_after(r, pred, anchor, count=1):
    """Move `count` consecutive records starting at round r's first `pred` to just after `anchor`."""
    def m(ev):
        i = _idx(ev, r, pred)
        moved = ev[i:i + count]
        ev = ev[:i] + ev[i + count:]
        j = _idx(ev, r, anchor)
        return ev[:j + 1] + moved + ev[j + 1:]
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


def _pair_of(r):
    """The seqs of round r's last acquire/release pair."""
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
DELIV5 = lambda e: e[0] == "entry" and e[1] == H5 and e[2][1] == CONT["c_nr3"]


def _self_test():
    import subprocess
    import tempfile

    def run(log, seal="SMP1_WITNESS_SEAL arch=x86_64 smp=2 mutual_rounds=12 result=ok"):
        paths = []
        for body in (log, seal, "features=x86_64-lock3-witness\n"):
            fd, path = tempfile.mkstemp()
            os.write(fd, body.encode())
            os.close(fd)
            paths.append(path)
        r = subprocess.run([sys.executable, os.path.abspath(sys.argv[0])] + paths,
                           capture_output=True, text=True)
        for p in paths:
            os.unlink(p)
        return r.returncode, r.stdout

    def nr3_entry(r, h):
        return lambda e: e[0] == "entry" and e[1] == h and e[2][1] == _nr3(h)

    cases = [
        # ── positives ──
        ("good", _synth(), 0, None, None),
        ("valid: holder's entry after the waiter's round-done", _synth(variant="late_entry"), 0,
         None, None),
        ("valid: coalescing — the waiter's TLB send merges and the delivering entry does its TLB "
         "work", _synth(variant="tlb_at_entry"), 0, None, None),
        ("valid: coalescing — an extra reschedule send to the holder after the pending observation",
         _synth(mutate=lambda ev: _at_insert_after(ev, 5, K("pending"), [
             "icr", W5, [H5, VECTOR, None, 0, 0]])), 0, "coalesced_sends=13", None),
        ("valid: one damaged copy recovered", _synth(lines_mut=_damage(
            lambda l: l.startswith("LOCK3_REC seq=60 ") and " pass=2 " in l)), 0, None, None),
        ("valid: one uncontended round (no credit, reason recorded)", _synth(mutate=_edit(
            3, K("hold"), _set(3, 0))), 0, "uncredited=3", None),
        # ── the shared core's transport and ownership controls ──
        ("dropped acquire/release pair (both copies)", _synth(omit=_pair_of(5)), 1,
         "missing records", None),
        ("missing ownership interval (holder's release deleted)", _synth(mutate=_drop(
            5, lambda e: e[0] == "release" and e[1] == H5)), 1, "overlapping owners", None),
        ("wrong lock (an acquisition names another lock)", _synth(mutate=_edit(
            5, lambda e: e[0] == "acquire" and e[1] == H5, _set(0, 2))), 1, "not the VM lock", None),
        ("wrong owner (the hold recorded by the waiter)", _synth(mutate=_edit(
            5, K("hold"), lambda e: [e[0], W5, e[2]])), 1, "designated holder", None),
        ("deleted scheduled round", _synth(mutate=lambda ev: [
            e for i, e in enumerate(ev) if _rnd(e) != 7]), 1, "has no complete record", None),
        ("conflicting checksum-valid copies", _synth(conflict=True), 1, "conflicting", None),
        ("metadata and completion counts disagree", _synth(meta_count=7), 1,
         "inconsistent counts", None),
        ("holder not masked (RFLAGS.IF set)", _synth(mutate=_edit(7, K("hold"), _set(4, 1))), 1,
         "not masked", None),
        ("insufficient genuine contention", _synth(mutate=_all_rounds(
            lambda r: _edit(r, K("hold"), _set(3, 0)))), 1, "contended rounds", None),
        ("hold value no waiter produced", _synth(mutate=_edit(5, K("contended"), _set(2, 77))), 1,
         "no waiter contention record produced", None),
        ("SMP1 summary missing", _synth(smp1="fail"), 1, "SMP1", None),
        # ── x86 reschedule-IPI controls ──
        ("premature arrival (delivery inside the masked acquisition)", _synth(mutate=_move_after(
            5, DELIV5, K("pending"), count=2)), 1, "inside the masked acquisition", None),
        ("arrival after the hold began but before the release", _synth(mutate=_move_after(
            5, DELIV5, K("begin"), count=2)), 1, "inside the masked acquisition", None),
        ("wrong target (the publication's ICR names the waiter)", _synth(mutate=_edit(
            5, lambda e: e[0] == "icr" and e[1] == W5 and e[2][1] == VECTOR, _set(0, W5))), 1,
         "publication's ICR write", None),
        ("wrong vector (the publication's ICR is 0xF0)", _synth(mutate=_edit(
            5, lambda e: e[0] == "icr" and e[1] == W5, _set(1, 0xF0))), 1,
         "publication's ICR write", None),
        ("wrong vector (the delivering entry shows 0xF1 not in service)", _synth(mutate=_edit(
            5, DELIV5, _set(3, 0))), 1, "not in service", None),
        ("wrong sender (the publication recorded by the holder)", _synth(mutate=_edit(
            5, K("ipi"), lambda e: [e[0], H5, e[2]])), 1, "no production publication", None),
        ("stale round (the delivery is an earlier arrival)", _synth(mutate=_move_after(
            5, DELIV5, lambda e: e[0] == "gate" and e[1] == H5, count=2)), 1, "stale", None),
        ("missing completion", _synth(mutate=_drop(5, lambda e: e[0] == "eoi" and e[1] == H5,
                                                     nth=0)), 1, "EOI", None),
        ("duplicate completion", _synth(mutate=lambda ev: _dup_after(ev, 5, DELIV5)), 1, "EOI",
         None),
        ("withheld EOI (entry never completed, other CPU unsettled)", _synth(
            mutate=_drop(12, lambda e: e[0] == "eoi" and e[1] == 1), counts_mut=lambda c: [
                c[0], c[1].replace("settled=1", "settled=0")]), 1, "settled", None),
        ("arrival ordinal gap (an arrival the handler did not record)", _synth(mutate=_edit(
            5, DELIV5, lambda e: [e[0], e[1], e[2][:4] + [e[2][4] + 1]])), 1, "ordinal", None),
        ("request already pending when the hold began", _synth(mutate=_edit(
            5, K("begin"), _set(2, VBIT << 32))), 1, "contradicts", None),
        ("TLB request inside the window, kernel still claims outstanding", _synth(mutate=_edit(
            5, K("pending"), lambda e: [e[0], e[1], [e[2][0], e[2][1], e[2][2],
                                                     e[2][3] + 1, e[2][4]]])), 1,
         "contradicts", None),
        ("valid: TLB request inside the window -> round ineligible (reason recorded)",
         _synth(mutate=_edit(5, K("pending"), lambda e: [e[0], e[1], [
             e[2][0], e[2][1], e[2][2], (e[2][3] & 0xffffffff) + 1, e[2][4]]])), 0,
         "uncredited=5(no-request-under-mask)", None),
        ("publication before the holder's hold began", _synth(mutate=_move_after(
            5, lambda e: e[0] == "icr" and e[1] == W5, lambda e: e[0] == "gate" and e[1] == H5,
            count=2)), 1, "outside the holder's masked hold", None),
        ("publication refused (ICR never idle)", _synth(mutate=_edit(5, K("ipi"), _set(3, 0))), 1,
         "no production publication", None),
        ("missing publication", _synth(mutate=lambda ev: _drop(5, K("ipi"))(
            _drop(5, lambda e: e[0] == "icr" and e[1] == W5)(ev))), 1,
         "no production publication", None),
        ("delivery on another continuation (not the mapping syscall's)", _synth(mutate=_edit(
            5, DELIV5, _set(1, CONT["c_gate"]))), 1, "continuation", None),
        ("progress before the delivery (round-done precedes the entry)", _synth(mutate=_move_after(
            5, lambda e: e[0] == "done" and e[1] == H5, K("pending"))), 1, "stale", None),
        ("suppressed reschedule IPI on every round (contention only)", _synth(mutate=_all_rounds(
            lambda r: (lambda ev: _edit(r, K("pending"), lambda e: [e[0], e[1], [
                r, e[2][1], 0, e[2][3] & 0xffffffff, e[2][4]]])(_drop_publication(ev, r))))), 1,
         "attributed reschedule deliveries", None),
    ]
    bad = 0
    for name, log, want, why, _ in cases:
        rc, out = run(log)
        ok = rc == want and (why is None or why in out)
        if not ok:
            bad += 1
        print("[self-test] %-74s rc=%d want=%d %s" % (name[:74], rc, want, "PASS" if ok else "FAIL"))
        if not ok:
            print("            expected reason %r; got:\n%s" % (why, out[-900:]))
    print("[self-test] %s (%d cases)" % ("ALL PASS" if not bad else "FAILURES", len(cases)))
    sys.exit(1 if bad else 0)


def _at_insert_after(ev, r, pred, rec):
    i = _idx(ev, r, pred)
    if rec[0] == "icr" and rec[2][2] is None:
        # The holder's TLB generation at that point (unchanged: a reschedule send bumps nothing).
        rec[2][2] = ev[i][2][3] & 0xffffffff
    return ev[:i + 1] + [rec] + ev[i + 1:]


def _dup_after(ev, r, pred):
    i = _idx(ev, r, pred)
    return ev[:i + 2] + [list(ev[i + 1][:2]) + [list(ev[i + 1][2])]] + ev[i + 2:]


def _drop_publication(ev, r):
    """Remove round r's publication record and the ICR write just before it on the waiter."""
    i = next(j for j, e in enumerate(ev) if e[0] == "ipi" and e[2][0] == r)
    w = ev[i][1]
    j = max(k for k in range(i) if ev[k][1] == w and ev[k][0] == "icr")
    return [e for k, e in enumerate(ev) if k not in (i, j)]


if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
    _self_test()

# ── inputs ───────────────────────────────────────────────────────────────────────────────────
LOG, SEAL, IDENT = (sys.argv[1:4] + [None, None])[:3]
lines = open(LOG, errors="replace").read().split("\n")
fails = []


def fail(msg):
    fails.append(msg)


def round_fail(r, msg):
    fail("round %d: %s" % (r, msg))


ident = open(IDENT).read() if IDENT and os.path.exists(IDENT) else ""
if "features=x86_64-lock3-witness" not in ident:
    fail("the artifact identity does not name the x86_64-lock3-witness build")

# ── corroboration: the SMP1 witness on the same boot ─────────────────────────────────────────
if not any(l.strip().startswith("SMP1_WITNESS_SUMMARY result=ok") for l in lines):
    fail("no SMP1_WITNESS_SUMMARY result=ok from the kernel on this boot")
seal = open(SEAL).read().strip() if SEAL and os.path.exists(SEAL) else ""
if not (seal.startswith("SMP1_WITNESS_SEAL ") and " mutual_rounds=%d " % ROUNDS in seal
        and seal.endswith("result=ok")):
    fail("the SMP1 grader did not seal this boot with %d mutual rounds: %r" % (ROUNDS, seal[-160:]))

META_RX = re.compile(r"^LOCK3_META vm_lock_id=(?P<lock>\d+) rounds=(?P<rounds>\d+) "
                     r"slots_used=(?P<slots_used>\d+) overflow=(?P<overflow>\d+) "
                     r"dump_cpu=(?P<dump_cpu>\d+) vector=0x(?P<vector>[0-9a-f]+) "
                     r"apic0=(?P<apic0>\d+) apic1=(?P<apic1>\d+) s_gate=0x(?P<s_gate>[0-9a-f]+) "
                     r"s_nr3=0x(?P<s_nr3>[0-9a-f]+) c_gate=0x(?P<c_gate>[0-9a-f]+) "
                     r"c_nr3=0x(?P<c_nr3>[0-9a-f]+)$")
COUNTS_RX = re.compile(r"^LOCK3_COUNTS cpu=(?P<cpu>\d+) entries=(?P<entries>\d+) eois=(?P<eois>\d+) "
                       r"arrivals=(?P<arrivals>\d+) tlb_req_gen=(?P<tlb>\d+) "
                       r"settled=(?P<settled>\d+)$")
meta, recs, sums = transport(lines, "LOCK3", META_RX, fail, summary=(COUNTS_RX, "cpu"))
apic = {0: 0, 1: 1}
cont = {}
if meta is not None:
    if int(meta.group("lock")) != VM_LOCK_ID:
        fail("LOCK3_META names vm_lock_id=%s, not %d" % (meta.group("lock"), VM_LOCK_ID))
    if int(meta.group("rounds")) != ROUNDS:
        fail("LOCK3_META schedules %s rounds, not %d" % (meta.group("rounds"), ROUNDS))
    if int(meta.group("dump_cpu")) not in (0, 1):
        fail("LOCK3_META names dump CPU %s" % meta.group("dump_cpu"))
    if int(meta.group("vector"), 16) != VECTOR:
        fail("LOCK3_META names vector 0x%s, not the reschedule vector 0x%x"
             % (meta.group("vector"), VECTOR))
    apic = {0: int(meta.group("apic0")), 1: int(meta.group("apic1"))}
    if apic != {0: 0, 1: 1}:
        fail("LOCK3_META APIC ids %s: the ICR records assume CPU index = APIC id" % apic)
    cont = {k: int(meta.group(k), 16) for k in ("s_gate", "s_nr3", "c_gate", "c_nr3")}
    if len(set(cont.values())) != 4 or not all(cont.values()):
        fail("LOCK3_META continuation addresses are not four distinct VAs: %s" % cont)
nr3 = {0: cont.get("c_nr3"), 1: cont.get("s_nr3")}

ev = [recs[k] for k in sorted(recs)]
check_events(ev, KINDS, (0, 1), VM_LOCK_ID, fail)
by = lambda kind: [e for e in ev if e["kind"] == kind]
acqs = ownership(ev, ROUNDS, fail)
contention_outside_own(ev, acqs, ROUNDS, fail)

# ── the handler's own records, per CPU: entry/EOI strictly paired, ordinals contiguous ─────────
entries_total = eois_total = tlb_entries = 0
for cpu in (0, 1):
    body = sums.get(str(cpu))
    if body is None:
        fail("no intact LOCK3_COUNTS line for CPU %d" % cpu)
        continue
    c = {k: int(v) for k, v in COUNTS_RX.match(body).groupdict().items()}
    if c["settled"] != 1:
        fail("CPU %d had not settled the 0xF1 arrival it had entered when the dump was taken" % cpu)
    mine = [e for e in ev if e["hart"] == cpu and e["kind"] in ("entry", "eoi")]
    n_ent = sum(1 for e in mine if e["kind"] == "entry")
    n_eoi = sum(1 for e in mine if e["kind"] == "eoi")
    if c["entries"] != c["eois"] or n_ent != c["entries"] or n_eoi != c["eois"]:
        fail("unbalanced handler obligations on CPU %d: entries %d / EOIs %d counted, %d / %d recorded"
             % (cpu, c["entries"], c["eois"], n_ent, n_eoi))
    expect = "entry"
    prev = None
    for e in mine:
        if e["kind"] != expect:
            fail("CPU %d: an %s (seq %d) where an %s was owed — every 0xF1 entry is completed by "
                 "exactly one EOI before the next" % (cpu, e["kind"], e["seq"], expect.upper()
                                                     if expect == "eoi" else expect))
            break
        if e["kind"] == "entry":
            if e["f"][0] & 0xff not in (1, 2):
                fail("seq %d: entry names origin %d" % (e["seq"], e["f"][0] & 0xff))
            if not e["f"][3] & VBIT:
                fail("seq %d: CPU %d's 0xF1 entry shows 0xF1 not in service (ISR 0x%x)"
                     % (e["seq"], cpu, e["f"][3] & 0xffffffff))
            if prev is not None and e["f"][4] != prev + 1:
                fail("CPU %d: arrival ordinal gap at seq %d (%d after %d) — an arrival the handler "
                     "did not record" % (cpu, e["seq"], e["f"][4], prev))
            prev = e["f"][4]
            tlb_entries += (e["f"][0] >> 8) & 1
            open_entry = e
            expect = "eoi"
        else:
            if e["f"][1] != open_entry["f"][4] or e["f"][0] & VBIT:
                fail("seq %d: CPU %d's EOI does not complete entry %d's 0xF1 (ordinal %d, ISR 0x%x)"
                     % (e["seq"], cpu, open_entry["seq"], e["f"][1], e["f"][0] & 0xffffffff))
            expect = "entry"
    if expect == "eoi" and c["settled"] == 1:
        fail("CPU %d: its last 0xF1 entry has no EOI" % cpu)
    if prev is not None and prev != c["arrivals"]:
        fail("CPU %d: the last recorded arrival ordinal %d is not the production arrival count %d"
             % (cpu, prev, c["arrivals"]))
    entries_total += c["entries"]
    eois_total += c["eois"]

# ── ICR writes ──
for i in by("icr"):
    if i["f"][0] not in apic.values():
        fail("seq %d: an ICR write names APIC id %d" % (i["seq"], i["f"][0]))
cpu_of_apic = {v: k for k, v in apic.items()}


def is_resched_to(i, h):
    return i["kind"] == "icr" and i["f"][0] == apic[h] and i["f"][1] & 0xff == VECTOR


# ── per-round obligations ────────────────────────────────────────────────────────────────────
credited = {0: 0, 1: 0}
delivered = {0: 0, 1: 0}
uncredited = []
coalesced = deliveries_with_tlb = 0
for r in range(1, ROUNDS + 1):
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    g_h = [e for e in by("gate") if e["f"][0] == r and e["hart"] == H and e["f"][2] == 0]
    g_w = [e for e in by("gate") if e["f"][0] == r and e["hart"] == W and e["f"][2] == 1]
    gates = [e for e in by("gate") if e["f"][0] == r]
    dones = [e for e in by("done") if e["f"][0] == r]
    holds = [e for e in by("hold") if e["f"][1] == r]
    pends = [e for e in by("pending") if e["f"][0] == r]
    begins = [e for e in by("begin") if e["f"][0] == r]
    if (len(g_h) != 1 or len(g_w) != 1 or len(gates) != 2
            or sorted(d["hart"] for d in dones) != [0, 1] or any(g["f"][1] != H for g in gates)):
        round_fail(r, "has no complete record (gates %d, dones %s)"
                   % (len(gates), sorted(d["hart"] for d in dones)))
        continue
    if (len(holds) != 1 or holds[0]["hart"] != H or len(pends) != 1 or pends[0]["hart"] != H
            or len(begins) != 1 or begins[0]["hart"] != H):
        round_fail(r, "has no complete record (expected one hold-begin, one hold and one pending "
                   "observation by the designated holder %d, found %d / %d / %d)"
                   % (H, len(begins), len(holds), len(pends)))
        continue
    begin, hold, pend = begins[0], holds[0], pends[0]
    aid = hold["f"][2]
    if hold["f"][4] & 1:
        round_fail(r, "holder was not masked (RFLAGS.IF set) inside the critical section")
        continue
    if not (inside(acqs, begin, aid) and inside(acqs, hold, aid) and acqs[aid]["round"] == r
            and begin["f"][1] == aid and begin["seq"] < hold["seq"]):
        round_fail(r, "the hold falls outside its acquisition %d" % aid)
        continue
    if pend["f"][4] != aid or not inside(acqs, pend, aid) or pend["f"][1] != W:
        round_fail(r, "the pending observation is not the holder's, inside acquisition %d" % aid)
        continue
    if g_h[0]["seq"] > begin["seq"]:
        round_fail(r, "the hold began (seq %d) before the holder left its gate (seq %d)"
                   % (begin["seq"], g_h[0]["seq"]))
        continue
    a = acqs[aid]
    # A handler entry on the holder anywhere inside the masked acquisition contradicts the mask.
    early = [e for e in ev if e["kind"] == "entry" and e["hart"] == H and a["a"] < e["seq"] < a["r"]]
    if early:
        round_fail(r, "a 0xF1 entry (seq %d) on the holder lies inside the masked acquisition %d — "
                   "before its release record (seq %d)" % (early[0]["seq"], aid, a["r"]))
        continue
    got_credit, why = contention_credit(r, W, hold, ev, acqs)
    if why:
        round_fail(r, why)
        continue
    contended = bool(got_credit and g_w[0]["f"][3] == 1)
    if contended:
        credited[W] += 1

    pre_isr, pre_irr = begin["f"][2] & 0xffffffff, begin["f"][2] >> 32
    post_isr, post_irr = pend["f"][2] & 0xffffffff, pend["f"][2] >> 32
    pre_tlb, post_tlb = begin["f"][3], pend["f"][3] & 0xffffffff
    out = int(not pre_irr & VBIT and not pre_isr & VBIT and bool(post_irr & VBIT)
              and not post_isr & VBIT and pre_tlb == post_tlb)
    if pend["f"][3] >> 32 != out:
        round_fail(r, "the masked holder's outstanding flag %d contradicts its LAPIC views (at hold "
                   "start ISR 0x%x IRR 0x%x TLB gen %d; after contention ISR 0x%x IRR 0x%x TLB gen "
                   "%d)" % (pend["f"][3] >> 32, pre_isr, pre_irr, pre_tlb, post_isr, post_irr,
                            post_tlb))
        continue
    if not out:
        uncredited.append("%d(no-request-under-mask)" % r)
        continue
    if not contended:
        uncredited.append("%d(uncontended)" % r)
        continue
    pubs = [e for e in by("ipi") if e["f"][0] == r and e["hart"] == W and e["f"][1] == H
            and e["f"][2] == VECTOR and e["f"][3] == 1]
    if len(pubs) != 1:
        round_fail(r, "no production publication by the waiter to the holder (vector 0x%x, accepted)"
                   % VECTOR)
        continue
    p = pubs[0]
    icr = [e for e in ev if e["hart"] == W and e["seq"] < p["seq"] and e["kind"] != "contended"]
    icr = icr[-1] if icr else None
    if (icr is None or icr["kind"] != "icr" or icr["f"][0] != apic[H] or icr["f"][1] != VECTOR
            or icr["f"][2] != pre_tlb):
        round_fail(r, "the publication's ICR write is not a fixed 0x%x IPI to the holder's APIC id "
                   "%d leaving its TLB generation %d (found %s)"
                   % (VECTOR, apic[H], pre_tlb, icr and (icr["kind"], icr["f"][:3])))
        continue
    if not (begin["seq"] < icr["seq"] < pend["seq"] and a["a"] < icr["seq"] < hold["seq"]
            and g_w[0]["seq"] < icr["seq"]):
        round_fail(r, "the publication's ICR write (seq %d) lies outside the holder's masked hold "
                   "(hold began seq %d, contention seen seq %d)"
                   % (icr["seq"], begin["seq"], hold["seq"]))
        continue
    others = [e for e in ev if begin["seq"] < e["seq"] < pend["seq"] and e is not icr
              and is_resched_to(e, H)]
    if others:
        uncredited.append("%d(second-sender-in-window)" % r)
        continue
    # The delivering entry: the holder's first 0xF1 entry after the pending observation, before its
    # round-done record.
    done_h = next(d for d in dones if d["hart"] == H)
    after = [e for e in ev if e["kind"] == "entry" and e["hart"] == H and e["seq"] > pend["seq"]]
    d = after[0] if after else None
    if d is None or d["seq"] > done_h["seq"]:
        round_fail(r, "stale or missing delivery: no 0xF1 entry on the holder between its pending "
                   "observation (seq %d) and its round-done record (seq %d)"
                   % (pend["seq"], done_h["seq"]))
        continue
    if d["seq"] < a["r"]:
        round_fail(r, "the delivering entry (seq %d) lies inside the masked acquisition %d"
                   % (d["seq"], aid))
        continue
    if d["f"][0] & 0xff != 2 or d["f"][1] != nr3.get(H):
        round_fail(r, "the delivering entry (seq %d) interrupted origin %d at 0x%x, not ring 3 at the "
                   "holder's mapping-syscall continuation 0x%x"
                   % (d["seq"], d["f"][0] & 0xff, d["f"][1], nr3.get(H) or 0))
        continue
    if not d["f"][3] & VBIT:
        round_fail(r, "the delivering entry shows 0xF1 not in service")
        continue
    eoi = [e for e in ev if e["kind"] == "eoi" and e["hart"] == H and e["seq"] > d["seq"]]
    eoi = eoi[0] if eoi else None
    nxt = [e for e in ev if e["kind"] == "entry" and e["hart"] == H and e["seq"] > d["seq"]]
    if eoi is None or (nxt and nxt[0]["seq"] < eoi["seq"]) or eoi["f"][1] != d["f"][4]:
        round_fail(r, "the delivering entry has no EOI of its own before the holder's next entry")
        continue
    if not eoi["seq"] < done_h["seq"]:
        round_fail(r, "the holder's round-done record (seq %d) precedes the delivery's EOI (seq %d)"
                   % (done_h["seq"], eoi["seq"]))
        continue
    coalesced += sum(1 for e in ev if pend["seq"] < e["seq"] < d["seq"] and is_resched_to(e, H))
    deliveries_with_tlb += (d["f"][0] >> 8) & 1
    delivered[W] += 1

for dd, who in ((1, "S (CPU 1)"), (0, "C (CPU 0)")):
    if credited[dd] < MIN_PER_DIRECTION:
        fail("contended rounds with %s waiting: %d, need >= %d"
             % (who, credited[dd], MIN_PER_DIRECTION))
    if delivered[dd] < MIN_PER_DIRECTION:
        fail("attributed reschedule deliveries with %s publishing: %d, need >= %d"
             % (who, delivered[dd], MIN_PER_DIRECTION))

for f in fails:
    print("[lock3-witness][fail] " + f)
ok = not fails
pubs_total = len(by("ipi"))
print("LOCK3_WITNESS_SEAL rounds=%d credited_c_waits=%d credited_s_waits=%d delivered_c=%d "
      "delivered_s=%d uncredited=%s requests=%d icr_writes=%d entries=%d eois=%d "
      "entries_with_tlb_work=%d coalesced_sends=%d deliveries_with_tlb_work=%d result=%s"
      % (ROUNDS, credited[0], credited[1], delivered[0], delivered[1],
         ",".join(uncredited) or "none", pubs_total, len(by("icr")), entries_total, eois_total,
         tlb_entries, coalesced, deliveries_with_tlb, "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
