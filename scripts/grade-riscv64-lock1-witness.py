#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-LOCK1 (SEAL + final checks) — the independent grader for the subdomain-lock contention witness.
#
# It re-derives every round from the raw sealed `LOCK1_REC` lines (never from the kernel's summary)
# with an explicit ownership model and a generation-attributed interrupt chain. Every ordering it
# relies on is justified from the recording semantics, not assumed from record sequence numbers:
#
#   * acquire records are taken AFTER the successful CAS and release records BEFORE the unlocking
#     store (release INTENT). Release-record fetch_add -> Release store -> Acquire CAS -> next
#     acquire-record fetch_add is a happens-before chain, so for one lock the acquire/release
#     records must strictly alternate, each release naming (by acquisition id, CPU and round) the
#     acquisition it ends. Overlapping owners, unmatched or duplicate releases, duplicate acquisition
#     ids and unreleased acquisitions are contradictions.
#   * hold / pending observations are made by the owning CPU inside its acquisition (program order),
#     so they must fall inside that acquisition's [acquire, release] records.
#   * a contention observation is recorded by a CPU inside its own lock() call, before the CAS that
#     ends it, so it can never fall inside an interval that CPU owns, and the CPU's next acquisition
#     is the one that call completed. Its record may land after the owner's release record (an
#     observer may run after the atomic it describes); contention is therefore attributed BY VALUE:
#     the holder's hold records the contention-counter value it saw advance while it owned the lock,
#     the waiter's contended record carries the value it produced, and the waiter's gate record
#     certifies its lock() call began after the holder's CAS.
#   * interrupt work is attributed BY GENERATION at the production mailbox: the publication owner
#     advances the (target, sender) generation before setting the bit; the masked holder positively
#     reads its own mailbox (bit outstanding? which generation?) inside its ownership; the arrival
#     hook receives the sources the consumption actually swapped out. An empty arrival, an unrelated
#     source, a stale generation, a wrong round or target, a never-armed link or an overwritten link
#     never discharges a round's obligation.
#
#   * TRANSPORT (final checks): the metadata and completion records are checksum-validated and must
#     agree on the record count; after deduplicating identical redundant copies the records must be
#     exactly the contiguous sequence the dump declares. A damaged copy is recovered from another
#     intact copy; a missing record, an extra record, inconsistent counts, conflicting valid copies or
#     an incomplete dump fails the boot.
#   * an attributed CONSUMPTION follows the release record of the acquisition its masked-pending
#     observation named: release record (intent) -> unlocking store -> interrupt restoration -> trap
#     -> consumption, program order on the holder's CPU. The release record is not the atomic unlock.
#
# Every scheduled round must have a complete record establishing its outcome; a valid uncontended
# round earns no credit, but absent or contradictory evidence fails the boot. The SMP3 seal on the
# same boot corroborates progress/results/context; it substitutes for none of the above.
#
# Usage: grade-riscv64-lock1-witness.py <boot.log> <artifact-identity> [<firmware.pin>]
import re
import sys

VM_LOCK_ID = 1
ROUNDS = 12
MIN_PER_DIRECTION = 4  # credited contended rounds, and attributed masked deliveries, per direction

KINDS = {"acquire", "contended", "release", "hold", "ipi", "arrival", "pending", "gate", "done",
         "linklost"}
LOCK_KINDS = {"acquire", "contended", "release", "hold"}


def fnv1a(s: str) -> int:
    h = 0x811C9DC5
    for b in s.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


# ── synthetic fixtures (self-test) ───────────────────────────────────────────────────────────
def _line(seq, kind, hart, f, pass_):
    b = "LOCK1_REC seq=%d kind=%s hart=%d f0=0x%x f1=0x%x f2=0x%x f3=0x%x f4=0x%x" % (
        seq, kind, hart, f[0], f[1], f[2], f[3], f[4])
    return "%s pass=%d crc=0x%08x" % (b, pass_, fnv1a(b))


