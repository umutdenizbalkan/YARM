#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SCHED-LOCK1 — grade the independence and wake witness (`kernel::schedlock1_witness`).

Usage: grade-sched-lock1-witness.py [--expect-cpus N] BOOT_LOG
       grade-sched-lock1-witness.py --self-test

A failure prints `SL1W_GRADE result=fail reason=<obligation>: <detail>`. The obligations, in the
order they are checked:

* `validity` — transport and structure: CRC, identical redundant copies, contiguous sequence,
  known kinds, CPUs in range, the SNAP ... CENSUS framing, section membership, generations.
* `cell-map` — the authoritative CPU → lock-instance map (`CELL`, recorded from the domain's cell
  array at SNAP and again at CENSUS): complete, distinct per CPU, one lock offset, unchanged.
* `lock-instance` — every `SEC` and every `HOLD_BEGIN` (credited or not) names its CPU's cell.
* `no-op-section` — the producer never records a section with an unchanged current, no
  addition and no removal; such a record is not a scheduling commit and is refused.
* `natural` — the replay from SNAP reaches CENSUS exactly (one placement per task, no lost,
  duplicated or phantom wake, every remote addition notified).
* `claim-commit` — every `CLAIM` names the exact section (record number, CPU, executing CPU,
  generation) that added its task, and each such addition is claimed at most once.
* `claim-incarnation` — every `CLAIM` carries the incarnation its owner staged under rank 2, and
  that incarnation agrees with the address space the CPU had active when the task next ran.
* `run` — every `RUN` (the first kernel entry by an installed task: its tick, call or fault)
  names the installation it observed, once, after it, on the same CPU.
* `attempt` — EVERY hold attempt is valid before any coverage is counted: the schedule's pair and
  attempt order, the holder's cell, its lock held and interrupts masked at begin and end, exactly
  its own 1L and no admission lock, no reacquisition, holder sections ordered around it, and a
  reported outcome consistent with its own counters and the fixed bounds.
* `progress` — every `PROGRESS` names an installation by the observer's own production owner, in
  its hold, of the task it reports, with the running address space.
* `coverage` — every scheduled round has a credited attempt. A valid attempt is credited when it
  reported success, its window holds at least two distinct genuine commits by the observer on its
  own cell, one of them installed (selected or restored) a task, and that task's execution was
  observed in the hold. A valid attempt may be uncredited; that alone is not a failure.

