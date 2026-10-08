# SPDX-License-Identifier: Apache-2.0
"""The architecture-neutral half of the lock-contention witness graders (QEMU-LOCK1 on RISC-V,
QEMU-LOCK2 on AArch64): transport accounting of the sealed dump, the same-boot corroborating seal,
the ownership model over every recorded acquisition and release, and contention credited by value.
Each architecture's grader adds its own interrupt-delivery chain on top; nothing here knows about a
mailbox or an interrupt controller.

Every function reports through a caller-supplied `fail(msg)`; none of them exits.
"""
import re

KINDS_LOCK = {"acquire", "contended", "release", "hold"}
TAIL = re.compile(r"^(.*) pass=(\d+) crc=0x([0-9a-f]{8})$")


def fnv1a(s: str) -> int:
    h = 0x811C9DC5
    for b in s.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


def emit(body, pass_):
    """A dump line exactly as the kernel prints it."""
    return "%s pass=%d crc=0x%08x" % (body, pass_, fnv1a(body))


def rec_body(prefix, seq, kind, hart, f):
    return "%s_REC seq=%d kind=%s hart=%d f0=0x%x f1=0x%x f2=0x%x f3=0x%x f4=0x%x" % (
        prefix, seq, kind, hart, f[0], f[1], f[2], f[3], f[4])


def intact(l):
    """-> (body, pass) for a checksum-valid copy, else None."""
    mm = TAIL.match(l.strip())
    if not mm or fnv1a(mm.group(1)) != int(mm.group(3), 16):
        return None
    return mm.group(1), int(mm.group(2))


# ── corroboration: the workload's own seal on the same boot ─────────────────────────────────
def corroborating_seal(lines, verdict, fail):
    """Require exactly one distinct intact `<verdict> …` line, and it must say `result=ok`."""
    seen = {}
    for l in lines:
        if l.strip().startswith(verdict + " "):
            got = intact(l)
            if got is not None:
                seen.setdefault(got[0], 0)
    name = verdict.split("_")[0]
    if not seen:
        fail("no intact %s verdict on this boot" % name)
    elif len(seen) > 1:
        fail("conflicting checksum-valid %s verdicts: %s" % (name, sorted(seen)))
    elif not next(iter(seen)).startswith(verdict + " result=ok "):
        fail("the %s seal did not pass: " % name + next(iter(seen))[:120])


# ── transport accounting ─────────────────────────────────────────────────────────────────────
# The dump is printed in two passes; each pass is `<P>_META`, every record, any per-CPU summary
# lines, then `<P>_DUMP_DONE`, each line carrying `pass=N crc=0x…` over the text before ` pass=`. A
# copy whose checksum fails (or that is torn) is damaged and ignored; another intact copy of the same
# line recovers it. Identical intact copies are deduplicated; differing intact copies of one line are
# a contradiction. The metadata and completion records must be intact and agree on the record count,
# and the deduplicated records must be exactly the contiguous sequence 0..count-1 that the dump
# declares — no gap, no extra record.
def transport(lines, prefix, meta_rx, fail, summary=None):
    """-> (meta match or None, {seq: rec}, {summary key: body}).

    `meta_rx` must capture `slots_used` and `overflow` as named groups. `summary` is an optional
    `(regex, key_group)` for per-CPU summary lines (`<P>_COUNTS …`); one intact, non-conflicting line
    per key is kept."""
    done_rx = re.compile(r"^%s_DUMP_DONE records=(\d+)$" % prefix)
    rec_rx = re.compile(r"^%s_REC seq=(\d+) kind=(\w+) hart=(\d+) f0=0x([0-9a-f]+) "
                        r"f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)$"
                        % prefix)
    metas, dones, recs, sums = {}, {}, {}, {}
    first_meta = last_done = None
    for i, l in enumerate(lines):
        s = l.strip()
        if not s.startswith(prefix + "_"):
            continue
        got = intact(s)
        if got is None:
            continue
        body, pass_ = got
        if pass_ not in (1, 2):
            fail("line %d: an intact %s copy names dump pass %d" % (i + 1, prefix, pass_))
        if body.startswith(prefix + "_META "):
            if not meta_rx.match(body):
                fail("a checksum-valid %s_META line is malformed: %r" % (prefix, body[:100]))
                continue
            metas.setdefault(body, []).append(pass_)
            first_meta = i if first_meta is None else first_meta
        elif body.startswith(prefix + "_DUMP_DONE "):
            if not done_rx.match(body):
                fail("a checksum-valid %s_DUMP_DONE line is malformed: %r" % (prefix, body[:100]))
                continue
            dones.setdefault(body, []).append(pass_)
            last_done = i
        elif body.startswith(prefix + "_REC "):
            mm = rec_rx.match(body)
            if not mm:
                fail("a checksum-valid %s_REC line is malformed: %r" % (prefix, body[:100]))
                continue
            seq = int(mm.group(1))
            rec = dict(seq=seq, kind=mm.group(2), hart=int(mm.group(3)),
                       f=[int(mm.group(j), 16) for j in range(4, 9)], body=body)
            if seq in recs:
                if recs[seq]["body"] != body:
                    fail("conflicting checksum-valid copies of seq %d" % seq)
            else:
                recs[seq] = rec
        elif summary is not None and summary[0].match(body):
            key = summary[0].match(body).group(summary[1])
            if key in sums and sums[key] != body:
                fail("conflicting checksum-valid %s summary copies for %s" % (prefix, key))
            sums.setdefault(key, body)
        else:
            fail("an intact line of unknown %s type: %r" % (prefix, body[:80]))

    meta = None
    declared = None
    if not metas:
        fail("no intact %s_META line (the dump's metadata is missing or every copy is damaged)"
             % prefix)
    elif len(metas) > 1:
        fail("conflicting checksum-valid %s_META copies: %s" % (prefix, sorted(metas)))
    else:
        meta = meta_rx.match(next(iter(metas)))
        if int(meta.group("overflow")) != 0:
            fail("the %s record overflowed its ring (overflow=%s)" % (prefix, meta.group("overflow")))
        declared = int(meta.group("slots_used"))
    if not dones:
        fail("the dump is incomplete: no intact %s_DUMP_DONE completion record" % prefix)
    elif len(dones) > 1:
        fail("conflicting checksum-valid %s_DUMP_DONE copies: %s" % (prefix, sorted(dones)))
    else:
        done_n = int(done_rx.match(next(iter(dones))).group(1))
        if declared is not None and done_n != declared:
            fail("inconsistent counts: %s_META declares %d records, %s_DUMP_DONE %d"
                 % (prefix, declared, prefix, done_n))
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
    return meta, recs, sums