def _round(r, ids, gen, k, variant):
    """One realistic round. Returns [(kind, hart, f)]. `variant` names a valid interleaving."""
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    a, b, c, d = ids
    hold = ("hold", H, [1, r, a, k, 0 | (1 << 1) | ((k - 1) << 8)])
    pend = ("pending", H, [r, W, 1, gen, a])
    con = ("contended", W, [1, r, k, 0, 0])
    ev = [
        ("gate", H, [r, H, 0, 1, 0]),
        ("acquire", H, [1, r, a, 0, 0]),
        ("gate", W, [r, H, 1, 1, 0]),
        ("ipi", W, [r, H, 1, gen, 0]),
    ]
    if variant == "late_contended":
        # The waiter's observer callback runs after the holder already recorded hold/pending/release.
        ev += [hold, pend, ("release", H, [1, r, a, 0, 0]), con]
    else:
        ev += [con, hold, pend, ("release", H, [1, r, a, 0, 0])]
    ev += [("acquire", W, [1, r, b, 0, 0]), ("release", W, [1, r, b, 0, 0])]
    if variant == "role_swap":
        # The holder's install contends on the waiter's install acquisition.
        ev += [("acquire", W, [1, r, d, 0, 0]), ("contended", H, [1, r, k + 1, 0, 0]),
               ("release", W, [1, r, d, 0, 0]), ("acquire", H, [1, r, c, 0, 0]),
               ("release", H, [1, r, c, 0, 0])]
    else:
        ev += [("acquire", H, [1, r, c, 0, 0]), ("release", H, [1, r, c, 0, 0]),
               ("acquire", W, [1, r, d, 0, 0]), ("release", W, [1, r, d, 0, 0])]
    snap = gen + 1 if variant == "coalesced" else gen
    ev += [("arrival", H, [r, 1 << W, gen, snap, 1 | (W << 8)]),
           ("done", H, [r, 0, 0, 0, 0]), ("done", W, [r, 0, 0, 0, 0])]
    return ev


def _emit(body, pass_):
    return "%s pass=%d crc=0x%08x" % (body, pass_, fnv1a(body))


def _synth(rounds=ROUNDS, meta_lock=VM_LOCK_ID, variant=None, mutate=None, conflict=False,
           omit=None, lines_mut=None, meta_count=None, done_count=None):
    """A complete, genuine two-pass dump: an intact SMP3 verdict and, per pass, an intact
    `LOCK1_META`, every record, and an intact `LOCK1_DUMP_DONE`, each with its real checksum and the
    real record count. `mutate` edits the logical record list (records are renumbered), `omit`
    names logical indices whose copies are left out WITHOUT renumbering, and `lines_mut` edits the
    final printed lines; the fixtures go through exactly the production transport validation."""
    logical = []
    nid, k = 1, 0
    gens = {0: 0, 1: 0}
    for r in range(1, rounds + 1):
        W = 1 - (0 if r % 2 == 1 else 1)
        gens[W] += 2 if variant == "coalesced" else 1  # a prior SMP3 send may have coalesced
        k += 2
        logical += [list(e) for e in _round(r, (nid, nid + 1, nid + 2, nid + 3), gens[W], k,
                                            variant)]
        nid += 4
    if mutate:
        logical = mutate(logical)
    omitted = omit(logical) if omit else set()
    n = len(logical)
    meta = "LOCK1_META vm_lock_id=%d rounds=%d slots_used=%d overflow=0 dump_cpu=0" % (
        meta_lock, ROUNDS, n if meta_count is None else meta_count)
    done = "LOCK1_DUMP_DONE records=%d" % (n if done_count is None else done_count)
    out = ["OpenSBI v1.3"] + [_emit("SMP3_VERDICT result=ok reason=none at=0", p) for p in (1, 2)]
    for p in (1, 2):
        out.append(_emit(meta, p))
        for seq, (kind, hart, f) in enumerate(logical):
            if seq not in omitted:
                out.append(_line(seq, kind, hart, f, p))
        out.append(_emit(done, p))
    if conflict:
        kind, hart, f = logical[0]
        bad = list(f)
        bad[3] ^= 1
        out.append(_line(0, kind, hart, bad, 2))
    if lines_mut:
        out = lines_mut(out)
    return "\n".join(out)


def _rnd(e):
    k, _, f = e
    return f[1] if k in LOCK_KINDS else f[0]


def _at(r, pred, fn):
    """Apply `fn(list, index)` at the first event of round r matching pred."""
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


def _insert_after(r, pred, new):
    return _at(r, pred, lambda ev, i: ev[:i + 1] + [new] + ev[i + 1:])


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
    """`omit` for the last complete acquire/release pair of round r (indices in the logical list)."""
    def o(ev):
        a = max(i for i, e in enumerate(ev) if e[0] == "acquire" and e[2][1] == r)
        aid = ev[a][2][2]
        rel = next(i for i in range(a + 1, len(ev)) if ev[i][0] == "release" and ev[i][2][2] == aid)
        return {a, rel}
    return o


