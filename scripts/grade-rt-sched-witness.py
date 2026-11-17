#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""RT-SCHED1A — grade the live RT selection witness (`kernel::rt_witness`).

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
  B      (remote wake, real IPI) H1's wake is issued on CPU 0 while UB is CPU 1's current; the
         first CPU 1 preemption after it is UB's, cause 1; H1 runs after it, on CPU 1.
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
"""
import re
import sys

PHASE = {n: i for i, n in enumerate([
    "OFF", "WAIT_IDLE", "SETUP", "WARMUP", "A_ARM", "A_WAIT", "B_ARM", "B_WAIT", "C_ARM",
    "C_WAIT", "C_H1", "C_AFTER", "C_YIELD", "D_ARM", "D_L", "D_WAIT", "DONE", "DUMPING"])}
ROLES = ["H0", "H1", "E", "L", "DUMP", "S", "UA", "UB"]
R = {n: i for i, n in enumerate(ROLES)}
PLAN = {"H0": (0, 4), "H1": (1, 4), "E": (1, 20), "L": (0, 16), "DUMP": (0, 3), "S": (0, 1),
        "UA": (0, 20), "UB": (1, 20)}
EXPECT_PHASES = ["SETUP", "WARMUP"] + ["A_ARM", "A_WAIT"] * 3 + [
    "B_ARM", "B_WAIT", "C_ARM", "C_WAIT", "C_H1", "C_AFTER", "C_YIELD", "D_ARM", "D_L", "D_WAIT",
    "DONE", "DUMPING"]
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


def grade(text):
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

    # ── ID ──
    for role in ROLES:
        if not g.check("ID", role in roles, "role %s missing" % role):
            return g
        tid, home, level = roles[role]
        g.check("ID", (home, level) == PLAN[role],
                "%s home/level %s != %s" % (role, (home, level), PLAN[role]))
        g.check("ID", tid != 0, "%s has no identity" % role)
    tid = {r: roles[r][0] for r in ROLES}
    g.check("ID", tid["UA"] == 9700 and tid["UB"] == 9701, "user TIDs %s %s" % (tid["UA"], tid["UB"]))
    g.check("ID", len(set(tid.values())) == len(ROLES), "identities not distinct")
    for role in ["H0", "H1", "E", "L", "DUMP", "S"]:
        st = [e for e in ev if e[1] == "START" and e[3][0] == R[role]]
        g.check("ID", len(st) == 1 and st[0][3][1] == tid[role] and st[0][2] == PLAN[role][0],
                "%s START %s" % (role, st))
    for role, cpu in (("UA", 0), ("UB", 1)):
        u = [e for e in ev if e[1] == "USER" and e[3][0] == R[role]]
        g.check("ID", len(u) == 1 and u[0][3][1] == tid[role] and u[0][3][3] == cpu,
                "%s placement %s" % (role, u))
    g.check("SEAL", not [e for e in ev if e[1] == "TIMEOUT"],
            "TIMEOUT %s" % [e for e in ev if e[1] == "TIMEOUT"])

    # ── PHASES ──
    phases = [e[3][0] for e in ev if e[1] == "PHASE"]
    names = [k for v in phases for k, i in PHASE.items() if i == v]
    g.check("PHASES", names == EXPECT_PHASES, "sequence %s" % names)

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
    w = wake("B_WAIT", "H1")
    if g.check("B", len(w) == 1, "wake %s" % w):
        w = w[0]
        g.check("B", w[2] == 0 and w[3][2] == tid["UB"], "wake not from CPU 0 over UB %s" % (w,))
        p = after(w[0], preempt(1))
        ok = g.check("B", p is not None and p[3][0] == tid["UB"] and p[3][1] == 1,
                     "first CPU 1 preemption after the remote wake %s" % (p,))
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
    return g


# ─────────────────────────────── self-test ───────────────────────────────

def synth(mut=None):
    """A passing record; `mut` names one corruption."""
    tid = {"H0": 0x1000_0001, "H1": 0x1000_0002, "E": 0x1000_0003, "L": 0x1000_0004,
           "DUMP": 0x1000_0005, "S": 0x1000_0000, "UA": 9700, "UB": 9701}
    ev = []

    def add(kind, cpu, *v):
        v = list(v) + [0] * (5 - len(v))
        ev.append((kind, cpu, v))

    def ph(n):
        add("PHASE", 0, PHASE[n])

    add("START", 0, R["S"], tid["S"], 0, 1)
    ph("SETUP")
    for role in ["H0", "H1", "E", "L", "DUMP"]:
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
        add("PREEMPT", 1, tid["UB"], 1, PHASE["B_WAIT"])
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
    add("PREEMPT", 1, tid["UB"], 1, PHASE["C_H1"])
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
    ph("DONE")
    if mut == "timeout":
        add("TIMEOUT", 0, PHASE["DONE"])
    ph("DUMPING")
    out = []
    for p in range(2):
        for role in ROLES:
            home, level = PLAN[role]
            out.append("RTW_ROLE pass=%d role=%s tid=0x%x home=%d level=%d" % (p, role, tid[role], home, level))
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
    return "\n".join(lines) + "\nRTW_DONE\n"


MUTANTS = {
    "a_tick": "A", "a_identity": "A", "a_no_storm": "A", "a_yields_fall": "A", "b_no_preempt": "B", "c_preempt": "C",
    "c_rotation": "C", "c_few_ticks": "C", "c_position": "C", "c_tail_position": "C", "d_kept": "D", "d_no_resume": "D", "timeout": "SEAL",
    "damaged": "B",
}


def self_test():
    g = grade(synth())
    if g.fails:
        print("SELF-TEST FAIL: the passing record failed: %s" % g.fails)
        return 1
    bad = 0
    for mut, rule in MUTANTS.items():
        g = grade(synth(mut))
        hit = [f for f in g.fails if f.startswith(rule + ":")]
        if not hit:
            print("SELF-TEST FAIL: mutant %s not caught by %s (fails=%s)" % (mut, rule, g.fails))
            bad += 1
    if bad:
        return 1
    print("RTW_GRADER_SELF_TEST result=pass mutants=%d" % len(MUTANTS))
    return 0


def main(argv):
    if "--self-test" in argv:
        return self_test()
    arch, smp, path = "?", "?", None
    it = iter(argv)
    for a in it:
        if a == "--arch":
            arch = next(it)
        elif a == "--smp":
            smp = next(it)
        else:
            path = a
    text = open(path, "rb").read().decode("utf-8", "replace")
    g = grade(text)
    for n in g.notes:
        print("[rtw-grade] " + n)
    for f in g.fails:
        print("[rtw-grade][fail] " + f)
    rules = ["ID", "SEAL", "PHASES", "A", "B", "C", "D"]
    for r in rules:
        print("RTW_RULE arch=%s smp=%s rule=%s result=%s" % (
            arch, smp, r, "fail" if any(f.startswith(r + ":") for f in g.fails) else "pass"))
    print("RTW_GRADE arch=%s smp=%s result=%s fails=%d" % (arch, smp, "fail" if g.fails else "pass", len(g.fails)))
    return 1 if g.fails else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
