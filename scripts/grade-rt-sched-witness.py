#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""RT-SCHED1A/RT-SCHED1B — grade the live RT selection witness (`kernel::rt_witness`).

Input: the normalized serial log of one boot with `<port>-rt-sched-witness` and
`yarm.sched_profile=rt`. The dumper printed every record twice (`RTW_REC pass=0|1 ...`), each line
with an FNV-1a checksum over its body; a line whose checksum fails counts for nothing, and the two
copies of a record must agree. Records are ordered by their causal stamp (`seq`), never by console
position. Identities come from the `RTW_ROLE` lines (each role's exact kernel-thread id or user
TID), and every rule below compares exact identities.

Rules (each one a named check; any failure fails the grade):

  ID     every role present, with its planned home and level; UA = 9700, UB = 9701; every kernel
         thread's START on its home; UA placed on CPU 0 and UB on CPU 1.
  SEAL   both seals verify, no overflow, the run ended in DUMPING, no TIMEOUT record.
  PHASES the phase sequence is exactly SETUP WARMUP (A_ARM A_WAIT)x3 B_ARM B_WAIT C_ARM C_WAIT
         C_YIELD D_ARM D_L D_WAIT DONE DUMPING.
  A      (local wake, x3) each round's wake of H0 is issued on CPU 0 while UA is current; the first
         CPU 0 preemption after it is UA's, cause 1 (the reschedule arrival, not a tick); H0 runs
         after it, on CPU 0; UA's yield storm is running (UA has yielded by round 1) and its count
         never falls. (A strict rise per round is not required: under RT the level-0 supervisor's
         one-tick receive poll may take nearly all of CPU 0, and correctly starves level 20.)
  B      (remote wake) H1's wake is issued on CPU 0 while UB is CPU 1's current; the first CPU 1
         preemption after it is UB's — at the reschedule IPI's arrival (cause 1) or at a CPU 1
         preempting tick that reached the strictly higher H1 first (cause 2; on AArch64 the two
         CPUs' ticks can be nearly phase-aligned with the driver's) — and H1 runs after it, on
         CPU 1.
  IPI    the real reschedule IPI: of the remote wakes issued over a running UB (B's, C's second
         H1 wake and, with budgets, E's placement of UH), at least one is settled at the
         reschedule arrival (cause 1).
  C      (equal priority, and the head position) E's wake (level 20, UB's level) is followed by
         a DECLINED arrival on CPU 1 for UB at 20 vs 20, cause 1, and at least 2 CPU 1 ticks on UB
         (no rotation). Then H1's second wake, issued over UB: the first CPU 1 preemption after it
         is UB's — at the reschedule arrival (cause 1) or a preempting tick that reached the
         strictly higher H1 first (cause 2); either is a legal scheduling point — and H1 runs; it is the ONLY CPU 1 preemption from E's wake until E
         runs. After H1 runs, at least 4 more CPU 1 ticks land on UB before the yield request: UB,
         not E, got the CPU back (UB returned to the HEAD of level 20, ahead of E). UB had not
         yielded at the request; E does not run before it; E runs on CPU 1 with UB's yield count
         exactly one higher (UB's one explicit yield put it at the tail, behind E — FIFO).
  D      (kernel thread) L takes CPU 0 from UA (cause 1) and starts; H0's wake is issued while L
         is the current kernel thread; L's next scheduling point reports rt=true (yield to the
         head); H0 runs; L then resumes and ends; no scheduling point ever reports GP.

RT-SCHED1B (`--budget on`: the boot also carries `yarm.sched_budget=on`):

  CFG    the boot's `YARM_SCHED_BUDGET` line says result=enforced on the global scheduler tick
         (budget off: result=off, and no budget phase ran). The phase sequence continues
         D_WAIT E_ARM E_PLACE E_RUN F_ARM F_RUN DONE.
  E      (budget exhaustion, a user task) UH (9702, CPU 1, level 8, 2/8) is placed FROM CPU 0 over
         a running UB and its remote wake preempts UB (cause 1); its context is bound with 2/8.
         Then at least 3 cycles, each: UH is charged to exhaustion (remaining 0); the next CPU 1
         preemption takes the CPU from UH and UH is parked (one reservation, level 8) with no
         queue position; while it is parked UH's heartbeat does not move and UB's (level 20, the
         LOWER priority) does; UH rejoins only at a release instant bound + k*8 that is later
         than its park, at most LATE_MAX ticks late, with budget; the next CPU 1 preemption after
         the rejoin is UB's (UH takes the CPU back) and UH's heartbeat moves before its next
         exhaustion.
  F      (budget exhaustion, a cooperative kernel thread) K (CPU 0, level 6, 2/10) starts with its
         context bound 2/10; at least 3 cycles, each: K is charged to exhaustion; its next
         scheduling point reports rt=true (the safe point honours the exhaustion) and K is parked
         there (kernel-thread headroom, level 6); its progress does not move while parked; it
         rejoins at a release instant bound + k*10 after its park, at most LATE_MAX late; it
         resumes from that same scheduling point (its progress equals the parked value); K ends.
  LEDGER each CPU's ledger: no unaccounted time (every thread placed on a budgeted CPU had a
         context), at most 32 contexts, essential tick work recorded; UH's and K's contexts show
         at least the graded exhaustions and releases.
