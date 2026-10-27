#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SCHED-LOCK1 — grade the independence and wake witness (`kernel::schedlock1_witness`).

Usage: grade-sched-lock1-witness.py [--expect-cpus N] BOOT_LOG
       grade-sched-lock1-witness.py --self-test

Every record is VALIDATED before anything is awarded (CRC, contiguous sequence, known kind,
in-range CPUs, section structure, per-CPU generations, hold pairing). Then two separately
labelled verdicts:

* NATURAL (membership trace): the trace is replayed from the coherent SNAP and must arrive
  exactly at the coherent CENSUS. Every section's `before` current must equal the model's; every
  REM must remove a queued task; no task may hold two placements; an installed current must come
  off this CPU's queue (or be a restore from nowhere); every remote-wake CLAIM must be matched by
  the ADD it names (same target, same executing CPU, same task); every remote ADD must be followed
  by its notification from the same CPU to the same target.
* SYNCHRONIZED (independence holds): for every round of the schedule an attempt is awarded only
  if the holder's own 1L was held at begin and end with no other acquisition of it in between
  (equal generations), the holder held exactly one 1L and no admission lock, NO section touched
  the holder's CPU inside the window, and the observer made at least two owner commits on its own
  CPU — under a different lock instance — and one unit of progress by a task one of those commits
  installed.