def _arrival_before_release(r):
    """Move round r's arrival to just after its pending observation, before the holder's release."""
    def m(ev):
        ia = next(i for i, e in enumerate(ev) if e[0] == "arrival" and e[2][0] == r)
        arr = ev.pop(ia)
        ip = next(i for i, e in enumerate(ev) if e[0] == "pending" and e[2][0] == r)
        ev.insert(ip + 1, arr)
        return ev
    return m


def _arrival_after_done(r):
    """A legitimately delayed consumption: round r's arrival after both completion records."""
    def m(ev):
        ia = next(i for i, e in enumerate(ev) if e[0] == "arrival" and e[2][0] == r)
        arr = ev.pop(ia)
        last = max(i for i, e in enumerate(ev) if e[0] == "done" and e[2][0] == r)
        ev.insert(last + 1, arr)
        return ev
    return m


def _damage(pred):
    """Corrupt (checksum no longer matches) every printed line matching pred."""
    def d(out):
        return [l[:-1] + ("0" if l[-1] != "0" else "1") if pred(l) else l for l in out]
    return d


def _rec_line(seq, pass_=None):
    return lambda l: l.startswith("LOCK1_REC seq=%d " % seq) and (
        pass_ is None or " pass=%d " % pass_ in l)


K = lambda kind, hart=None: (lambda e: e[0] == kind and (hart is None or e[1] == hart))
H5, W5 = 0, 1  # round 5's holder / waiter


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
        r = subprocess.run([sys.executable, sys.argv[0], path, idpath, "/dev/null"],
                           capture_output=True, text=True)
        os.unlink(path)
        os.unlink(idpath)
        return r.returncode, r.stdout

    def holder(r):
        return 0 if r % 2 == 1 else 1

    _FULL = sum(1 for l in _synth().split("\n") if l.startswith("LOCK1_REC") and " pass=1 " in l)
    cases = [
        # ── positives: valid recordings, interleavings and coalescing ──
        ("good", _synth(), 0, None),
        ("valid: coalesced publications", _synth(variant="coalesced"), 0, None),
        ("valid: late contended observer record", _synth(variant="late_contended"), 0, None),
        ("valid: role-swapped install contention", _synth(variant="role_swap"), 0, None),
        ("valid: one uncontended round (no credit)", _synth(mutate=lambda ev: _edit(
            3, K("pending"), _set(2, 0))(_edit(3, K("hold"), _set(3, 0))(
            _drop(3, K("arrival"))(ev)))), 0, None),
        # ── §1 — the three remaining false passes ──
        ("overlapping owners", _synth(mutate=_insert_after(
            5, K("hold"), ["acquire", W5, [1, 5, 999, 0, 0]])), 1, "overlapping owners"),
        ("release without acquisition", _synth(mutate=_at(
            5, K("gate"), lambda ev, i: ev[:i] + [["release", W5, [1, 5, 998, 0, 0]]] + ev[i:])),
         1, "release with no preceding acquisition"),
        ("deleted scheduled round", _synth(mutate=lambda ev: [e for e in ev if _rnd(e) != 7]), 1,
         "has no complete record"),
        # ── the four LOCK1-ACCEPTANCE reproductions ──
        ("lock 99", _synth(mutate=lambda ev: [
            [k, h, [99] + f[1:]] if k in LOCK_KINDS else [k, h, f] for k, h, f in ev]), 1,
         "names lock 99"),
        ("waiter release before acquire", _synth(mutate=_at(
            5, K("acquire", W5), lambda ev, i: ev[:i] + [ev[i + 1], ev[i]] + ev[i + 2:])), 1,
         "release with no preceding acquisition"),
        ("ipi after releases", _synth(mutate=_at(
            5, K("ipi"), lambda ev, i: ev[:i] + ev[i + 1:i + 6] + [ev[i]] + ev[i + 6:])), 1,
         "publication is not ordered before the hold"),
        ("one holder unmasked", _synth(mutate=_edit(7, K("hold"), _set(4, 1))), 1,
         "not masked"),
        # ── §2 — ownership-history controls ──
        ("missing holder acquire", _synth(mutate=_drop(5, K("acquire", H5))), 1,
         "release with no preceding acquisition"),
        ("missing holder release", _synth(mutate=_drop(5, K("release", H5))), 1,
         "overlapping owners"),
        ("duplicate acquisition id", _synth(mutate=_edit(5, K("acquire", W5), _set(2, 1))), 1,
         "duplicate acquisition id"),
        ("release names another acquisition", _synth(mutate=_edit(
            5, K("release", H5), _set(2, 2))), 1, "does not match the owning acquisition"),
        ("contention inside own ownership", _synth(mutate=_insert_after(
            5, K("acquire", W5), ["contended", W5, [1, 5, 500, 0, 0]])), 1,
         "while it owned the lock"),
        ("hold outside its acquisition", _synth(mutate=_at(
            5, K("hold"), lambda ev, i: ev[:i] + ev[i + 1:i + 3] + [ev[i]] + ev[i + 3:])), 1,
         "outside its acquisition"),
        ("hold value no waiter produced", _synth(mutate=_edit(5, K("contended", W5),
                                                              _set(2, 777))), 1,
         "no waiter contention record produced"),
        ("insufficient genuine contention", _synth(mutate=_all_rounds(
            lambda r: _edit(r, K("hold"), _set(3, 0)))), 1, "contended rounds"),
        ("missing gate", _synth(mutate=_drop(5, K("gate", W5))), 1, "has no complete record"),
        ("missing done", _synth(mutate=_drop(5, K("done", W5))), 1, "has no complete record"),
        ("meta lock not 1", _synth(meta_lock=99), 1, "vm_lock_id"),
        ("conflicting checksum-valid copies", _synth(conflict=True), 1, "conflicting"),
        # ── §3 — attributed delivery controls ──
        ("empty arrival while armed", _synth(mutate=_insert_after(
            5, K("release", W5), ["arrival", H5, [5, 0, 1, 1, 0 | (W5 << 8)]])), 1,
         "did not swap out"),
        ("unrelated-source arrival while armed", _synth(mutate=_insert_after(
            5, K("release", W5), ["arrival", H5, [5, 1 << H5, 1, 1, 0 | (W5 << 8)]])), 1,
         "did not swap out"),
        ("wrong-round consumption (other holder's round)", _synth(mutate=_edit(
            5, K("arrival"), _set(0, 4))), 1, "never armed"),
        ("wrong-round consumption (later round, same hart)", _synth(mutate=_edit(
            5, K("arrival"), _set(0, 9))), 1, "precedes the masked observation"),
        ("wrong-target consumption", _synth(mutate=_edit(5, K("arrival"), lambda e: [
            e[0], W5, e[2]])), 1, "never armed"),
        ("stale-generation consumption", _synth(mutate=_edit(5, K("arrival"), _set(3, 0))), 1,
         "stale"),
        ("consumed before hold + unrelated arrival", _synth(mutate=_all_rounds(
            lambda r: (lambda ev: _edit(r, K("arrival"), _set(1, 1 << holder(r)))(
                _edit(r, K("pending"), _set(2, 0))(ev))))), 1, "never armed"),
        ("missing consumption", _synth(mutate=_drop(5, K("arrival"))), 1, "never discharged"),
        ("missing publication", _synth(mutate=_drop(5, K("ipi"))), 1, "no production publication"),
        ("publication to the wrong target", _synth(mutate=_edit(5, K("ipi"), _set(1, W5))), 1,
         "no production publication"),
        ("pending generation is not the round's", _synth(mutate=_edit(5, K("pending"),
                                                                     _set(3, 99))), 1,
         "generation"),
        ("overwritten unresolved link", _synth(mutate=_insert_after(
            5, K("pending"), ["linklost", H5, [3, 5, 1, 0, 0]])), 1, "overwrote"),
        # ── final checks: transport accounting ──
        ("valid: one damaged copy recovered from the other pass", _synth(
            lines_mut=_damage(_rec_line(40, 2))), 0, None),
        ("valid: damaged first-pass metadata, intact second pass", _synth(
            lines_mut=_damage(lambda l: l.startswith("LOCK1_META ") and " pass=1 " in l)), 0, None),
        ("valid: second pass cut short, first pass complete", _synth(
            lines_mut=lambda out: out[:len(out) - 40]), 0, None),
        ("dropped acquire/release pair (both copies)", _synth(omit=_last_pair(5)), 1,
         "missing records"),
        ("both copies of one record damaged", _synth(lines_mut=_damage(_rec_line(40))), 1,
         "missing records"),
        ("records renumbered, declared count unchanged", _synth(
            mutate=lambda ev: ev[:-2], meta_count=_FULL, done_count=_FULL), 1, "missing records"),
        ("record beyond the declared count", _synth(meta_count=_FULL - 1, done_count=_FULL - 1), 1,
         "beyond the declared count"),
        ("metadata and completion counts disagree", _synth(done_count=_FULL + 1), 1,
         "inconsistent counts"),
        ("no completion record (incomplete dump)", _synth(
            lines_mut=lambda out: [l for l in out if not l.startswith("LOCK1_DUMP_DONE")]), 1,
         "incomplete"),
        ("every metadata copy damaged", _synth(
            lines_mut=_damage(lambda l: l.startswith("LOCK1_META "))), 1, "no intact LOCK1_META"),
        ("conflicting checksum-valid metadata", _synth(lines_mut=lambda out: out + [_emit(
            "LOCK1_META vm_lock_id=1 rounds=12 slots_used=7 overflow=0 dump_cpu=0", 2)]), 1,
         "conflicting checksum-valid LOCK1_META"),
        ("SMP3 verdict copies all damaged", _synth(
            lines_mut=_damage(lambda l: l.startswith("SMP3_VERDICT "))), 1,
         "no intact SMP3 verdict"),
        # ── final checks: arrival ordering ──
        ("arrival between pending and the holder's release", _synth(
            mutate=_arrival_before_release(5)), 1, "precedes the release record"),
        ("valid: consumption delayed past both completions", _synth(
            mutate=_arrival_after_done(5)), 0, None),
        ("valid: delayed consumption with coalesced publications", _synth(
            variant="coalesced", mutate=_arrival_after_done(6)), 0, None),
    ]
    bad = 0
    for name, log, want, why in cases:
        rc, out = run(log)
        ok = rc == want and (why is None or why in out)
        if not ok:
            bad += 1
        print("[self-test] %-44s rc=%d want=%d %s" % (name, rc, want, "PASS" if ok else "FAIL"))
        if not ok and why is not None:
            print("            expected reason %r; got:\n%s" % (why, out[-600:]))
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
    pin = dict(l.split("=", 1) for l in open(PIN).read().splitlines()
               if "=" in l and not l.startswith("#"))
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