The round population and the thresholds are fixed here, from the CPU count; the log cannot
redefine them.
"""
import re
import sys
import zlib

KINDS = {"SNAP_BEGIN", "SNAP_CUR", "SNAP_Q", "SEC", "ADD", "REM", "CLAIM", "NOTIFY",
         "HOLD_BEGIN", "PROGRESS", "HOLD_END", "RELATED", "CENSUS_BEGIN", "CENSUS_CUR",
         "CENSUS_Q", "CENSUS_END", "ALL_MUTATED", "CELL", "RUN"}
REC = re.compile(r"(SL1W_R seq=(\d+) kind=(\w+) cpu=(\d+) exec=(\d+) f0=(\d+) f1=(\d+) f2=(\d+) "
                 r"f3=(\d+) f4=(\d+) f5=(\d+)) crc=([0-9a-f]{8})")
END = re.compile(r"(SL1W_END count=(\d+) overflow=(\d) phase=(\d) ncpu=(\d+) rounds=(\d+) "
                 r"rounds_ok=(\d+)) crc=([0-9a-f]{8})")

# Fixed by the producer (`kernel::schedlock1_witness`): never read from the log.
HOLD_SPINS = 400_000_000
ATTEMPTS = 3
COMMITS_REQUIRED = 2
INC_UNBOUND, INC_NO_ASID = 0, 1


class Fail(Exception):
    pass


def fail(obligation, detail):
    raise Fail("%s: %s" % (obligation, detail))


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
            fail("validity", "two different CRC-valid copies of seq %d" % seq)
        recs[seq] = r
    for m in END.finditer(text):
        if crc_ok(m.group(1), m.group(8)):
            e = {"count": int(m.group(2)), "overflow": int(m.group(3)), "phase": int(m.group(4)),
                 "ncpu": int(m.group(5)), "rounds": int(m.group(6)), "ok": int(m.group(7))}
            if end is not None and end != e:
                fail("validity", "two different CRC-valid SL1W_END lines")
            end = e
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
    # ── validity: transport, completeness, framing ───────────────────────────────────────────
    if end is None:
        fail("validity", "no CRC-valid SL1W_END")
    if end["overflow"]:
        fail("validity", "the record ring overflowed")
    if end["phase"] != 2:
        fail("validity", "the window never closed (phase=%d)" % end["phase"])
    n = end["ncpu"]
    if not 2 <= n <= 8 or (expect_cpus is not None and n != min(expect_cpus, 8)):
        fail("validity", "ncpu=%d (expected %s)" % (n, expect_cpus))
    rounds = 2 if n == 2 else 2 * n
    if end["rounds"] != rounds:
        fail("validity", "%d rounds recorded, schedule has %d" % (end["rounds"], rounds))
    if sorted(recs) != list(range(1, end["count"] + 1)):
        missing = sorted(set(range(1, end["count"] + 1)) - set(recs))
        fail("validity", "records missing or extra (first missing %s)" % missing[:5])
    rs = [recs[i] for i in range(1, end["count"] + 1)]
    by_seq = {r["seq"]: r for r in rs}
    for r in rs:
        if r["kind"] not in KINDS:
            fail("validity", "seq %d unknown kind %s" % (r["seq"], r["kind"]))
        if r["cpu"] >= n or r["exec"] >= n:
            fail("validity", "seq %d names a CPU outside 0..%d" % (r["seq"], n - 1))
        if r["kind"] == "ALL_MUTATED":
            fail("validity", "a coherent view mutated membership (seq %d)" % r["seq"])
    if rs[0]["kind"] != "SNAP_BEGIN" or rs[-1]["kind"] != "CENSUS_END":
        fail("validity", "the trace must run SNAP_BEGIN ... CENSUS_END")

    def block(start_kind, cur_kind, q_kind, i, phase):
        if rs[i]["kind"] != start_kind or rs[i]["f"][0] != n:
            fail("validity", "%s malformed at seq %d" % (start_kind, rs[i]["seq"]))
        i += 1
        cells = {}
        while i < len(rs) and rs[i]["kind"] == "CELL":
            r = rs[i]
            if r["cpu"] in cells or r["f"][2] != phase:
                fail("cell-map", "%s: CELL seq %d repeats cpu %d or names phase %d"
                     % (start_kind, r["seq"], r["cpu"], r["f"][2]))
            cells[r["cpu"]] = (r["f"][0], r["f"][1])
            i += 1
        if sorted(cells) != list(range(n)):
            fail("cell-map", "%s does not map every CPU to its cell (has %s)" % (start_kind, sorted(cells)))
        cur, q = {}, {c: {} for c in range(n)}
        while i < len(rs) and rs[i]["kind"] in (cur_kind, q_kind):
            r = rs[i]
            if r["kind"] == cur_kind:
                if r["cpu"] in cur:
                    fail("validity", "two %s for cpu %d" % (cur_kind, r["cpu"]))
                cur[r["cpu"]] = (dec(r["f"][0]), r["f"][1])
            else:
                if r["cpu"] not in cur:
                    fail("validity", "%s before its CPU's current" % q_kind)
                q[r["cpu"]][r["f"][0]] = r["f"][1]
            i += 1
        if sorted(cur) != list(range(n)):
            fail("validity", "%s does not cover every CPU" % start_kind)
        return cells, cur, q, i

    snap_cells, snap_cur, snap_q, i = block("SNAP_BEGIN", "SNAP_CUR", "SNAP_Q", 0, 0)
    ci = max(k for k, r in enumerate(rs) if r["kind"] == "CENSUS_BEGIN")
    cen_cells, cen_cur, cen_q, j = block("CENSUS_BEGIN", "CENSUS_CUR", "CENSUS_Q", ci, 1)
    if j != len(rs) - 1:
        fail("validity", "records between the census and CENSUS_END")
    if rs[-1]["f"][0] != end["ok"] or rs[-1]["f"][1] != rounds:
        fail("validity", "CENSUS_END disagrees with SL1W_END")
    # ── cell-map: one authoritative CPU → lock-instance map ──────────────────────────────────
    if snap_cells != cen_cells:  # repair:cell-map
        fail("cell-map", "a CPU's cell changed inside the window (SNAP %s, CENSUS %s)"
             % (snap_cells, cen_cells))
    cell = {c: snap_cells[c][0] for c in range(n)}
    distinct = 0 not in cell.values() and len(set(cell.values())) == n and len({l for _, l in snap_cells.values()}) == n
    if not distinct:  # repair:cell-map
        fail("cell-map", "CPU cells are not distinct non-null instances %s" % snap_cells)
    if len({l - c for c, l in snap_cells.values()}) != 1:  # repair:cell-map
        fail("cell-map", "the lock does not sit at one offset in every cell %s" % snap_cells)
    body = rs[i:ci]
    # ── sections ──────────────────────────────────────────────────────────────────────────────
    secs, k, last_gen = [], 0, {c: snap_cur[c][1] for c in range(n)}
    sec_by_seq = {}
    while k < len(body):
        r = body[k]
        if r["kind"] in ("ADD", "REM"):
            fail("validity", "seq %d %s outside a section" % (r["seq"], r["kind"]))
        if r["kind"] != "SEC":
            k += 1
            continue
        gen, bcur, acur, na, nr, inst = r["f"]
        part = body[k + 1:k + 1 + na + nr]
        if len(part) != na + nr or [p["kind"] for p in part] != ["ADD"] * na + ["REM"] * nr:
            fail("validity", "section seq %d is not followed by its %d ADD/%d REM" % (r["seq"], na, nr))
        for p in part:
            if (p["cpu"], p["exec"], p["f"][0]) != (r["cpu"], r["exec"], gen):
                fail("validity", "section seq %d has a foreign member seq %d" % (r["seq"], p["seq"]))
        tids = [p["f"][1] for p in part]
        if len(set(tids)) != len(tids):
            fail("validity", "section seq %d lists a task twice (a net change cannot)" % r["seq"])
        if gen <= last_gen[r["cpu"]]:
            fail("validity", "cpu %d generations not increasing at seq %d" % (r["cpu"], r["seq"]))
        last_gen[r["cpu"]] = gen
        if inst != cell[r["cpu"]]:  # repair:lock-instance
            fail("lock-instance", "section seq %d on cpu %d names lock instance %#x; cpu %d's cell is %#x"
                 % (r["seq"], r["cpu"], inst, r["cpu"], cell[r["cpu"]]))
        if bcur == acur and na == 0 and nr == 0:  # repair:no-op-section
            fail("no-op-section", "section seq %d on cpu %d changes nothing (current %s kept, 0 added, "
                 "0 removed): the producer never records one, and it is no scheduling commit"
                 % (r["seq"], r["cpu"], dec(bcur)))
        s = {"seq": r["seq"], "cpu": r["cpu"], "exec": r["exec"], "gen": gen,
             "before": dec(bcur), "after": dec(acur),
             "add": {p["f"][1]: p["f"][2] for p in part if p["kind"] == "ADD"},
             "rem": {p["f"][1] for p in part if p["kind"] == "REM"}}
        secs.append(s)
        sec_by_seq[r["seq"]] = s
        k += 1 + na + nr
    # ── NATURAL: replay ──────────────────────────────────────────────────────────────────────
    cur = {c: snap_cur[c][0] for c in range(n)}
    q = {c: dict(snap_q[c]) for c in range(n)}

    def placements(tid):
        return [("cur", c) for c in range(n) if cur[c] == tid] + [("q", c) for c in range(n) if tid in q[c]]

    for tid in {t for c in range(n) for t in q[c]} | {t for t in cur.values() if t is not None}:
        if tid != 0 and len(placements(tid)) > 1:
            fail("natural", "SNAP places tid %d twice" % tid)
    restores = 0
    for s in secs:
        c = s["cpu"]
        if cur[c] != s["before"]:
            fail("natural", "seq %d cpu %d section starts from current %s, model has %s (a lost or "
                 "unrecorded transition)" % (s["seq"], c, s["before"], cur[c]))
        for tid in s["rem"]:
            if tid not in q[c]:
                fail("natural", "seq %d removes tid %d not queued on cpu %d (duplicate consumption)"
                     % (s["seq"], tid, c))
            del q[c][tid]
        for tid, prio in s["add"].items():
            if tid in q[c]:
                fail("natural", "seq %d queues tid %d twice on cpu %d" % (s["seq"], tid, c))
            q[c][tid] = prio
        s["install"] = None
        if s["after"] != s["before"] and s["after"] is not None:
            a = s["after"]
            s["install"] = "selected" if a in s["rem"] else "restored"
            if a not in s["rem"] and a != 0:
                if placements(a):
                    fail("natural", "seq %d installs tid %d, which is placed elsewhere" % (s["seq"], a))
                restores += 1
        cur[c] = s["after"]
        for tid in {s["before"], s["after"], *s["add"], *s["rem"]}:
            if tid not in (None, 0) and len(placements(tid)) > 1:
                fail("natural", "after seq %d tid %d holds %d placements %s"
                     % (s["seq"], tid, len(placements(tid)), placements(tid)))
    for c in range(n):
        if cur[c] != cen_cur[c][0] or q[c] != cen_q[c]:
            fail("natural", "the replay ends at cpu %d current=%s queue=%s; the CENSUS has current=%s "
                 "queue=%s (a lost, phantom or substituted wake)" % (c, cur[c], sorted(q[c]),
                                                                     cen_cur[c][0], sorted(cen_q[c])))
    # remote adds → notification
    pending, remote_adds, notifies = {}, 0, 0
    for r in body:
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
                fail("natural", "remote ADD seq %d (cpu %d → cpu %d) was never notified" % (seq, e, t))

    def installation(seq, cpu, gen, tid, what):
        s = sec_by_seq.get(seq)
        if s is None or s["cpu"] != cpu or s["gen"] != gen or s["after"] != tid or s["install"] is None:
            return "%s names installation seq %d (cpu %d gen %d tid %d), which is not one" % (
                what, seq, cpu, gen, tid)
        return None

    # ── RUN: observed executions of installed tasks ──────────────────────────────────────────
    runs, run_of_install = [], {}
    for r in body:
        if r["kind"] != "RUN":
            continue
        tid, inc, igen, tgen, iseq = r["f"][:5]
        bad = installation(iseq, r["cpu"], igen, tid, "RUN seq %d" % r["seq"])
        if bad:  # repair:run
            fail("run", bad)
        later = iseq < r["seq"] and tgen >= igen and r["exec"] == r["cpu"] and inc != INC_UNBOUND and r["f"][5] in (0, 1)
        if not later:  # repair:run
            fail("run", "RUN seq %d is not a later tick on cpu %d of installation seq %d" % (r["seq"], r["cpu"], iseq))
        if iseq in run_of_install:  # repair:run
            fail("run", "installation seq %d observed running twice (RUN seq %d)" % (iseq, r["seq"]))
        run_of_install[iseq] = r
        runs.append(r)
    # ── claims: bound to the commit and to the incarnation ───────────────────────────────────
    claimed, claims, verified = set(), [], 0
    for r in body:
        if r["kind"] != "CLAIM":
            continue
        t, e = r["cpu"], r["exec"]
        tid, inc, _rnd, cgen, cseq = r["f"][:5]
        if inc == INC_UNBOUND:  # repair:claim-incarnation
            fail("claim-incarnation", "CLAIM seq %d (cpu %d → cpu %d tid %d) carries no incarnation staged by "
                 "its owner" % (r["seq"], e, t, tid))
        s = sec_by_seq.get(cseq)
        bound = s is not None and cseq < r["seq"] and s["cpu"] == t and s["exec"] == e and s["gen"] == cgen and tid in s["add"]
        if not bound:  # repair:claim-commit
            fail("claim-commit", "CLAIM seq %d (cpu %d → cpu %d tid %d) names commit seq %d gen %d, which did "
                 "not add tid %d to cpu %d on cpu %d (dropped publication, or substituted destination, "
                 "task or commit)" % (r["seq"], e, t, tid, cseq, cgen, tid, t, e))
        if (cseq, tid) in claimed:  # repair:claim-commit
            fail("claim-commit", "CLAIM seq %d claims commit seq %d for tid %d a second time" % (r["seq"], cseq, tid))
        claimed.add((cseq, tid))
        claims.append((r, s))
    claims = [(r, s) for r, s in claims if s is not None]
    for idx, (r, s) in enumerate(claims):
        t, tid, inc = r["cpu"], r["f"][0], r["f"][1]
        nxt = [c for c, _ in claims[idx + 1:] if c["cpu"] == t and c["f"][0] == tid]
        horizon = nxt[0]["f"][4] if nxt else float("inf")
        seen = [x for x in runs if x["cpu"] == t and x["f"][0] == tid and s["seq"] < x["f"][4] < horizon]
        if seen and inc != INC_NO_ASID:
            if seen[0]["f"][1] != inc:  # repair:claim-incarnation
                fail("claim-incarnation", "CLAIM seq %d placed tid %d with incarnation code %d; when it next ran "
                     "(RUN seq %d) cpu %d had code %d active (stale or substituted incarnation)"
                     % (r["seq"], tid, inc, seen[0]["seq"], t, seen[0]["f"][1]))
            verified += 1
    # ── SYNCHRONIZED: attempts — validity first, then coverage ───────────────────────────────
    attempts, open_hold, late = [], None, 0
    for r in body:
        if r["kind"] == "HOLD_BEGIN":
            if open_hold is not None:
                fail("attempt", "nested hold at seq %d" % r["seq"])
            open_hold = {"begin": r, "progress": [], "related": []}
        elif r["kind"] in ("PROGRESS", "RELATED"):
            if open_hold is not None and r["f"][0] == open_hold["begin"]["f"][0]:
                open_hold["progress" if r["kind"] == "PROGRESS" else "related"].append(r)
            elif attempts and r["f"][0] == attempts[-1]["begin"]["f"][0] and (
                    open_hold is None or open_hold["begin"]["f"][0] != r["f"][0]):
                # Observed during the hold that has just ended (the holder ended it by its bound
                # before this record's slot was reserved). Validated, but credits nothing.
                attempts[-1]["late"].append(r)
                late += 1
            else:
                fail("attempt", "%s seq %d outside its hold" % (r["kind"], r["seq"]))
        elif r["kind"] == "HOLD_END":
            if open_hold is None or r["f"][0] != open_hold["begin"]["f"][0] or r["cpu"] != open_hold["begin"]["cpu"]:
                fail("attempt", "unpaired HOLD_END seq %d" % r["seq"])
            open_hold["end"] = r
            open_hold["late"] = []
            attempts.append(open_hold)
            open_hold = None
    if open_hold is not None:
        fail("attempt", "a hold never ended")

    def check_progress(p, a, obs, b, e, rnd):
        tid, tgen, igen, inc, iseq = p["f"][1], p["f"][2], p["f"][3], p["f"][4], p["f"][5]
        if p["cpu"] != obs or p["exec"] != obs:  # repair:progress
            fail("progress", "round %d: PROGRESS seq %d on cpu %d/exec %d, the observer is %d"
                 % (rnd, p["seq"], p["cpu"], p["exec"], obs))
        bad = installation(iseq, obs, igen, tid, "round %d: PROGRESS seq %d" % (rnd, p["seq"]))
        if bad:  # repair:progress
            fail("progress", bad + " (progress linked to the wrong installation)")
        s = sec_by_seq.get(iseq)
        own = s is not None and s["exec"] == obs and b["seq"] < iseq < p["seq"] and tgen > igen and inc != INC_UNBOUND
        if not own:  # repair:progress
            fail("progress", "round %d: PROGRESS seq %d's installation seq %d is not the observer's own "
                 "commit inside the hold, before a later tick" % (rnd, p["seq"], iseq))
        run = run_of_install.get(iseq)
        if run is not None and run["f"][1] != inc:  # repair:progress
            fail("progress", "round %d: PROGRESS seq %d reports incarnation code %d; RUN seq %d of the same "
                 "installation saw %d" % (rnd, p["seq"], inc, run["seq"], run["f"][1]))
        return s

    expect_round, expect_try, ok_attempts = 0, 0, 0
    credited = {}
    for at in attempts:
        b, e = at["begin"], at["end"]
        rnd, obs_f, inst, bgen, held, f5 = b["f"]
        a, obs = pair(rnd, n)
        # (1) the schedule and the attempt order
        if rnd >= rounds or b["cpu"] != a or obs_f != obs or b["exec"] != a or e["exec"] != a:
            fail("attempt", "seq %d: round %d held by cpu %d for %d, schedule says %d for %d"
                 % (b["seq"], rnd, b["cpu"], obs_f, a, obs))
        if (rnd, f5 >> 8) != (expect_round, expect_try):  # repair:attempt-order
            fail("attempt", "seq %d: attempt %d of round %d out of order (expected attempt %d of round %d)"
                 % (b["seq"], f5 >> 8, rnd, expect_try, expect_round))
        # (2) the holder's lock, its mask state and its lock count
        if inst != cell[a]:  # repair:lock-instance
            fail("lock-instance", "round %d attempt seq %d holds instance %#x; cpu %d's cell is %#x"
                 % (rnd, b["seq"], inst, a, cell[a]))
        if (f5 & 1) != 1 or (e["f"][5] & 1) != 1:  # repair:attempt-ownership
            fail("attempt", "round %d attempt seq %d: the holder's lock was not held at begin and end" % (rnd, b["seq"]))
        if (f5 >> 1) & 1 or (e["f"][5] >> 2) & 1:  # repair:attempt-ownership
            fail("attempt", "round %d attempt seq %d: interrupts were enabled during the hold" % (rnd, b["seq"]))
        held_1l, held_1g = held & 0xFFFFFFFF, held >> 32
        if held_1l != 1 or held_1g != 0:  # repair:attempt-ownership
            fail("attempt", "round %d attempt seq %d: the holder held 1L=%d 1G=%d (exactly its own 1L required)"
                 % (rnd, b["seq"], held_1l, held_1g))
        if (e["f"][5] >> 8) != bgen:  # repair:attempt-ownership
            fail("attempt", "round %d attempt seq %d: the holder's lock was re-acquired during the hold" % (rnd, b["seq"]))
        # (3) the holder's own sections are ordered around the hold
        for s in secs:
            if s["cpu"] != a:
                continue
            if b["seq"] < s["seq"] < e["seq"]:
                fail("attempt", "round %d — a section on the HOLDER's cpu %d inside its hold (its lock did not "
                     "exclude)" % (rnd, a))
            if (s["seq"] < b["seq"] and s["gen"] >= bgen) or (s["seq"] > e["seq"] and s["gen"] < bgen):  # repair:attempt-order
                fail("attempt", "round %d attempt seq %d: holder generation %d disagrees with section seq %d gen %d"
                     % (rnd, b["seq"], bgen, s["seq"], s["gen"]))
        # (4) the reported outcome against its own counters and the fixed bounds
        commits_r, prog_r, rel_r, spins, ok = e["f"][1], e["f"][2], e["f"][3], e["f"][4], (e["f"][5] >> 1) & 1
        consistent_ok = commits_r >= COMMITS_REQUIRED and prog_r >= 1 and rel_r == 0 and spins < HOLD_SPINS
        if ok and not consistent_ok:  # repair:attempt-outcome
            fail("attempt", "round %d attempt seq %d reports success with commits=%d progress=%d related=%d spins=%d"
                 % (rnd, b["seq"], commits_r, prog_r, rel_r, spins))
        if not ok and not (rel_r > 0 or spins >= HOLD_SPINS):  # repair:attempt-outcome
            fail("attempt", "round %d attempt seq %d reports failure with neither interference nor its bound "
                 "(related=%d spins=%d)" % (rnd, b["seq"], rel_r, spins))
        if at["related"] and ok:  # repair:attempt-outcome
            fail("attempt", "round %d attempt seq %d reports success despite RELATED records" % (rnd, b["seq"]))
        for rr in at["related"] + [x for x in at["late"] if x["kind"] == "RELATED"]:
            if rr["cpu"] != a or rr["exec"] != obs:
                fail("attempt", "RELATED seq %d is not the observer touching the holder's lock" % rr["seq"])
        # (5) every progress record, counted or late, is a genuine linked execution
        good_prog = [check_progress(p, a, obs, b, e, rnd) for p in at["progress"]]
        for p in at["late"]:
            if p["kind"] == "PROGRESS":
                check_progress(p, a, obs, b, e, rnd)
        # the producer's own round progression
        if ok:
            ok_attempts += 1
            expect_round, expect_try = rnd + 1, 0
        elif expect_try + 1 >= ATTEMPTS:
            expect_round, expect_try = rnd + 1, 0
        else:
            expect_try += 1
        # ── coverage: credit a valid attempt ─────────────────────────────────────────────────
        commits = [s for s in secs if b["seq"] < s["seq"] < e["seq"] and s["cpu"] == obs and s["exec"] == obs]
        installs = {s["seq"] for s in commits if s["install"] is not None}
        ran = [s for s in good_prog if s["seq"] in installs]
        enough = len({s["gen"] for s in commits}) >= COMMITS_REQUIRED  # repair:commit-count
        if ok and not at["related"] and enough and ran:
            credited.setdefault(rnd, (a, obs, len(commits), len(ran)))
    if ok_attempts != end["ok"]:  # repair:attempt-outcome
        fail("attempt", "%d attempts report success; the producer counted %d" % (ok_attempts, end["ok"]))
    missing = [r for r in range(rounds) if r not in credited]
    if missing:
        fail("coverage", "rounds %s were never credited (schedule %s)" % (missing, [pair(r, n) for r in missing]))
    dirs = sorted({(a, b) for a, b, _, _ in credited.values()})
    uncredited = len(attempts) - len(credited)
    return ("SL1W_GRADE result=pass ncpu=%d | SYNCHRONIZED holds: %d/%d rounds credited over %d valid attempts "
            "(%d uncredited), directions %s, %d late observer records not counted | NATURAL trace: %d sections "
            "(%d remote adds, %d notifications, %d claims bound to their commits, %d incarnations confirmed by "
            "a later run, %d runs, %d restores) replayed SNAP→CENSUS exactly; cells %s"
            % (n, len(credited), rounds, len(attempts), uncredited, dirs, late, len(secs), remote_adds,
               notifies, len(claims), verified, len(runs), restores,
               " ".join("%d:%#x" % (c, cell[c]) for c in range(n))))


# ── self-test (the full corpus is tests/fixtures/sched_lock1/mkcases.py) ───────────────────────
def line(seq, kind, cpu, ex, f):
    body = "SL1W_R seq=%d kind=%s cpu=%d exec=%d f0=%d f1=%d f2=%d f3=%d f4=%d f5=%d" % (
        seq, kind, cpu, ex, *f)
    return "%s crc=%08x" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)


def build(events, ncpu=2, phase=2, overflow=0, ok=2):
    """`events`: (kind, cpu, exec, fields[, label]); a field may be ("@", label) for the record
    number of the labelled event."""
    seq_of = {ev[4]: i + 1 for i, ev in enumerate(events) if len(ev) > 4}
    out = []
    for i, ev in enumerate(events):
        f = [seq_of[x[1]] if isinstance(x, tuple) else x for x in ev[3]]
        out.append(line(i + 1, ev[0], ev[1], ev[2], f + [0] * (6 - len(f))))
    endb = "SL1W_END count=%d overflow=%d phase=%d ncpu=%d rounds=%d rounds_ok=%d" % (
        len(events), overflow, phase, ncpu, 2 if ncpu == 2 else 2 * ncpu, ok)
    out.append("%s crc=%08x" % (endb, zlib.crc32(endb.encode()) & 0xFFFFFFFF))
    return "\n".join(o + "\n" + o for o in out)


C0, C1, LOCK_OFF = 0x80001000, 0x80002000, 0x40
A2 = lambda asid: asid + 2  # noqa: E731 — incarnation code for an address space


def good():
    """Two CPUs. Round 0: cpu 0 holds; cpu 1 requeues 100 and selects 300, which runs. Then cpu 0
    wakes 200 onto cpu 1 (claimed with its incarnation). Round 1: cpu 1 holds; cpu 0 selects 500,
    which runs. Finally cpu 1 selects 200, which runs in the claimed address space."""
    ev = [("SNAP_BEGIN", 0, 0, [2, 3, 3, 10, 0, 0]),
          ("CELL", 0, 0, [C0, C0 + LOCK_OFF, 0]), ("CELL", 1, 1, [C1, C1 + LOCK_OFF, 0]),
          ("SNAP_CUR", 0, 0, [0, 5]), ("SNAP_CUR", 1, 1, [101, 7]), ("SNAP_Q", 1, 1, [300, 1])]
    ev += [("HOLD_BEGIN", 0, 0, [0, 1, C0, 6, 1, 1]),
           ("SEC", 1, 1, [8, 101, 0, 1, 0, C1]), ("ADD", 1, 1, [8, 100, 1]),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, C1], "i300"), ("REM", 1, 1, [9, 300]),
           ("RUN", 1, 1, [300, A2(3), 9, 10, ("@", "i300")]),
           ("PROGRESS", 1, 1, [0, 300, 10, 9, A2(3), ("@", "i300")]),
           ("HOLD_END", 0, 0, [0, 2, 1, 0, 50, 1 | 2 | (6 << 8)])]
    ev += [("SEC", 1, 0, [11, 301, 301, 1, 0, C1], "c200"), ("ADD", 1, 0, [11, 200, 1]),
           ("CLAIM", 1, 0, [200, A2(9), 1, 11, ("@", "c200")]), ("NOTIFY", 1, 0, [11])]
    ev += [("SEC", 0, 0, [7, 0, 0, 1, 0, C0]), ("ADD", 0, 0, [7, 500, 1]),
           ("HOLD_BEGIN", 1, 1, [1, 0, C1, 12, 1, 1]),
           ("SEC", 0, 0, [8, 0, 501, 0, 1, C0], "i500"), ("REM", 0, 0, [8, 500]),
           ("SEC", 0, 0, [9, 501, 501, 1, 0, C0]), ("ADD", 0, 0, [9, 600, 1]),
           ("RUN", 0, 0, [500, A2(5), 8, 10, ("@", "i500")]),
           ("PROGRESS", 0, 0, [1, 500, 10, 8, A2(5), ("@", "i500")]),
           ("HOLD_END", 1, 1, [1, 2, 1, 0, 70, 1 | 2 | (12 << 8)])]
    ev += [("SEC", 1, 1, [13, 301, 201, 1, 1, C1], "i200"), ("ADD", 1, 1, [13, 300, 1]),
           ("REM", 1, 1, [13, 200]),
           ("RUN", 1, 1, [200, A2(9), 13, 14, ("@", "i200")])]
    ev += [("CENSUS_BEGIN", 0, 0, [2, 3, 3, 20, 2, 2]),
           ("CELL", 0, 0, [C0, C0 + LOCK_OFF, 1]), ("CELL", 1, 1, [C1, C1 + LOCK_OFF, 1]),
           ("CENSUS_CUR", 0, 0, [501, 10]), ("CENSUS_Q", 0, 0, [600, 1]),
           ("CENSUS_CUR", 1, 1, [201, 14]), ("CENSUS_Q", 1, 1, [100, 1]), ("CENSUS_Q", 1, 1, [300, 1]),
           ("CENSUS_END", 0, 0, [2, 2])]
    return ev


def self_test():
    g = good()

    def sub(kind, k, idx, val):
        ev = [list(e) for e in g]
        i = [i for i, e in enumerate(ev) if e[0] == kind][k]
        ev[i][3] = list(ev[i][3])
        ev[i][3][idx] = val
        return [tuple(e) for e in ev]

    cases = [("valid trace", build(g), None),
             ("section names a third lock instance", build(sub("SEC", 0, 5, 0x7E570000)), "lock-instance:"),
             ("claim incarnation substituted", build(sub("CLAIM", 0, 1, A2(77))), "claim-incarnation:"),
             ("claim names another commit", build(sub("CLAIM", 0, 3, 9)), "claim-commit:"),
             ("holder without its lock", build(sub("HOLD_BEGIN", 0, 5, 0)), "attempt:"),
             ("overflow", build(g, overflow=1), "validity:"),
             ("window not closed", build(g, phase=1), "validity:")]
    bad = 0
    for name, text, want in cases:
        try:
            out, got = grade(text), None
        except Fail as e:
            out, got = str(e), str(e)
        verdict = (got is None) if want is None else (got is not None and got.startswith(want))
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