"""
import re
import sys
import zlib

KINDS = {"SNAP_BEGIN", "SNAP_CUR", "SNAP_Q", "SEC", "ADD", "REM", "CLAIM", "NOTIFY",
         "HOLD_BEGIN", "PROGRESS", "HOLD_END", "RELATED", "CENSUS_BEGIN", "CENSUS_CUR",
         "CENSUS_Q", "CENSUS_END", "ALL_MUTATED"}
REC = re.compile(r"(SL1W_R seq=(\d+) kind=(\w+) cpu=(\d+) exec=(\d+) f0=(\d+) f1=(\d+) f2=(\d+) "
                 r"f3=(\d+) f4=(\d+) f5=(\d+)) crc=([0-9a-f]{8})")
END = re.compile(r"(SL1W_END count=(\d+) overflow=(\d) phase=(\d) ncpu=(\d+) rounds=(\d+) "
                 r"rounds_ok=(\d+)) crc=([0-9a-f]{8})")


class Fail(Exception):
    pass


def crc_ok(body, crc):
    return "%08x" % (zlib.crc32(body.encode()) & 0xFFFFFFFF) == crc


def parse(text):
    recs, end = {}, None
    for m in REC.finditer(text):
        if not crc_ok(m.group(1), m.group(12)):
            continue
        seq = int(m.group(2))
        r = {"seq": seq, "kind": m.group(3), "cpu": int(m.group(4)), "exec": int(m.group(5)),
             "f": [int(m.group(i)) for i in range(6, 12)]}
        if seq in recs and recs[seq] != r:
            raise Fail("validity: two different CRC-valid copies of seq %d" % seq)
        recs[seq] = r
    for m in END.finditer(text):
        if crc_ok(m.group(1), m.group(8)):
            end = {"count": int(m.group(2)), "overflow": int(m.group(3)), "phase": int(m.group(4)),
                   "ncpu": int(m.group(5)), "rounds": int(m.group(6)), "ok": int(m.group(7))}
    return recs, end


def pair(rnd, n):
    n = max(n, 2)
    r = rnd % (2 * n)
    a = r % n
    b = (a + 1) % n
    return (a, b) if r < n else (b, a)


def dec(v):
    return None if v == 0 else v - 1


def grade(text, expect_cpus=None):
    recs, end = parse(text)
    # ── validity ──────────────────────────────────────────────────────────────────────────────
    if end is None:
        raise Fail("validity: no CRC-valid SL1W_END")
    if end["overflow"]:
        raise Fail("validity: the record ring overflowed")
    if end["phase"] != 2:
        raise Fail("validity: the window never closed (phase=%d)" % end["phase"])
    n = end["ncpu"]
    if not 2 <= n <= 8 or (expect_cpus is not None and n != min(expect_cpus, 8)):
        raise Fail("validity: ncpu=%d (expected %s)" % (n, expect_cpus))
    rounds = 2 if n == 2 else 2 * n
    if end["rounds"] != rounds:
        raise Fail("validity: %d rounds recorded, schedule has %d" % (end["rounds"], rounds))
    if sorted(recs) != list(range(1, end["count"] + 1)):
        missing = sorted(set(range(1, end["count"] + 1)) - set(recs))
        raise Fail("validity: records missing or extra (first missing %s)" % missing[:5])
    rs = [recs[i] for i in range(1, end["count"] + 1)]
    for r in rs:
        if r["kind"] not in KINDS:
            raise Fail("validity: seq %d unknown kind %s" % (r["seq"], r["kind"]))
        if r["cpu"] >= n or r["exec"] >= n:
            raise Fail("validity: seq %d names a CPU outside 0..%d" % (r["seq"], n - 1))
        if r["kind"] == "ALL_MUTATED":
            raise Fail("validity: a coherent view mutated membership (seq %d)" % r["seq"])
    if rs[0]["kind"] != "SNAP_BEGIN" or rs[-1]["kind"] != "CENSUS_END":
        raise Fail("validity: the trace must run SNAP_BEGIN ... CENSUS_END")
    # snapshot / census blocks
    def block(start_kind, cur_kind, q_kind, i):
        if rs[i]["kind"] != start_kind or rs[i]["f"][0] != n:
            raise Fail("validity: %s malformed at seq %d" % (start_kind, rs[i]["seq"]))
        i += 1
        cur, q = {}, {c: {} for c in range(n)}
        while i < len(rs) and rs[i]["kind"] in (cur_kind, q_kind):
            r = rs[i]
            if r["kind"] == cur_kind:
                if r["cpu"] in cur:
                    raise Fail("validity: two %s for cpu %d" % (cur_kind, r["cpu"]))
                cur[r["cpu"]] = dec(r["f"][0])
            else:
                if r["cpu"] not in cur:
                    raise Fail("validity: %s before its CPU's current" % q_kind)
                q[r["cpu"]][r["f"][0]] = r["f"][1]
            i += 1
        if sorted(cur) != list(range(n)):
            raise Fail("validity: %s does not cover every CPU" % start_kind)
        return cur, q, i
    snap_cur, snap_q, i = block("SNAP_BEGIN", "SNAP_CUR", "SNAP_Q", 0)
    ci = max(k for k, r in enumerate(rs) if r["kind"] == "CENSUS_BEGIN")
    cen_cur, cen_q, j = block("CENSUS_BEGIN", "CENSUS_CUR", "CENSUS_Q", ci)
    if j != len(rs) - 1:
        raise Fail("validity: records between the census and CENSUS_END")
    if rs[-1]["f"][0] != end["ok"] or rs[-1]["f"][1] != rounds:
        raise Fail("validity: CENSUS_END disagrees with SL1W_END")
    body = rs[i:ci]
    # sections: SEC followed by exactly n_add ADD and n_rem REM of the same (cpu, exec, gen)
    secs, k, last_gen = [], 0, {}
    while k < len(body):
        r = body[k]
        if r["kind"] in ("ADD", "REM"):
            raise Fail("validity: seq %d %s outside a section" % (r["seq"], r["kind"]))
        if r["kind"] != "SEC":
            k += 1
            continue
        gen, bcur, acur, na, nr, inst = r["f"]
        part = body[k + 1:k + 1 + na + nr]
        if len(part) != na + nr or [p["kind"] for p in part] != ["ADD"] * na + ["REM"] * nr:
            raise Fail("validity: section seq %d is not followed by its %d ADD/%d REM" % (r["seq"], na, nr))
        for p in part:
            if (p["cpu"], p["exec"], p["f"][0]) != (r["cpu"], r["exec"], gen):
                raise Fail("validity: section seq %d has a foreign member seq %d" % (r["seq"], p["seq"]))
        tids = [p["f"][1] for p in part]
        if len(set(tids)) != len(tids):
            raise Fail("validity: section seq %d lists a task twice (a net change cannot)" % r["seq"])
        if gen <= last_gen.get(r["cpu"], 0):
            raise Fail("validity: cpu %d generations not increasing at seq %d" % (r["cpu"], r["seq"]))
        last_gen[r["cpu"]] = gen
        secs.append({"seq": r["seq"], "cpu": r["cpu"], "exec": r["exec"], "gen": gen,
                     "before": dec(bcur), "after": dec(acur), "inst": inst,
                     "add": {p["f"][1]: p["f"][2] for p in part if p["kind"] == "ADD"},
                     "rem": {p["f"][1] for p in part if p["kind"] == "REM"}})
        k += 1 + na + nr
    # ── NATURAL: replay ──────────────────────────────────────────────────────────────────────
    cur = dict(snap_cur)
    q = {c: dict(snap_q[c]) for c in range(n)}

    def placements(tid):
        return [("cur", c) for c in range(n) if cur[c] == tid] + [("q", c) for c in range(n) if tid in q[c]]

    for tid in {t for c in range(n) for t in q[c]} | {t for t in cur.values() if t is not None}:
        if tid != 0 and len(placements(tid)) > 1:
            raise Fail("natural: SNAP places tid %d twice" % tid)
    restores = 0
    for s in secs:
        c = s["cpu"]
        if cur[c] != s["before"]:
            raise Fail("natural: seq %d cpu %d section starts from current %s, model has %s (a lost or "
                       "unrecorded transition)" % (s["seq"], c, s["before"], cur[c]))
        for tid in s["rem"]:
            if tid not in q[c]:
                raise Fail("natural: seq %d removes tid %d not queued on cpu %d (duplicate consumption)"
                           % (s["seq"], tid, c))
            del q[c][tid]
        for tid, prio in s["add"].items():
            if tid in q[c]:
                raise Fail("natural: seq %d queues tid %d twice on cpu %d" % (s["seq"], tid, c))
            q[c][tid] = prio
        if s["after"] != s["before"] and s["after"] is not None:
            a = s["after"]
            if a not in s["rem"] and a != 0:
                if placements(a):
                    raise Fail("natural: seq %d installs tid %d, which is placed elsewhere" % (s["seq"], a))
                restores += 1
        cur[c] = s["after"]
        for tid in {s["before"], s["after"], *s["add"], *s["rem"]}:
            if tid not in (None, 0) and len(placements(tid)) > 1:
                raise Fail("natural: after seq %d tid %d holds %d placements %s"
                           % (s["seq"], tid, len(placements(tid)), placements(tid)))
    for c in range(n):
        if cur[c] != cen_cur[c] or q[c] != cen_q[c]:
            raise Fail("natural: the replay ends at cpu %d current=%s queue=%s; the CENSUS has current=%s "
                       "queue=%s (a lost, phantom or substituted wake)" % (c, cur[c], sorted(q[c]),
                                                                         cen_cur[c], sorted(cen_q[c])))
    # claims ↔ adds
    claimed = set()
    for r in body:
        if r["kind"] != "CLAIM":
            continue
        t, e, tid = r["cpu"], r["exec"], r["f"][0]
        cands = [s for s in secs if s["seq"] < r["seq"] and s["cpu"] == t and s["exec"] == e
                 and tid in s["add"] and s["seq"] not in claimed]
        if not cands:
            raise Fail("natural: CLAIM seq %d (cpu %d → cpu %d tid %d) has no ADD it names (dropped "
                       "publication, or substituted destination/incarnation)" % (r["seq"], e, t, tid))
        claimed.add(cands[-1]["seq"])
    # remote adds → notification
    pending, remote_adds, notifies = {}, 0, 0
    for idx, r in enumerate(body):
        if r["kind"] == "SEC" and r["exec"] != r["cpu"] and r["f"][3] > 0:
            pending.setdefault((r["exec"], r["cpu"]), []).append(r["seq"])
            remote_adds += 1
        elif r["kind"] == "NOTIFY":
            notifies += 1
            lst = pending.get((r["exec"], r["cpu"]))
            if lst:
                lst.pop(0)
    for (e, t), lst in pending.items():
        for seq in lst:
            later = [s for s in secs if s["exec"] == e and s["seq"] > seq]
            if later:
                raise Fail("natural: remote ADD seq %d (cpu %d → cpu %d) was never notified" % (seq, e, t))
    # ── SYNCHRONIZED: holds ──────────────────────────────────────────────────────────────────
    attempts, open_hold = [], None
    for r in body:
        if r["kind"] == "HOLD_BEGIN":
            if open_hold is not None:
                raise Fail("validity: nested hold at seq %d" % r["seq"])
            open_hold = {"begin": r, "progress": [], "related": []}
        elif r["kind"] in ("PROGRESS", "RELATED"):
            if open_hold is None or r["f"][0] != open_hold["begin"]["f"][0]:
                raise Fail("validity: %s seq %d outside its hold" % (r["kind"], r["seq"]))
            open_hold["progress" if r["kind"] == "PROGRESS" else "related"].append(r)
        elif r["kind"] == "HOLD_END":
            if open_hold is None or r["f"][0] != open_hold["begin"]["f"][0] or r["cpu"] != open_hold["begin"]["cpu"]:
                raise Fail("validity: unpaired HOLD_END seq %d" % r["seq"])
            open_hold["end"] = r
            attempts.append(open_hold)
            open_hold = None
    if open_hold is not None:
        raise Fail("validity: a hold never ended")
    awarded = {}
    for at in attempts:
        b, e = at["begin"], at["end"]
        rnd = b["f"][0]
        a, obs = pair(rnd, n)
        if b["cpu"] != a or b["f"][1] != obs:
            raise Fail("validity: round %d held by cpu %d for %d, schedule says %d for %d"
                       % (rnd, b["cpu"], b["f"][1], a, obs))
        ok_flag = (e["f"][5] >> 1) & 1
        win = [s for s in secs if b["seq"] < s["seq"] < e["seq"]]
        if any(s["cpu"] == a for s in win):
            raise Fail("synchronized: round %d — a section on the HOLDER's cpu %d inside its hold "
                       "(its lock did not exclude)" % (rnd, a))
        if not ok_flag:
            continue
        held_1l, held_1g = b["f"][4] & 0xFFFFFFFF, b["f"][4] >> 32
        if (b["f"][5] & 1) != 1 or (e["f"][5] & 1) != 1:
            raise Fail("synchronized: round %d — the holder's lock was not held at begin and end" % rnd)
        if (e["f"][5] >> 8) != b["f"][3]:
            raise Fail("synchronized: round %d — the holder's lock was re-acquired during the hold" % rnd)
        if held_1l != 1 or held_1g != 0:
            raise Fail("synchronized: round %d — the holder held 1L=%d 1G=%d (exactly its own 1L "
                       "required)" % (rnd, held_1l, held_1g))
        commits = [s for s in win if s["cpu"] == obs and s["exec"] == obs]
        if any(s["inst"] == b["f"][2] for s in commits):
            raise Fail("synchronized: round %d — the observer's commits used the holder's lock" % rnd)
        prog = [p for p in at["progress"] if p["cpu"] == obs and any(
            s["after"] == p["f"][1] and s["gen"] == p["f"][3] and s["seq"] < p["seq"]
            and p["f"][2] > s["gen"] for s in commits)]
        if len(commits) >= 2 and prog and not at["related"]:
            awarded.setdefault(rnd, (a, obs, len(commits), len(prog)))
    missing = [r for r in range(rounds) if r not in awarded]
    if missing:
        raise Fail("synchronized: rounds %s were never awarded (schedule %s)"
                   % (missing, [pair(r, n) for r in missing]))
    dirs = sorted({(a, b) for a, b, _, _ in awarded.values()})
    return ("SL1W_GRADE result=pass ncpu=%d | SYNCHRONIZED holds: %d/%d rounds awarded over %d attempts, "
            "directions %s | NATURAL trace: %d sections (%d remote adds, %d notifications, %d claims "
            "matched, %d restores) replayed SNAP→CENSUS exactly"
            % (n, len(awarded), rounds, len(attempts), dirs, len(secs), remote_adds, notifies,
               len(claimed), restores))


# ── self-test ─────────────────────────────────────────────────────────────────────────────────
def line(seq, kind, cpu, ex, f):
    body = "SL1W_R seq=%d kind=%s cpu=%d exec=%d f0=%d f1=%d f2=%d f3=%d f4=%d f5=%d" % (
        seq, kind, cpu, ex, *f)
    return "%s crc=%08x" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)


def build(events, ncpu=2, phase=2, overflow=0, ok=2):
    out = [line(i + 1, k, c, e, f + [0] * (6 - len(f))) for i, (k, c, e, f) in enumerate(events)]
    endb = "SL1W_END count=%d overflow=%d phase=%d ncpu=%d rounds=%d rounds_ok=%d" % (
        len(events), overflow, phase, ncpu, 2 if ncpu == 2 else 2 * ncpu, ok)
    out.append("%s crc=%08x" % (endb, zlib.crc32(endb.encode()) & 0xFFFFFFFF))
    return "\n".join(o + "\n" + o for o in out)


def good():
    # Two CPUs; T=100 runs on cpu 1; cpu 0 wakes T2=200 onto cpu 1; both rounds held.
    ev = [("SNAP_BEGIN", 0, 0, [2, 3, 3, 10, 0, 0]),
          ("SNAP_CUR", 0, 0, [0, 5]), ("SNAP_CUR", 1, 1, [101, 7]), ("SNAP_Q", 1, 1, [300, 1])]
    # round 0: cpu 0 holds; cpu 1 preempts 100 (requeue), dispatches 300, ticks with 300 current
    ev += [("HOLD_BEGIN", 0, 0, [0, 1, 0xA0, 6, 1, 1]),
           ("SEC", 1, 1, [8, 101, 0, 1, 0, 0xB0]), ("ADD", 1, 1, [8, 100, 1]),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, 0xB0]), ("REM", 1, 1, [9, 300]),
           ("PROGRESS", 1, 1, [0, 300, 10, 9]),
           ("HOLD_END", 0, 0, [0, 2, 1, 0, 50, 1 | 2 | (6 << 8)])]
    # natural: cpu 0 wakes 200 onto cpu 1 and notifies; the claim names it
    ev += [("SEC", 1, 0, [11, 301, 301, 1, 0, 0xB0]), ("ADD", 1, 0, [11, 200, 1]),
           ("NOTIFY", 1, 0, [11]), ("CLAIM", 1, 0, [200, 9, 0])]
    # round 1: cpu 1 holds; cpu 0 installs 400 from nowhere? no — cpu 0 dispatches its queued 500
    ev += [("SEC", 0, 0, [7, 0, 0, 1, 0, 0xA0]), ("ADD", 0, 0, [7, 500, 1]),
           ("HOLD_BEGIN", 1, 1, [1, 0, 0xB0, 12, 1, 1]),
           ("SEC", 0, 0, [8, 0, 501, 0, 1, 0xA0]), ("REM", 0, 0, [8, 500]),
           ("SEC", 0, 0, [9, 501, 501, 1, 0, 0xA0]), ("ADD", 0, 0, [9, 600, 1]),
           ("PROGRESS", 0, 0, [1, 500, 10, 8]),
           ("HOLD_END", 1, 1, [1, 2, 1, 0, 70, 1 | 2 | (12 << 8)])]
    ev += [("CENSUS_BEGIN", 0, 0, [2, 3, 3, 20, 2, 2]),
           ("CENSUS_CUR", 0, 0, [501]), ("CENSUS_Q", 0, 0, [600, 1]),
           ("CENSUS_CUR", 1, 1, [301]), ("CENSUS_Q", 1, 1, [100, 1]), ("CENSUS_Q", 1, 1, [200, 1]),
           ("CENSUS_END", 0, 0, [2, 2])]
    return ev


def mutate(ev, fn):
    ev = [list(e) for e in ev]
    for e in ev:
        e[3] = list(e[3])
    fn(ev)
    return [tuple(e) for e in ev]


def find(ev, kind, k=0):
    return [i for i, e in enumerate(ev) if e[0] == kind][k]


def self_test():
    cases = []
    g = good()
    cases.append(("valid trace", build(g), None))

    def drop_add(ev):  # the publication claimed but never queued (dropped / stranded wake)
        i = [i for i, e in enumerate(ev) if e[0] == "ADD" and e[3][1] == 200][0]
        del ev[i]
        ev[i - 1][3][3] = 0
        ev[:] = [e for e in ev if not (e[0] == "CENSUS_Q" and e[3][0] == 200)]
    cases.append(("dropped publication", build(mutate(g, drop_add)), "natural:"))

    def wrong_dest(ev):  # the claim names cpu 1, the wake landed on cpu 0
        i = find(ev, "CLAIM")
        ev[i][1] = 0
    cases.append(("substituted destination", build(mutate(g, wrong_dest)), "natural: CLAIM"))

    def wrong_tid(ev):
        ev[find(ev, "CLAIM")][3][0] = 201
    cases.append(("substituted incarnation", build(mutate(g, wrong_tid)), "natural: CLAIM"))

    def dup(ev):  # 300 consumed twice
        i = find(ev, "REM")
        ev.insert(i + 1, ("REM", 1, 1, [9, 300]))
        ev[i - 1][3][4] = 2
    cases.append(("duplicate consumption", build(mutate(g, dup)), "lists a task twice"))

    def dup2(ev):  # a second section consuming 300 again
        i = find(ev, "PROGRESS")
        ev.insert(i, ("REM", 1, 1, [10, 300]))
        ev.insert(i, ("SEC", 1, 1, [10, 301, 301, 0, 1, 0xB0]))
    cases.append(("duplicate consumption (second section)", build(mutate(g, dup2)), "duplicate consumption"))

    def no_notify(ev):
        del ev[find(ev, "NOTIFY")]
    cases.append(("dropped wake notification", build(mutate(g, no_notify)), "never notified"))

    def serialized(ev):  # global serialization: the observer could not commit during the hold
        del ev[find(ev, "PROGRESS")]
        b, e = find(ev, "HOLD_BEGIN"), find(ev, "HOLD_END")
        end = ev.pop(e)
        end[3][1], end[3][2], end[3][5] = 0, 0, 1 | (6 << 8)
        ev.insert(b + 1, end)
    cases.append(("global serialization", build(mutate(g, serialized)), "synchronized: rounds"))

    def holder_section(ev):  # a section on the holder's CPU inside its hold
        b = find(ev, "HOLD_BEGIN", 1)
        ev.insert(b + 1, ("SEC", 1, 1, [13, 301, 301, 1, 0, 0xB0]))
        ev.insert(b + 2, ("ADD", 1, 1, [13, 700, 1]))
        ev[find(ev, "CENSUS_END") - 1:find(ev, "CENSUS_END") - 1] = []
        ev.insert(find(ev, "CENSUS_END"), ("CENSUS_Q", 1, 1, [700, 1]))
    cases.append(("holder lock did not exclude", build(mutate(g, holder_section)), "HOLDER"))

    def reacquired(ev):
        e = find(ev, "HOLD_END")
        ev[e][3][5] = 1 | 2 | (7 << 8)
    cases.append(("holder lock re-acquired", build(mutate(g, reacquired)), "re-acquired"))

    def two_locks(ev):
        ev[find(ev, "HOLD_BEGIN")][3][4] = 2
    cases.append(("holder held a second lock", build(mutate(g, two_locks)), "exactly its own 1L"))

    def census_mismatch(ev):
        i = [i for i, e in enumerate(ev) if e[0] == "CENSUS_Q" and e[3][0] == 100][0]
        del ev[i]
    cases.append(("lost wake at census", build(mutate(g, census_mismatch)), "CENSUS"))

    def no_progress(ev):
        del ev[find(ev, "PROGRESS")]
    cases.append(("no task progress", build(mutate(g, no_progress)), "never awarded"))

    cases.append(("overflow", build(g, overflow=1), "overflowed"))
    cases.append(("window not closed", build(g, phase=1), "never closed"))
    torn = build(g).replace("kind=SEC cpu=1 exec=0", "kind=SEC cpu=1 exec=1")
    cases.append(("torn record (bad CRC)", torn, "validity: records missing"))
    bad = 0
    for name, text, want in cases:
        try:
            out = grade(text)
            got = None
        except Fail as e:
            out, got = str(e), str(e)
        verdict = (got is None) if want is None else (got is not None and want in got)
        print("[%s] %-40s %s" % ("ok" if verdict else "BAD", name, out))
        bad += 0 if verdict else 1
    accepted = sum(1 for _, _, w in cases if w is None)
    print("SL1W self-test: %d cases, %d accepted, %d rejected, %d wrong" % (
        len(cases), accepted, len(cases) - accepted, bad))
    return bad == 0


def main(argv):
    if argv[1:] == ["--self-test"]:
        return 0 if self_test() else 1
    expect = None
    args = argv[1:]
    if args[:1] == ["--expect-cpus"]:
        expect = int(args[1])
        args = args[2:]
    if len(args) != 1:
        print(__doc__)
        return 2
    text = open(args[0], encoding="utf-8", errors="replace").read().replace("\r", "\n")
    try:
        print(grade(text, expect))
        return 0
    except Fail as e:
        print("SL1W_GRADE result=fail reason=%s" % e)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