# ── the SMP3 seal must pass on this same boot (corroboration; intact, non-conflicting copies) ──
smp3 = {}
for l in lines:
    if l.strip().startswith("SMP3_VERDICT "):
        mm = re.match(r"^(.*) pass=(\d+) crc=0x([0-9a-f]{8})$", l.strip())
        if mm and fnv1a(mm.group(1)) == int(mm.group(3), 16):
            smp3.setdefault(mm.group(1), 0)
if not smp3:
    fail("no intact SMP3 verdict on this boot")
elif len(smp3) > 1:
    fail("conflicting checksum-valid SMP3 verdicts: %s" % sorted(smp3))
elif not next(iter(smp3)).startswith("SMP3_VERDICT result=ok "):
    fail("the SMP3 seal did not pass: " + next(iter(smp3))[:120])

# ── transport accounting ─────────────────────────────────────────────────────────────────────
# The dump is printed in two passes; each pass is `LOCK1_META`, every record, `LOCK1_DUMP_DONE`, each
# line carrying `pass=N crc=0x…` over the text before ` pass=`. A copy whose checksum fails (or that
# is torn) is damaged and ignored; another intact copy of the same line recovers it. Identical intact
# copies are deduplicated; differing intact copies of one line are a contradiction. The metadata and
# completion records must be intact and agree on the record count, and the deduplicated records must
# be exactly the contiguous sequence 0..count-1 that the dump declares — no gap, no extra record.
TAIL = re.compile(r"^(.*) pass=(\d+) crc=0x([0-9a-f]{8})$")
META_RX = re.compile(r"^LOCK1_META vm_lock_id=(\d+) rounds=(\d+) slots_used=(\d+) overflow=(\d+) "
                     r"dump_cpu=(\d+)$")