def check_events(ev, kinds, harts, lock_id, fail):
    for e in ev:
        if e["kind"] not in kinds:
            fail("unknown event kind %r at seq %d" % (e["kind"], e["seq"]))
        if e["hart"] not in harts:
            fail("seq %d recorded by unexpected hart %d" % (e["seq"], e["hart"]))
        if e["kind"] in KINDS_LOCK and e["f"][0] != lock_id:
            fail("seq %d: a lock event names lock %d, not the VM lock %d"
                 % (e["seq"], e["f"][0], lock_id))


# ── the ownership model (every recorded acquisition and release, in record order) ──────────
# Acquire records are taken AFTER the successful CAS and release records BEFORE the unlocking store
# (release INTENT). Release-record fetch_add -> Release store -> Acquire CAS -> next acquire-record
# fetch_add is a happens-before chain, so for one lock the acquire/release records must strictly
# alternate, each release naming (by acquisition id, CPU and round) the acquisition it ends.
def ownership(ev, rounds, fail):
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
                fail("seq %d: overlapping owners — hart %d acquired (id %d) while hart %d's "
                     "acquisition %d (seq %d) was unreleased"
                     % (e["seq"], e["hart"], aid, o["cpu"], owner, o["a"]))
            if not 1 <= rnd <= rounds:
                fail("seq %d: acquisition tagged with unscheduled round %d" % (e["seq"], rnd))
            acqs[aid] = dict(cpu=e["hart"], round=rnd, a=e["seq"], r=None)
            owner = aid
        else:
            if owner is None:
                fail("seq %d: release with no preceding acquisition (names id %d)"
                     % (e["seq"], aid))
                continue
            o = acqs[owner]
            if aid != owner or e["hart"] != o["cpu"] or rnd != o["round"]:
                fail("seq %d: release (hart %d, id %d, round %d) "
                     "does not match the owning acquisition (hart %d, id %d, round %d)"
                     % (e["seq"], e["hart"], aid, rnd, o["cpu"], owner, o["round"]))
            o["r"] = e["seq"]
            owner = None
    if owner is not None:
        fail("acquisition %d (hart %d, seq %d) was never released"
             % (owner, acqs[owner]["cpu"], acqs[owner]["a"]))
    return acqs


def inside(acqs, e, aid):
    """`e` was recorded by the owner of `aid`, strictly between its acquire and release records."""
    a = acqs.get(aid)
    return (a is not None and a["r"] is not None and a["cpu"] == e["hart"]
            and a["a"] < e["seq"] < a["r"])


def contention_outside_own(ev, acqs, rounds, fail):
    """A CPU observing the lock held is inside its own lock() call, never inside its own ownership."""
    for c in (e for e in ev if e["kind"] == "contended"):
        if not 1 <= c["f"][1] <= rounds:
            fail("seq %d: contention tagged with unscheduled round %d" % (c["seq"], c["f"][1]))
        for aid, a in acqs.items():
            if a["cpu"] == c["hart"] and a["r"] is not None and a["a"] < c["seq"] < a["r"]:
                fail("seq %d: hart %d recorded contention while it owned the lock (acquisition %d)"
                     % (c["seq"], c["hart"], aid))


def contention_credit(r, waiter, hold, ev, acqs):
    """Contention attributed BY VALUE: the holder's hold recorded the contention-counter value it saw
    advance while it owned the lock (`f3`, baseline in `f4 >> 8`); a waiter contended record must have
    produced a value in (baseline, k_seen], and the waiter's next acquisition must be this round's and
    released. -> (credited, failure message or None). `credited` is False with no failure for a valid
    uncontended round."""
    k_seen, base = hold["f"][3], hold["f"][4] >> 8
    if not k_seen:
        return False, None
    con = [c for c in ev if c["kind"] == "contended" and c["hart"] == waiter and c["f"][1] == r
           and base < c["f"][2] <= k_seen]
    if not con:
        return False, ("the holder saw contention value %d that "
                       "no waiter contention record produced" % k_seen)
    c = min(con, key=lambda e: e["seq"])
    nxt = [a for a in acqs.values() if a["cpu"] == waiter and a["a"] > c["seq"]]
    if not nxt:
        return False, "the contender never acquired after observing the lock held"
    got = min(nxt, key=lambda a: a["a"])
    if got["round"] != r or got["r"] is None:
        return False, "the contender's acquisition is not this round's or never released"
    return True, None