"""
import re
import sys

PHASE = {n: i for i, n in enumerate([
    "OFF", "WAIT_IDLE", "SETUP", "WARMUP", "A_ARM", "A_WAIT", "B_ARM", "B_WAIT", "C_ARM",
    "C_WAIT", "C_H1", "C_AFTER", "C_YIELD", "D_ARM", "D_L", "D_WAIT", "E_ARM", "E_PLACE", "E_RUN",
    "F_ARM", "F_RUN", "DONE", "DUMPING"])}
ROLES = ["H0", "H1", "E", "L", "DUMP", "S", "UA", "UB"]
BUDGET_ROLES = ["K", "UH"]
R = {n: i for i, n in enumerate(ROLES + BUDGET_ROLES)}
PLAN = {"H0": (0, 4), "H1": (1, 4), "E": (1, 20), "L": (0, 16), "DUMP": (0, 3), "S": (0, 1),
        "UA": (0, 20), "UB": (1, 20), "K": (0, 6), "UH": (1, 8)}
BUDGET_PHASES = ["E_ARM", "E_PLACE", "E_RUN", "F_ARM", "F_RUN"]


def expect_phases(budget):
    return ["SETUP", "WARMUP"] + ["A_ARM", "A_WAIT"] * 3 + [
        "B_ARM", "B_WAIT", "C_ARM", "C_WAIT", "C_H1", "C_AFTER", "C_YIELD", "D_ARM", "D_L",
        "D_WAIT"] + (BUDGET_PHASES if budget else []) + ["DONE", "DUMPING"]


UH_BUDGET = (2, 8)
K_BUDGET = (2, 10)
E_CYCLES = 3
F_CYCLES = 3
# A release is applied at the enforcing CPU's next scheduler section after the clock owner
# reaches it: on CPU 1 its next tick (the two CPUs' ticks are not phase-locked), on CPU 0 the
# clock owner's own tick. Measured in clock-owner ticks.
LATE_MAX = 2
BUDGET_LINE = re.compile(r"YARM_SCHED_BUDGET req=(\S+) profile=(\S+) result=(\S+) route=(\S+) clock=(\S+)")
A_ROUNDS = 3
C_TICKS_BEFORE = 2
C_TICKS_MIN = 4


def fnv(b):
    h = 0x811C9DC5
    for c in b:
        h = ((h ^ c) * 0x01000193) & 0xFFFFFFFF
    return h


LINE = re.compile(r"(RTW_(?:REC|ROLE|SEAL) [^\r\n]*?) crc=0x([0-9a-f]{8})")


def parse(text):
    """-> (records by seq, roles, seals, damaged count)."""
    recs, roles, seals, damaged = {}, {}, [], 0
    conflicts = []
    for m in LINE.finditer(text):
        body, crc = m.group(1), int(m.group(2), 16)
        if fnv(body.encode()) != crc:
            damaged += 1
            continue
        f = dict(kv.split("=", 1) for kv in body.split()[1:] if "=" in kv)
        if body.startswith("RTW_REC"):
            seq = int(f["seq"])
            rec = (f["kind"], int(f["cpu"]), tuple(int(f["v%d" % i], 16) for i in range(5)))
            if seq in recs and recs[seq] != rec:
                conflicts.append(seq)
            recs[seq] = rec
        elif body.startswith("RTW_ROLE"):
            roles[f["role"]] = (int(f["tid"], 16), int(f["home"]), int(f["level"]))
        else:
            seals.append(f)
    return recs, roles, seals, damaged, conflicts


class Grade:
    def __init__(self):
        self.fails = []
        self.notes = []

    def check(self, name, ok, why):
        if not ok:
            self.fails.append("%s: %s" % (name, why))
        return ok


def grade(text, budget=False):
    g = Grade()
    recs, roles, seals, damaged, conflicts = parse(text)
    g.notes.append("records=%d damaged_lines=%d" % (len(recs), damaged))
    g.check("SEAL", not conflicts, "copies disagree at seq %s" % conflicts[:5])
    g.check("SEAL", len(seals) >= 1, "no verified seal")
    for s in seals:
        g.check("SEAL", s.get("overflow") == "0", "overflow=%s" % s.get("overflow"))
        g.check("SEAL", s.get("final_phase") == "DUMPING", "final_phase=%s" % s.get("final_phase"))
        g.check("SEAL", int(s.get("records", -1)) == len(recs) or int(s.get("taken", -1)) >= len(recs),
                "records=%s vs %d" % (s.get("records"), len(recs)))
    seqs = sorted(recs)
    if seals:
        taken = int(seals[0].get("taken", 0))
        missing = [s for s in range(min(taken, 2048)) if s not in recs]
        g.check("SEAL", not missing, "records missing (both copies damaged?) %s" % missing[:8])
    ev = [(s,) + recs[s] for s in seqs]

    # ── CFG ──
    cfg = BUDGET_LINE.findall(text)
    if budget:
        g.check("CFG", bool(cfg) and cfg[-1][2] == "enforced" and cfg[-1][1] == "rt"
                and cfg[-1][4] == "global_scheduler_tick", "budget configuration line %s" % cfg)
    else:
        g.check("CFG", not cfg or cfg[-1][2] != "enforced", "budgets enforced in a budget-off run %s" % cfg)
        g.check("CFG", not [e for e in ev if e[1].startswith("BUDGET_") or e[1] in ("KSTART", "LEDGER")],
                "budget records in a budget-off run")
    if cfg:
        g.notes.append("budget configuration req=%s profile=%s result=%s route=%s clock=%s" % cfg[-1])

    # ── ID ──
    all_roles = ROLES + (BUDGET_ROLES if budget else [])
    for role in all_roles:
        if not g.check("ID", role in roles, "role %s missing" % role):
            return g
        tid, home, level = roles[role]
        g.check("ID", (home, level) == PLAN[role],
                "%s home/level %s != %s" % (role, (home, level), PLAN[role]))
        g.check("ID", tid != 0, "%s has no identity" % role)
    tid = {r: roles[r][0] for r in all_roles}
    g.check("ID", tid["UA"] == 9700 and tid["UB"] == 9701, "user TIDs %s %s" % (tid["UA"], tid["UB"]))
    if budget:
        g.check("ID", tid["UH"] == 9702, "UH TID %s" % tid["UH"])
    g.check("ID", len(set(tid.values())) == len(all_roles), "identities not distinct")
    for role in ["H0", "H1", "E", "L", "DUMP", "S"] + (["K"] if budget else []):
        st = [e for e in ev if e[1] == "START" and e[3][0] == R[role]]
        g.check("ID", len(st) == 1 and st[0][3][1] == tid[role] and st[0][2] == PLAN[role][0],
                "%s START %s" % (role, st))
    for role, cpu in (("UA", 0), ("UB", 1)) + ((("UH", 1),) if budget else ()):
        u = [e for e in ev if e[1] == "USER" and e[3][0] == R[role]]
        g.check("ID", len(u) == 1 and u[0][3][1] == tid[role] and u[0][3][3] == cpu,
                "%s placement %s" % (role, u))
    g.check("SEAL", not [e for e in ev if e[1] == "TIMEOUT"],
            "TIMEOUT %s" % [e for e in ev if e[1] == "TIMEOUT"])

    # ── PHASES ──
    phases = [e[3][0] for e in ev if e[1] == "PHASE"]
    names = [k for v in phases for k, i in PHASE.items() if i == v]
    g.check("PHASES", names == expect_phases(budget), "sequence %s" % names)

    def after(s, pred):
        return next((e for e in ev if e[0] > s and pred(e)), None)

    def between(a, b, pred):
        return [e for e in ev if a < e[0] < b and pred(e)]

    def wake(phase, role, extra=lambda e: True):
        return [e for e in ev if e[1] == "WAKE" and e[3][0] == PHASE[phase] and e[3][1] == R[role]
                and extra(e)]

    def run(role, phase, cpu):
        return lambda e: (e[1] == "RUN" and e[3][0] == R[role] and e[3][1] == tid[role]
                          and e[3][2] == PHASE[phase] and e[2] == cpu)

    preempt = lambda cpu: (lambda e: e[1] == "PREEMPT" and e[2] == cpu)

    # ── A ──
    prev_yields = None
    a_runs = 0
    a_yields = []
    for r in range(1, A_ROUNDS + 1):
        w = wake("A_WAIT", "H0", lambda e: e[3][3] == r)
        if not g.check("A", len(w) == 1, "round %d wake %s" % (r, w)):
            continue
        w = w[0]
        g.check("A", w[2] == 0 and w[3][2] == tid["UA"], "round %d wake not on CPU 0 over UA: %s" % (r, w))
        p = after(w[0], preempt(0))
        ok = g.check("A", p is not None and p[3][0] == tid["UA"] and p[3][1] == 1,
                     "round %d first CPU 0 preemption after the wake %s" % (r, p))
        h = after(w[0], run("H0", "A_WAIT", 0))
        if g.check("A", h is not None and ok and h[0] > p[0], "round %d H0 run %s" % (r, h)):
            a_runs += 1
            y = h[3][4] >> 32
            if prev_yields is None:
                g.check("A", y >= 1, "UA's yield storm had not run by round 1 (yields=%d)" % y)
            else:
                g.check("A", y >= prev_yields, "UA yields fell between rounds (%s -> %s)" % (prev_yields, y))
            a_yields.append(y)
            prev_yields = y
    g.notes.append("A local-wake preemptions graded: %d/%d; UA yields at H0's runs %s" % (a_runs, A_ROUNDS, a_yields))

    # ── B ──
    remote_causes = []
    w = wake("B_WAIT", "H1")
    if g.check("B", len(w) == 1, "wake %s" % w):
        w = w[0]
        g.check("B", w[2] == 0 and w[3][2] == tid["UB"], "wake not from CPU 0 over UB %s" % (w,))
        p = after(w[0], preempt(1))
        ok = g.check("B", p is not None and p[3][0] == tid["UB"] and p[3][1] in (1, 2),
                     "first CPU 1 preemption after the remote wake %s" % (p,))
        if p is not None:
            remote_causes.append(("B", p[3][1]))
        h = after(w[0], run("H1", "B_WAIT", 1))
        g.check("B", h is not None and ok and h[0] > p[0], "H1 run %s" % (h,))

    # ── C ──
    w = wake("C_WAIT", "E")
    w_h = wake("C_H1", "H1")
    if g.check("C", len(w) == 1 and len(w_h) == 1, "wakes %s %s" % (w, w_h)):
        w, w_h = w[0], w_h[0]
        y0 = w[3][4]
        g.check("C", w[3][2] == tid["UB"] and w_h[3][2] == tid["UB"], "wakes not over UB %s %s" % (w, w_h))
        e_run = after(w[0], lambda e: e[1] == "RUN" and e[3][0] == R["E"])
        yreq = after(w[0], lambda e: e[1] == "YREQ")
        end = e_run[0] if e_run else 1 << 62
        d = between(w[0], w_h[0], lambda e: e[1] == "DECLINED" and e[2] == 1)
        g.check("C", any(x[3][0] == tid["UB"] and x[3][1] == 20 and x[3][2] == 20 and x[3][3] == 1 for x in d),
                "no DECLINED arrival for UB at 20/20 on CPU 1 before H1's wake: %s" % d)
        t1 = between(w[0], w_h[0], lambda e: e[1] == "TICK" and e[2] == 1 and e[3][0] == tid["UB"])
        g.check("C", len(t1) >= C_TICKS_BEFORE, "only %d CPU 1 ticks on UB before H1's wake" % len(t1))
        p = after(w_h[0], preempt(1))
        ok = g.check("C", p is not None and p[3][0] == tid["UB"] and p[3][1] in (1, 2),
                     "first CPU 1 preemption after H1's wake %s" % (p,))
        if p is not None:
            g.notes.append("C head-position preemption cause=%d" % p[3][1])
            remote_causes.append(("C", p[3][1]))
        pre = between(w[0], end, preempt(1))
        g.check("C", ok and [x[0] for x in pre] == [p[0]],
                "CPU 1 preemptions between E's wake and E's run: %s" % pre)
        h = after(w_h[0], run("H1", "C_H1", 1))
        g.check("C", h is not None and ok and h[0] > p[0], "H1 run %s" % (h,))
        if g.check("C", yreq is not None and h is not None, "no yield request"):
            ticks = between(h[0], yreq[0], lambda e: e[1] == "TICK" and e[2] == 1 and e[3][0] == tid["UB"])
            g.check("C", len(ticks) >= C_TICKS_MIN,
                    "only %d CPU 1 ticks on UB after H1 ran (UB not back at its level's head?)" % len(ticks))
            g.check("C", yreq[3][0] == tid["UB"] and yreq[3][1] == y0,
                    "UB yielded before the request (%s vs %s)" % (yreq[3][1], y0))
            g.check("C", e_run is not None and e_run[0] > yreq[0], "E ran before UB's yield: %s" % (e_run,))
        if g.check("C", e_run is not None, "E never ran"):
            g.check("C", e_run[2] == 1 and e_run[3][1] == tid["E"], "E run identity %s" % (e_run,))
            g.check("C", (e_run[3][4] & 0xFFFFFFFF) == y0 + 1,
                    "UB yields at E's run %d, expected exactly %d" % (e_run[3][4] & 0xFFFFFFFF, y0 + 1))

    # ── D ──
    w1 = wake("D_L", "L")
    w2 = wake("D_WAIT", "H0")
    if g.check("D", len(w1) == 1 and len(w2) == 1, "wakes %s %s" % (w1, w2)):
        w1, w2 = w1[0], w2[0]
        g.check("D", w1[3][2] == tid["UA"], "L's wake not over UA %s" % (w1,))
        p = after(w1[0], preempt(0))
        g.check("D", p is not None and p[3][0] == tid["UA"] and p[3][1] == 1 and p[0] < w2[0],
                "UA not preempted by L's wake %s" % (p,))
        ls = after(w1[0], lambda e: e[1] == "LSTART")
        g.check("D", ls is not None and ls[3][0] == tid["L"] and ls[0] < w2[0], "L start %s" % (ls,))
        g.check("D", w2[3][2] == tid["L"], "H0's wake not issued over L %s" % (w2,))
        sp = after(w2[0], lambda e: e[1] == "SCHEDPT" and e[3][0] == tid["L"])
        h = after(w2[0], run("H0", "D_WAIT", 0))
        # L's resume AFTER H0 ran. An earlier resume (from a yield to some other higher entry
        # before H0's wake) can be recorded after the wake: L records its resume only once its
        # scheduling point returns, and the driver's tick may land on L in between.
        lr = after(h[0], lambda e: e[1] == "LRESUMED" and e[3][0] == tid["L"]) if h else None
        le = after(w2[0], lambda e: e[1] == "LEND" and e[3][0] == tid["L"])
        ok = g.check("D", sp is not None and sp[3][1] == 2, "L's scheduling point %s" % (sp,))
        ok = g.check("D", h is not None and ok and h[0] > sp[0], "H0 run %s" % (h,)) and ok
        g.check("D", lr is not None and ok, "L did not resume after H0 ran: %s" % (lr,))
        g.check("D", le is not None and lr is not None and le[0] > lr[0], "L end %s" % (le,))
    g.check("D", not [e for e in ev if e[1] == "SCHEDPT" and e[3][1] == 0], "a GP scheduling point under RT")
    if budget:
        e_cause = grade_budget(g, ev, tid, after, between, preempt)
        if e_cause is not None:
            remote_causes.append(("E", e_cause))
    g.notes.append("remote-wake preemption causes %s" % remote_causes)
    g.check("IPI", any(c == 1 for _, c in remote_causes),
            "no remote wake was settled at the reschedule IPI's arrival: %s" % remote_causes)
    return g


def hb_own(e):
    return e[3][4] >> 32


def hb_lower(e):
    return e[3][4] & 0xFFFFFFFF


def rejoined(e):
    return (e[3][3] >> 16) & 1 == 1


def grade_cycles(g, rule, ev, who, bound, period, start, cycles, after, between):
    """The exhaustion -> park -> release -> rejoin cycles of `who` after seq `start`.
    -> list of (exhaust, park, replenish) for the graded cycles."""
    rej = [e for e in ev if e[0] > start and e[1] == "BUDGET_REPLENISH" and e[3][0] == who and rejoined(e)]
    g.check(rule, len(rej) >= cycles, "only %d rejoins after exhaustion (need %d)" % (len(rej), cycles))
    out = []
    prev = start
    for i, r in enumerate(rej[:cycles]):
        parks = between(prev, r[0], lambda e: e[1] == "BUDGET_PARK" and e[3][0] == who)
        if not g.check(rule, len(parks) >= 1, "cycle %d: rejoin %s without a park" % (i + 1, r)):
            prev = r[0]
            continue
        p = parks[-1]
        ex = between(prev, p[0], lambda e: e[1] == "BUDGET_EXHAUST" and e[3][0] == who)
        ok = g.check(rule, len(ex) >= 1 and (ex[-1][3][2] & 0xFFFFFFFF) == 0,
                     "cycle %d: park %s not preceded by an exhaustion to 0 (%s)" % (i + 1, p, ex[-1:]))
        x = ex[-1] if ex else None
        g.check(rule, (p[3][3] & 0xFFFFFFFF) == 0, "cycle %d: parked with budget %s" % (i + 1, p))
        g.check(rule, hb_own(p) == hb_own(r),
                "cycle %d: ran while parked (own progress %d -> %d)" % (i + 1, hb_own(p), hb_own(r)))
        rel, now = r[3][2], r[3][1]
        g.check(rule, rel > bound and (rel - bound) % period == 0,
                "cycle %d: release %d not on the grid bound %d + k*%d" % (i + 1, rel, bound, period))
        g.check(rule, rel > p[3][1], "cycle %d: rejoined at release %d not after the park at %d" % (i + 1, rel, p[3][1]))
        g.check(rule, 0 <= now - rel <= LATE_MAX, "cycle %d: applied %d ticks after its release" % (i + 1, now - rel))
        g.check(rule, (r[3][3] & 0xFFFF) > 0, "cycle %d: rejoined without budget %s" % (i + 1, r))
        if ok:
            out.append((x, p, r))
        prev = r[0]
    return out


def grade_budget(g, ev, tid, after, between, preempt):
    """-> the cause of the preemption E's placement of UH took (None if not graded)."""
    e_cause = None
    # ── E ── the high-priority user task UH on CPU 1.
    uh, ub = tid["UH"], tid["UB"]
    w = [e for e in ev if e[1] == "WAKE" and e[3][0] == PHASE["E_PLACE"] and e[3][1] == R["UH"]]
    u = [e for e in ev if e[1] == "USER" and e[3][0] == R["UH"]]
    if g.check("E", len(w) == 1 and len(u) == 1, "placement %s %s" % (w, u)):
        w, u = w[0], u[0]
        g.check("E", w[3][2] == ub and u[2] == 0 and u[3][3] == 1,
                "UH not placed from CPU 0 over UB: %s %s" % (w, u))
        b = [e for e in ev if e[1] == "BUDGET_BIND" and e[3][0] == uh]
        bound = None
        if g.check("E", len(b) == 1 and b[0][3][2] == (UH_BUDGET[0] << 32 | UH_BUDGET[1]),
                   "UH's binding %s" % b):
            bound = b[0][3][3]
            g.notes.append("E UH bound at tick %d with %d/%d" % ((bound,) + UH_BUDGET))
        p = after(u[0], preempt(1))
        if g.check("E", p is not None and p[3][0] == ub and p[3][1] in (1, 2),
                   "the remote wake placing UH did not preempt UB: %s" % (p,)):
            e_cause = p[3][1]
        if bound is not None:
            cyc = grade_cycles(g, "E", ev, uh, bound, UH_BUDGET[1], u[0], E_CYCLES, after, between)
            causes, lates, lows = [], [], []
            for i, (x, pk, r) in enumerate(cyc):
                tp = after(x[0], preempt(1))
                g.check("E", tp is not None and tp[3][0] == uh and tp[0] > pk[0] and tp[3][1] in (1, 2),
                        "cycle %d: the CPU was not taken from exhausted UH: %s" % (i + 1, tp))
                if tp is not None:
                    causes.append(tp[3][1])
                g.check("E", hb_lower(r) > hb_lower(pk),
                        "cycle %d: lower-priority UB did not run while UH was parked (%d -> %d)"
                        % (i + 1, hb_lower(pk), hb_lower(r)))
                lows.append(hb_lower(r) - hb_lower(pk))
                lates.append(r[3][1] - r[3][2])
                back = after(r[0], preempt(1))
                g.check("E", back is not None and back[3][0] == ub,
                        "cycle %d: UH did not take CPU 1 back from UB after its rejoin: %s" % (i + 1, back))
                nx = after(r[0], lambda e: e[1] == "BUDGET_EXHAUST" and e[3][0] == uh)
                g.check("E", nx is not None and hb_own(nx) > hb_own(r),
                        "cycle %d: UH did not run after its rejoin: %s" % (i + 1, nx))
            ex = [e for e in ev if e[1] == "BUDGET_EXHAUST" and e[3][0] == uh]
            g.notes.append("E cycles=%d exhaust-preemption causes=%s release lateness=%s UB heartbeats while parked=%s overruns=%s"
                           % (len(cyc), causes, lates, lows, sorted(set(e[3][2] >> 32 for e in ex))))

    # ── F ── the budgeted cooperative kernel thread K on CPU 0.
    k = tid["K"]
    w = [e for e in ev if e[1] == "WAKE" and e[3][0] == PHASE["F_RUN"] and e[3][1] == R["K"]]
    ks = [e for e in ev if e[1] == "KSTART" and e[3][0] == k]
    if g.check("F", len(w) == 1 and len(ks) == 1, "K's wake/start %s %s" % (w, ks)):
        ks = ks[0]
        g.check("F", ks[2] == 0 and ks[3][1] == (K_BUDGET[0] << 32 | K_BUDGET[1]),
                "K's context %s" % (ks,))
        bound = ks[3][2]
        cyc = grade_cycles(g, "F", ev, k, bound, K_BUDGET[1], ks[0], F_CYCLES, after, between)
        lates = []
        for i, (x, pk, r) in enumerate(cyc):
            g.check("F", (pk[3][2] >> 8) == 2 and (pk[3][2] & 0xFF) == PLAN["K"][1],
                    "cycle %d: K parked as %s" % (i + 1, pk))
            sp = between(x[0], pk[0], lambda e: e[1] == "SCHEDPT" and e[3][0] == k)
            g.check("F", len(sp) >= 1 and sp[-1][3][1] == 2,
                    "cycle %d: K's exhaustion was not honoured at its scheduling point: %s" % (i + 1, sp))
            kr = after(r[0], lambda e: e[1] == "KRESUMED" and e[3][0] == k)
            g.check("F", kr is not None and kr[3][1] == hb_own(pk) and kr[3][3] >= i + 1,
                    "cycle %d: K did not resume from its scheduling point after the rejoin: %s (parked at %d)"
                    % (i + 1, kr, hb_own(pk)))
            lates.append(r[3][1] - r[3][2])
        ke = [e for e in ev if e[1] == "KEND" and e[3][0] == k]
        g.check("F", len(ke) == 1 and ke[0][3][2] >= F_CYCLES, "K end %s" % ke)
        ex = [e for e in ev if e[1] == "BUDGET_EXHAUST" and e[3][0] == k]
        g.notes.append("F cycles=%d release lateness=%s overruns=%s" % (
            len(cyc), lates, sorted(set(e[3][2] >> 32 for e in ex))))

    # ── LEDGER ──
    led = {e[2]: e for e in ev if e[1] == "LEDGER"}
    if g.check("LEDGER", set(led) == {0, 1}, "ledgers %s" % sorted(led)):
        for cpu, e in sorted(led.items()):
            idle, unacc, ticks, arrivals, nbound = e[3]
            g.check("LEDGER", unacc == 0, "CPU %d: %d unaccounted ticks" % (cpu, unacc))
            g.check("LEDGER", 1 <= nbound <= 32, "CPU %d: %d contexts" % (cpu, nbound))
            g.check("LEDGER", ticks > 0, "CPU %d: no essential tick work recorded" % cpu)
            g.notes.append("LEDGER cpu=%d idle=%d unaccounted=%d essential_ticks=%d essential_arrivals=%d contexts=%d"
                           % (cpu, idle, unacc, ticks, arrivals, nbound))
    ctx = {e[3][0]: e for e in ev if e[1] == "CTX"}
    for name, cycles in (("UH", E_CYCLES), ("K", F_CYCLES)):
        c = ctx.get(tid[name])
        if g.check("LEDGER", c is not None, "%s's context missing at the dump" % name):
            ex, rel = c[3][3] >> 32, c[3][3] & 0xFFFFFFFF
            g.check("LEDGER", ex >= cycles and rel >= cycles, "%s: exhaustions %d releases %d" % (name, ex, rel))
            g.notes.append("CTX %s charged=%d overrun=%d exhaustions=%d releases=%d debt=%d remaining=%d"
                           % (name, c[3][1], c[3][2], ex, rel, c[3][4] >> 32, c[3][4] & 0xFFFFFFFF))
    return e_cause