DONE_RX = re.compile(r"^LOCK1_DUMP_DONE records=(\d+)$")
REC_RX = re.compile(r"^LOCK1_REC seq=(\d+) kind=(\w+) hart=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) "
                    r"f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)$")


def intact(l):
    """-> (body, pass) for a checksum-valid copy, else None."""
    mm = TAIL.match(l.strip())
    if not mm or fnv1a(mm.group(1)) != int(mm.group(3), 16):
        return None
    return mm.group(1), int(mm.group(2))


metas, dones, recs, damaged = {}, {}, {}, 0
first_meta = last_done = None
for i, l in enumerate(lines):
    s = l.strip()
    if not s.startswith("LOCK1_"):
        continue
    got = intact(s)
    if got is None:
        damaged += 1
        continue
    body, pass_ = got
    if pass_ not in (1, 2):
        fail("line %d: an intact LOCK1 copy names dump pass %d" % (i + 1, pass_))
    if body.startswith("LOCK1_META "):
        if not META_RX.match(body):
            fail("a checksum-valid LOCK1_META line is malformed: %r" % body[:100])
            continue
        metas.setdefault(body, []).append(pass_)
        first_meta = i if first_meta is None else first_meta
    elif body.startswith("LOCK1_DUMP_DONE "):
        if not DONE_RX.match(body):
            fail("a checksum-valid LOCK1_DUMP_DONE line is malformed: %r" % body[:100])
            continue
        dones.setdefault(body, []).append(pass_)
        last_done = i
    elif body.startswith("LOCK1_REC "):
        mm = REC_RX.match(body)
        if not mm:
            fail("a checksum-valid LOCK1_REC line is malformed: %r" % body[:100])
            continue
        seq = int(mm.group(1))
        rec = dict(seq=seq, kind=mm.group(2), hart=int(mm.group(3)),
                   f=[int(mm.group(j), 16) for j in range(4, 9)], body=body)
        if seq in recs:
            if recs[seq]["body"] != body:
                fail("conflicting checksum-valid copies of seq %d" % seq)
        else:
            recs[seq] = rec
    else:
        fail("an intact line of unknown LOCK1 type: %r" % body[:80])

declared = None
if not metas:
    fail("no intact LOCK1_META line (the dump's metadata is missing or every copy is damaged)")
elif len(metas) > 1:
    fail("conflicting checksum-valid LOCK1_META copies: %s" % sorted(metas))
else:
    vm_lock, rounds_m, slots, overflow, dump_cpu = map(int, META_RX.match(next(iter(metas))).groups())
    if overflow != 0:
        fail("the LOCK1 record overflowed its ring (overflow=%d)" % overflow)
    if vm_lock != VM_LOCK_ID:
        fail("LOCK1_META names vm_lock_id=%d, not %d" % (vm_lock, VM_LOCK_ID))
    if rounds_m != ROUNDS:
        fail("LOCK1_META schedules %d rounds, not %d" % (rounds_m, ROUNDS))
    if dump_cpu not in (0, 1):
        fail("LOCK1_META names dump CPU %d" % dump_cpu)
    declared = slots
if not dones:
    fail("the dump is incomplete: no intact LOCK1_DUMP_DONE completion record")
elif len(dones) > 1:
    fail("conflicting checksum-valid LOCK1_DUMP_DONE copies: %s" % sorted(dones))
else:
    done_n = int(DONE_RX.match(next(iter(dones))).group(1))
    if declared is not None and done_n != declared:
        fail("inconsistent counts: LOCK1_META declares %d records, LOCK1_DUMP_DONE %d"
             % (declared, done_n))
    if first_meta is not None and last_done < first_meta:
        fail("the dump is incomplete: no completion record follows its metadata")
    declared = done_n if declared is None else declared
if declared is not None:
    missing = [s for s in range(declared) if s not in recs]
    extra = sorted(s for s in recs if s >= declared)
    if missing:
        fail("missing records: %d of the %d declared have no intact copy (seq %s%s)"
             % (len(missing), declared, ", ".join(map(str, missing[:8])),
                ", …" if len(missing) > 8 else ""))
    if extra:
        fail("records beyond the declared count %d: seq %s" % (declared, extra[:8]))
ev = [recs[k] for k in sorted(recs)]
for e in ev:
    if e["kind"] not in KINDS:
        fail("unknown event kind %r at seq %d" % (e["kind"], e["seq"]))
    if e["hart"] not in (0, 1):
        fail("seq %d recorded by unexpected hart %d" % (e["seq"], e["hart"]))
    if e["kind"] in LOCK_KINDS and e["f"][0] != VM_LOCK_ID:
        fail("seq %d: a lock event names lock %d, not the VM lock %d"
             % (e["seq"], e["f"][0], VM_LOCK_ID))
by = lambda kind: [e for e in ev if e["kind"] == kind]