# ─────────────────────────────── self-test ───────────────────────────────

def synth(mut=None, budget=False):
    """A passing record; `mut` names one corruption."""
    tid = {"H0": 0x1000_0001, "H1": 0x1000_0002, "E": 0x1000_0003, "L": 0x1000_0004,
           "DUMP": 0x1000_0005, "S": 0x1000_0000, "UA": 9700, "UB": 9701, "K": 0x1000_0006,
           "UH": 9702}
    ev = []

    def add(kind, cpu, *v):
        v = list(v) + [0] * (5 - len(v))
        ev.append((kind, cpu, v))

    def ph(n):
        add("PHASE", 0, PHASE[n])

    add("START", 0, R["S"], tid["S"], 0, 1)
    ph("SETUP")
    for role in ["H0", "H1", "E", "L", "DUMP"] + (["K"] if budget else []):
        add("START", PLAN[role][0], R[role], tid[role], PLAN[role][0], PLAN[role][1])
    add("USER", 0, R["UA"], tid["UA"], 0, 0)
    add("USER", 0, R["UB"], tid["UB"], 1, 1)
    ph("WARMUP")
    ph("A_ARM")
    ua_y = 0 if mut == "a_no_storm" else 10
    for r in range(1, 4):
        ph("A_WAIT")
        add("WAKE", 0, PHASE["A_WAIT"], R["H0"], tid["UA"], r, ua_y)
        add("PREEMPT", 0, tid["UA"] if mut != "a_identity" else tid["UB"], 2 if mut == "a_tick" else 1, PHASE["A_WAIT"])
        y_rec = ua_y - 20 if (mut == "a_yields_fall" and r == 3) else ua_y
        add("RUN", 0, R["H0"], tid["H0"], PHASE["A_WAIT"], r, (y_rec << 32) | 3)
        ph("A_ARM" if r < 3 else "B_ARM")
        ua_y += 7
    ph("B_WAIT")
    add("WAKE", 0, PHASE["B_WAIT"], R["H1"], tid["UB"], 500, 3)
    if mut != "b_no_preempt":
        add("PREEMPT", 1, tid["UB"], 2 if mut in ("ipi_never", "b_tick") else 1, PHASE["B_WAIT"])
    add("RUN", 1, R["H1"], tid["H1"], PHASE["B_WAIT"], 3, (ua_y << 32) | 3)
    ph("C_ARM")
    ph("C_WAIT")
    add("WAKE", 0, PHASE["C_WAIT"], R["E"], tid["UB"], 900, 3)
    if mut == "c_preempt":
        add("PREEMPT", 1, tid["UB"], 1, PHASE["C_WAIT"])
    else:
        add("DECLINED", 1, tid["UB"], 20, 20, 1, PHASE["C_WAIT"])
    if mut == "c_rotation":
        add("RUN", 1, R["E"], tid["E"], PHASE["C_WAIT"], 3, (ua_y << 32) | 3)
    for _ in range(1 if mut == "c_few_ticks" else 2):
        add("TICK", 1, tid["UB"], 0, 0, PHASE["C_WAIT"])
    ph("C_H1")
    add("WAKE", 0, PHASE["C_H1"], R["H1"], tid["UB"], 950, 3)
    add("PREEMPT", 1, tid["UB"], 2 if mut == "ipi_never" else 1, PHASE["C_H1"])
    add("RUN", 1, R["H1"], tid["H1"], PHASE["C_H1"], 3, (ua_y << 32) | 3)
    ph("C_AFTER")
    if mut == "c_tail_position":
        # UB went to the tail on its preemption: E, not UB, got CPU 1 when H1 parked.
        add("RUN", 1, R["E"], tid["E"], PHASE["C_AFTER"], 3, (ua_y << 32) | 3)
    else:
        for _ in range(4):
            add("TICK", 1, tid["UB"], 0, 0, PHASE["C_AFTER"])
    ph("C_YIELD")
    add("YREQ", 0, tid["UB"], 3, 4)
    if mut not in ("c_rotation", "c_tail_position"):
        add("RUN", 1, R["E"], tid["E"], PHASE["C_YIELD"], 3,
            (ua_y << 32) | (5 if mut == "c_position" else 4))
    ph("D_ARM")
    ph("D_L")
    add("WAKE", 0, PHASE["D_L"], R["L"], tid["UA"], 0, ua_y)
    add("PREEMPT", 0, tid["UA"], 1, PHASE["D_L"])
    add("LSTART", 0, tid["L"], PHASE["D_L"])
    ph("D_WAIT")
    add("WAKE", 0, PHASE["D_WAIT"], R["H0"], tid["L"])
    # The live RISC-V interleaving: L's resume from an EARLIER yield is recorded after the wake.
    add("LRESUMED", 0, tid["L"], 39, PHASE["D_WAIT"])
    add("SCHEDPT", 0, tid["L"], 1 if mut == "d_kept" else 2, PHASE["D_WAIT"])
    add("RUN", 0, R["H0"], tid["H0"], PHASE["D_WAIT"], 3, (ua_y << 32) | 4)
    if mut != "d_no_resume":
        add("LRESUMED", 0, tid["L"], 40, PHASE["D_WAIT"])
    add("LEND", 0, tid["L"], 48, 8)
    if budget:
        synth_budget(add, ph, tid, mut)
    ph("DONE")
    if mut == "timeout":
        add("TIMEOUT", 0, PHASE["DONE"])
    ph("DUMPING")
    out = []
    for p in range(2):
        for role in ROLES + BUDGET_ROLES:
            home, level = PLAN[role]
            t = tid[role] if (budget or role in ROLES) else 0
            out.append("RTW_ROLE pass=%d role=%s tid=0x%x home=%d level=%d" % (p, role, t, home, level))
        for seq, (kind, cpu, v) in enumerate(ev):
            out.append("RTW_REC pass=%d seq=%d kind=%s cpu=%d %s" % (
                p, seq, kind, cpu, " ".join("v%d=0x%x" % (i, x) for i, x in enumerate(v))))
        out.append("RTW_SEAL pass=%d records=%d taken=%d overflow=0 final_phase=DUMPING seal=0x0" % (p, len(ev), len(ev)))
    lines = []
    for i, body in enumerate(out):
        crc = fnv(body.encode())
        if mut == "damaged" and "kind=PREEMPT" in body and "cpu=1" in body:
            crc ^= 1  # both copies of B's preemption damaged: the record is lost
        lines.append("%s crc=0x%08x" % (body, crc))
    cfg = "YARM_SCHED_BUDGET req=%s profile=rt result=%s route=construction clock=global_scheduler_tick clock_owner=cpu0" % (
        ("on", "enforced") if budget else ("off", "off"))
    return cfg + "\n" + "\n".join(lines) + "\nRTW_DONE\n"