# ── the ownership model (every recorded acquisition and release, in record order) ──
acqs = {}
owner = None
for e in ev:
    if e["kind"] not in ("acquire", "release"):
        continue
    aid, rnd = e["f"][2], e["f"][1]
    if e["kind"] == "acquire":
        if aid in acqs:
            fail("seq %d: duplicate acquisition id %d" % (e["seq"], aid))
            continue
        if owner is not None:
            o = acqs[owner]
            fail("seq %d: overlapping owners — hart %d acquired (id %d) while hart %d's acquisition "
                 "%d (seq %d) was unreleased" % (e["seq"], e["hart"], aid, o["cpu"], owner, o["a"]))
        if not 1 <= rnd <= ROUNDS:
            fail("seq %d: acquisition tagged with unscheduled round %d" % (e["seq"], rnd))
        acqs[aid] = dict(cpu=e["hart"], round=rnd, a=e["seq"], r=None)
        owner = aid
    else:
        if owner is None:
            fail("seq %d: release with no preceding acquisition (names id %d)" % (e["seq"], aid))
            continue
        o = acqs[owner]
        if aid != owner or e["hart"] != o["cpu"] or rnd != o["round"]:
            fail("seq %d: release (hart %d, id %d, round %d) does not match the owning acquisition "
                 "(hart %d, id %d, round %d)" % (e["seq"], e["hart"], aid, rnd, o["cpu"], owner,
                                                 o["round"]))
        o["r"] = e["seq"]
        owner = None
if owner is not None:
    fail("acquisition %d (hart %d, seq %d) was never released"
         % (owner, acqs[owner]["cpu"], acqs[owner]["a"]))


def inside(e, aid):
    a = acqs.get(aid)
    return (a is not None and a["r"] is not None and a["cpu"] == e["hart"]
            and a["a"] < e["seq"] < a["r"])


# A CPU observing the lock held is inside its own lock() call, never inside its own ownership.
for c in by("contended"):
    if not 1 <= c["f"][1] <= ROUNDS:
        fail("seq %d: contention tagged with unscheduled round %d" % (c["seq"], c["f"][1]))
    for aid, a in acqs.items():
        if a["cpu"] == c["hart"] and a["r"] is not None and a["a"] < c["seq"] < a["r"]:
            fail("seq %d: hart %d recorded contention while it owned the lock (acquisition %d)"
                 % (c["seq"], c["hart"], aid))

for lk in by("linklost"):
    fail("seq %d: hart %d overwrote round %d's unresolved delivery link"
         % (lk["seq"], lk["hart"], lk["f"][0]))

# ── per-round obligations ────────────────────────────────────────────────────────────────────
credited = {0: 0, 1: 0}
delivered = {0: 0, 1: 0}
ssip_rounds = 0
armed = {}  # (hart, round) -> pending record with outstanding = 1


def round_fail(r, msg):
    fail("round %d: %s" % (r, msg))


for r in range(1, ROUNDS + 1):
    H = 0 if r % 2 == 1 else 1
    W = 1 - H
    g_h = [e for e in by("gate") if e["f"][0] == r and e["hart"] == H and e["f"][2] == 0]
    g_w = [e for e in by("gate") if e["f"][0] == r and e["hart"] == W and e["f"][2] == 1]
    gates = [e for e in by("gate") if e["f"][0] == r]
    dones = [e for e in by("done") if e["f"][0] == r]
    holds = [e for e in by("hold") if e["f"][1] == r]
    pends = [e for e in by("pending") if e["f"][0] == r]
    if (len(g_h) != 1 or len(g_w) != 1 or len(gates) != 2 or sorted(d["hart"] for d in dones) != [0, 1]
            or any(g["f"][1] != H for g in gates)):
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
        round_fail(r, "holder was not masked (sstatus.SIE set) inside the critical section")
        continue
    if not inside(hold, aid) or acqs[aid]["round"] != r:
        round_fail(r, "the hold falls outside its acquisition %d" % aid)
        continue
    if pend["f"][4] != aid or not inside(pend, aid) or pend["f"][1] != W:
        round_fail(r, "the pending observation is not the holder's, inside acquisition %d" % aid)
        continue
    if (hold["f"][4] >> 1) & 1:
        ssip_rounds += 1

    # ── contention, attributed by value ──
    k_seen, base = hold["f"][3], hold["f"][4] >> 8
    contended_credit = False
    if k_seen:
        con = [c for c in by("contended") if c["hart"] == W and c["f"][1] == r
               and base < c["f"][2] <= k_seen]
        if not con:
            round_fail(r, "the holder saw contention value %d that no waiter contention record "
                       "produced" % k_seen)
            continue
        c = min(con, key=lambda e: e["seq"])
        nxt = [a for a in acqs.values() if a["cpu"] == W and a["a"] > c["seq"]]
        if not nxt:
            round_fail(r, "the contender never acquired after observing the lock held")
            continue
        got = min(nxt, key=lambda a: a["a"])
        if got["round"] != r or got["r"] is None:
            round_fail(r, "the contender's acquisition is not this round's or never released")
            continue
        if g_w[0]["f"][3] == 1:
            contended_credit = True
            credited[W] += 1

    # ── delivery, attributed by generation ──
    ipi = [e for e in by("ipi") if e["f"][0] == r and e["hart"] == W]
    if pend["f"][2] == 1:
        armed[(H, r)] = pend
        if not contended_credit:
            continue
        good = [e for e in ipi if e["f"][1] == H and e["f"][2] == 1 and e["seq"] > g_w[0]["seq"]]
        if len(good) != 1:
            round_fail(r, "no production publication by the waiter to the holder after its gate")
            continue
        p = good[0]
        if p["seq"] > hold["seq"]:
            # The publication record precedes the waiter's contention bump (program order), which
            # the holder observed before recording its hold: the order is forced.
            round_fail(r, "the publication is not ordered before the hold that observed its "
                       "contention")
            continue
        if pend["f"][3] != p["f"][3]:
            round_fail(r, "the mailbox generation seen under the mask (%d) is not the round's "
                       "publication generation (%d)" % (pend["f"][3], p["f"][3]))
            continue
        delivered[W] += 1

# Every arrival must belong to an armed link, and every armed link must be discharged exactly by its
# first consumption on that CPU after the arming.
for a in by("arrival"):
    rnd = a["f"][0]
    if (a["hart"], rnd) not in armed:
        fail("seq %d: an arrival on hart %d names round %d's link, which was never armed there"
             % (a["seq"], a["hart"], rnd))
for (h, r), pend in sorted(armed.items(), key=lambda kv: kv[1]["seq"]):
    W = 1 - h
    arr = sorted([a for a in by("arrival") if a["hart"] == h and a["f"][0] == r],
                 key=lambda e: e["seq"])
    if not arr:
        round_fail(r, "the armed delivery link was never discharged (no consumption)")
        continue
    first = arr[0]
    if first["seq"] < pend["seq"]:
        round_fail(r, "a consumption precedes the masked observation that armed its link")
        continue
    # The holder can consume only after it unmasks: its release record (intent, recorded before the
    # unlocking store) -> the unlocking store -> interrupt restoration -> the trap -> the consumption,
    # all program order on one CPU, so the consumption's record follows the release record of the
    # acquisition the pending observation named. Earlier is a contradiction, not a valid race.
    held = acqs.get(pend["f"][4])
    if held is None or held["r"] is None or first["seq"] < held["r"]:
        round_fail(r, "the consumption (seq %d) precedes the release record (seq %s) of acquisition "
                   "%d, which its pending observation named" % (first["seq"],
                                                              held and held["r"], pend["f"][4]))
        continue
    if not (first["f"][4] & 1) or not ((first["f"][1] >> W) & 1) or (first["f"][4] >> 8) != W:
        round_fail(r, "the first consumption after arming did not swap out the waiter's bit "
                   "(sources 0x%x)" % first["f"][1])
        continue
    if first["f"][2] != pend["f"][3] or first["f"][3] < first["f"][2]:
        round_fail(r, "the consumption's generation is stale (link %d, snapshot %d, armed at %d)"
                   % (first["f"][2], first["f"][3], pend["f"][3]))
        continue
    if len(arr) > 1:
        round_fail(r, "the link was discharged more than once")

for d, who in ((1, "S (hart 1)"), (0, "C (hart 0)")):
    if credited[d] < MIN_PER_DIRECTION:
        fail("contended rounds with %s waiting: %d, need >= %d"
             % (who, credited[d], MIN_PER_DIRECTION))
    if delivered[d] < MIN_PER_DIRECTION:
        fail("attributed masked deliveries with %s publishing: %d, need >= %d"
             % (who, delivered[d], MIN_PER_DIRECTION))

for f in fails:
    print("[lock1-witness][fail] " + f)
ok = not fails
print("LOCK1_WITNESS_SEAL rounds=%d credited_c_waits=%d credited_s_waits=%d delivered_c=%d "
      "delivered_s=%d ssip_rounds=%d result=%s"
      % (ROUNDS, credited[0], credited[1], delivered[0], delivered[1], ssip_rounds,
         "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