def synth_budget(add, ph, tid, mut):
    uh, ub, k = tid["UH"], tid["UB"], tid["K"]
    hb = lambda own, low: (own << 32) | low
    ph("E_ARM")
    ph("E_PLACE")
    add("WAKE", 0, PHASE["E_PLACE"], R["UH"], ub, 900, 4)
    bound = 1000
    add("BUDGET_BIND", 0, uh, 3, UH_BUDGET[0] << 32 | UH_BUDGET[1], bound, hb(0, 900))
    add("USER", 0, R["UH"], uh, 1, 1, 900)
    add("PREEMPT", 1, ub, 2 if mut == "ipi_never" else 1, PHASE["E_PLACE"])
    ph("E_RUN")
    own, low, t = 0, 900, bound
    for c in range(E_CYCLES + 1):
        own += 50
        t += 2
        add("BUDGET_EXHAUST", 1, uh, t, 0, 0, hb(own, low))
        if c == E_CYCLES:
            break
        add("BUDGET_PARK", 0, uh, t, 1 << 8 | 8, 0, hb(own, low))
        add("PREEMPT", 1, uh, 2, PHASE["E_RUN"])
        low += 0 if (mut == "e_lower_starved" and c == 1) else 40
        rel = t - 2 + UH_BUDGET[1] + (1 if (mut == "e_off_grid" and c == 0) else 0)
        run_parked = 5 if (mut == "e_ran_parked" and c == 2) else 0
        add("BUDGET_REPLENISH", 1, uh, rel + 1, rel, 1 << 32 | 1 << 16 | 2, hb(own + run_parked, low))
        if mut == "e_no_return" and c == E_CYCLES - 1:
            add("PREEMPT", 1, tid["H1"], 2, PHASE["E_RUN"])
        else:
            add("PREEMPT", 1, ub, 2, PHASE["E_RUN"])
        own += run_parked
        t = rel
    ph("F_ARM")
    ph("F_RUN")
    add("WAKE", 0, PHASE["F_RUN"], R["K"], 9700)
    kb = 40
    add("KSTART", 0, k, K_BUDGET[0] << 32 | K_BUDGET[1], kb, 2, PHASE["F_RUN"])
    prog, t = 0, kb + 10 * 3
    for c in range(F_CYCLES):
        prog += 30
        add("BUDGET_EXHAUST", 0, k, t + 2, 0, 0, hb(prog, 10))
        add("SCHEDPT", 0, k, 1 if (mut == "f_kept" and c == 1) else 2, PHASE["F_RUN"])
        add("BUDGET_PARK", 0, k, t + 2, 2 << 8 | 6, 0, hb(prog, 10))
        add("BUDGET_REPLENISH", 0, k, t + 10, t + 10, 1 << 32 | 1 << 16 | 2, hb(prog, 20))
        add("KRESUMED", 0, k, prog + (1 if (mut == "f_moved" and c == 0) else 0), PHASE["F_RUN"], c + 1, 0)
        t += 10
    add("KEND", 0, k, prog + 5, F_CYCLES)
    add("LEDGER", 0, 100, 0, 500, 3, 12)
    add("LEDGER", 1, 80, 7 if mut == "ledger_unaccounted" else 0, 500, 9, 6)
    add("CTX", 1, uh, 8, 0, (E_CYCLES + 1) << 32 | (E_CYCLES + 2), 2)
    add("CTX", 0, k, 6, 0, F_CYCLES << 32 | 8, 2)


MUTANTS = {
    "a_tick": "A", "a_identity": "A", "a_no_storm": "A", "a_yields_fall": "A", "b_no_preempt": "B", "c_preempt": "C",
    "c_rotation": "C", "c_few_ticks": "C", "c_position": "C", "c_tail_position": "C", "d_kept": "D", "d_no_resume": "D", "timeout": "SEAL",
    "damaged": "B", "ipi_never": "IPI",
}


BUDGET_MUTANTS = {
    "e_lower_starved": "E", "e_off_grid": "E", "e_ran_parked": "E", "e_no_return": "E",
    "f_kept": "F", "f_moved": "F", "ledger_unaccounted": "LEDGER", "ipi_never": "IPI",
}


def self_test():
    for budget in (False, True):
        for m in (None, "b_tick"):
            g = grade(synth(m, budget=budget), budget=budget)
            if g.fails:
                print("SELF-TEST FAIL: the passing record (budget=%s, %s) failed: %s" % (budget, m, g.fails))
                return 1
    bad = 0
    # A budget-off record graded as budget-on, and the reverse, are refused.
    if not [f for f in grade(synth(), budget=True).fails if f.startswith("CFG:")]:
        print("SELF-TEST FAIL: a budget-off record passed CFG as budget-on")
        bad += 1
    if not [f for f in grade(synth(budget=True), budget=False).fails if f.startswith("CFG:")]:
        print("SELF-TEST FAIL: a budget-on record passed CFG as budget-off")
        bad += 1
    for mut, rule in BUDGET_MUTANTS.items():
        g = grade(synth(mut, budget=True), budget=True)
        hit = [f for f in g.fails if f.startswith(rule + ":")]
        if not hit:
            print("SELF-TEST FAIL: budget mutant %s not caught by %s (fails=%s)" % (mut, rule, g.fails))
            bad += 1
    for mut, rule in MUTANTS.items():
        g = grade(synth(mut))
        hit = [f for f in g.fails if f.startswith(rule + ":")]
        if not hit:
            print("SELF-TEST FAIL: mutant %s not caught by %s (fails=%s)" % (mut, rule, g.fails))
            bad += 1
    if bad:
        return 1
    print("RTW_GRADER_SELF_TEST result=pass mutants=%d budget_mutants=%d" % (len(MUTANTS), len(BUDGET_MUTANTS) + 2))
    return 0


def main(argv):
    if "--self-test" in argv:
        return self_test()
    arch, smp, path, budget = "?", "?", None, False
    it = iter(argv)
    for a in it:
        if a == "--budget":
            budget = next(it) in ("on", "1")
        elif a == "--arch":
            arch = next(it)
        elif a == "--smp":
            smp = next(it)
        else:
            path = a
    text = open(path, "rb").read().decode("utf-8", "replace")
    g = grade(text, budget=budget)
    for n in g.notes:
        print("[rtw-grade] " + n)
    for f in g.fails:
        print("[rtw-grade][fail] " + f)
    rules = ["CFG", "ID", "SEAL", "PHASES", "A", "B", "C", "D", "IPI"] + (["E", "F", "LEDGER"] if budget else [])
    for r in rules:
        print("RTW_RULE arch=%s smp=%s rule=%s result=%s" % (
            arch, smp, r, "fail" if any(f.startswith(r + ":") for f in g.fails) else "pass"))
    print("RTW_GRADE arch=%s smp=%s budget=%s result=%s fails=%d" % (
        arch, smp, "on" if budget else "off", "fail" if g.fails else "pass", len(g.fails)))
    return 1 if g.fails else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
