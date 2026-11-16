#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SCHED-TIME-FOUNDATION1 C — grade one live sleep / clock / rotation witness boot.

Input: the normalized serial log of a boot built with `sched-time-witness` (kernel and init) and
booted with `yarm.futex_requeue_witness=1`. Init's cell (init/service.rs, `sched_time_witness`)
logs, per graded sleep, `STW_CALL case n clock flags req t0 bound` and `STW_RET case n code t0 t1
bound late_ns` (t0/t1: ring-3 reads of the sleep's clock around the call; bound: the deadline the
kernel must honour). The kernel (`kernel::sleep_witness`) logs `STW_SLEEP_REGISTER tid gen
deadline now`, `STW_SLEEP_DONE cpu home tid gen deadline now current`, `STW_SLEEP_REFUSED cpu home
tid gen deadline now`, `STW_SLEEP_KILLED tid gen`, `STW_KTHREAD_START`, `STW_KTHREAD_BEAT ... n
mono`, `STW_KTHREAD_DONE`. SCHED-SAT-EVIDENCE1: every refused placement of a sleep or a receive
timeout also logs `SATW_REFUSED class cpu home tid gen prio queued reserved cap full deadline now
facts` (`kernel::sat_witness`): the home queue's entries and promised slots at the waiter's
priority, captured under that CPU's lock BY the refusal; `full=1` iff queued + reserved >= cap.

Markers fail closed: a damaged or unknown STW line fails the boot; nothing is repaired.

Rules (the brief's twelve cases):
  every call   code=0 and t1 >= bound (a sleep never ends early); a call with a future deadline
               (every case but zero / abs_past, and a `short` 1 ms call the kernel may already find
               due, which then registers nothing) has exactly one REGISTER of its tid between CALL
               and RET, a generation strictly
               above that tid's previous one, and exactly ONE DONE of (tid, gen) in the whole log,
               between REGISTER and RET, with now >= deadline; a zero / past call registers nothing.
  1 zero, 2 short x3, 3 long, 4 abs (and abs_past): present, as above.
  5 two tasks   two_h (H) and two_w (W) both graded; two_h returns before two_w.
  6 exit        STW_SLEEP_KILLED of the victim's registered generation; no DONE of the victim ever;
               STW_KILL_CHECK x_returned=0 and no STW_X_RETURNED.
  7 generations 16 `cycle` calls, each its own fresh generation and single DONE (the every-call rule).
  8 full queue  W's `full` cycle: at least one SATW_REFUSED class=sleep of its (tid, gen) with
               facts=ok, full=1, home = its DONE's home and now >= its deadline, logged before H's
               STW_HOLD_END (the hold was still in force: H topped up all through it) and before
               the DONE; every refusal of the cycle precedes its one DONE; STW_HOLD_END bounded=0;
               STW_SATURATED early=1; STW_ROTATION parked=fillers. Spawn refusals and filler
               counts are diagnostics only: they earn nothing.
  9 all blocked three `idle` calls during which no other thread of the cell logs anything.
  10 cross-CPU  eight `cross` sleeps while two spinner threads trap on (balanced placement
               spreads them); at least one DONE in the whole log with cpu != home.
  11 kthreads   STW_KTHREAD_START ok, >= 2 beats, and at least one DONE between the first and the
               last beat.
  12 registers  every STW_ERR code == want (Unavailable before realtime, InvalidArgs for a bad clock,
               flag or nsec); every graded call returned code=0 through its resume boundary.
  IPC capacity R's graded timed receive (STW_RECV_CALL .. STW_RECV_RET), entered only after H
               reported every queue full (sat_ready=1; R stayed runnable until then, since a
               blocked task cannot be woken onto a full queue), so saturation precedes its
               deadline (STW_SAT_RECV early=1): at least one SATW_REFUSED
               class=recv of R between them, due (now >= deadline), with facts=ok, full=1, before
               STW_HOLD_END; a U8_RECV_TIMEOUT_PLACEMENT_DEFERRED of R for the same
               wait generation; then EXACTLY ONE U8_RECV_TIMEOUT_SETTLED of (R, that generation),
               after the first proving refusal and before R's return; R returns outcome=none (its
               timeout) exactly once. R's unsaturated calibration receive returns outcome=none. Any
               other thread's deferral (periodic service traffic) earns nothing.
  STW_WAITER_DONE fails=0, STW_DONE result=ok, no STW_FAIL, plus the standard core smoke.

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

N = r"\d+"
K = {
    "REG": ("STW_SLEEP_REGISTER", dict(tid=N, gen=N, deadline=N, now=N)),
    "DONE": ("STW_SLEEP_DONE", dict(cpu=N, home=N, tid=N, gen=N, deadline=N, now=N, current=N)),
    "REFUSED": ("STW_SLEEP_REFUSED", dict(cpu=N, home=N, tid=N, gen=N, deadline=N, now=N)),
    "KILLED": ("STW_SLEEP_KILLED", dict(tid=N, gen=N)),
    "KSTART": ("STW_KTHREAD_START", dict(kthread=r"0x[0-9a-f]+", cpu=N)),
    "BEAT": ("STW_KTHREAD_BEAT", dict(kthread=r"0x[0-9a-f]+", cpu=N, n=N, mono=N)),
    "KDONE": ("STW_KTHREAD_DONE", dict(kthread=r"0x[0-9a-f]+", cpu=N, beats=N)),
    "SATW": ("SATW_REFUSED", {"class": r"sleep|futex|recv", "cpu": N, "home": N, "tid": N, "gen": N, "n": N,
                              "prio": N, "queued": N, "reserved": N, "cap": N, "full": r"[01]",
                              "deadline": N, "now": N, "facts": r"ok|lost"}),
}
U = {
    "BEGIN": dict(s_tid=N), "THREADS": dict(waiter=N, helper=N, victim=N, receiver=N),
    "CLOCK": dict(rt_before=N), "ERR": dict(case=r"\w+", code=N, want=N), "MONO": dict(m0=N, m1=N),
    "CALL": dict(case=r"\w+", n=N, clock=N, flags=N, req=N, t0=N, bound=N),
    "RET": dict(case=r"\w+", n=N, code=N, t0=N, t1=N, bound=N, late_ns=N),
    "KILL_CHECK": dict(x_returned=N), "X_SLEEP": dict(req=N), "X_RETURNED": dict(code=N),
    "RT_SET": dict(code=N),
    "SATURATED": dict(fillers=N, first=N, refusals=N, full_at=N, w_deadline=N, early=N),
    "ROTATION": dict(fillers=N, parked=N, holds=N, waits=N),
    "WAITER_DONE": dict(calls=N, fails=N),
    "DONE": dict(waiter=N, helper=N, victim=N, calls=N, fails=N, result=r"ok|fail"),
    "FAIL": dict(what=r"\w+"),
    "SPINNER": dict(tid=N),
    "RECV_CAL": dict(ticks=N, elapsed=N, tick_est=N, outcome=r"none|msg|err"),
    "RECV_CALL": dict(ticks=N, t0=N, earliest=N, due_by=N, sat_ready=r"[01]"),
    "SAT_RECV": dict(r_deadline=N, full_at=N, early=r"[01]"),
    "RECV_TOPUP": dict(filled=r"[01]"),
    "RECV_RET": dict(outcome=r"none|msg|err", t0=N, t1=N, due_by=N),
    "HOLD_END": dict(end=N, w_deadline=N, r_due_by=N, fillers=N, topups=N, refusals=N, holds=N,
                     bounded=r"[01]"),
}
COUNTS = dict(zero=1, short=3, long=1, abs=1, abs_past=1, idle=3, cycle=16, cross=8, two_w=1, two_h=1,
              kill_wait=1, rt_rel=1, rt_abs=1, full=1)
NO_REGISTER = {"zero", "abs_past"}
# A 1 ms request may already be due when the kernel registers it (the registration reads the clock
# inside its hold): a legal immediate return, with no registration and no completion.
MAY_BE_DUE = {"short"}
FATAL = ("KERNEL PANIC", "panicked at", "KTHREAD_STACK_OVERFLOW")


def kv(s):
    return dict(re.findall(r"(\w+)=([^\s,]+)", s))


def parse(line):
    for kind, (head, fields) in K.items():
        if line.startswith(head + " ") or line == head:
            if kind == "KILLED" and "result=not_sleeping" in line:
                return "DAMAGED", "STW_SLEEP_KILLED: the fixture found no sleeping victim"
            if kind == "KSTART" and "result=fail" in line:
                return "DAMAGED", "STW_KTHREAD_START failed"
            d = kv(line)
            for k, pat in fields.items():
                if k not in d or not re.fullmatch(pat, d[k]):
                    return "DAMAGED", f"{head}: field `{k}` missing or malformed"
            return kind, d
    m = re.match(r"^USER_LOG tid=(\d+) msg=STW_([A-Z_]+?) (.*)$", line)
    if m:
        name = m.group(2)
        if name not in U:
            return "DAMAGED", f"STW_{name}: unknown cell line"
        d = kv(m.group(3))
        for k, pat in U[name].items():
            if k not in d or not re.fullmatch(pat, d[k]):
                return "DAMAGED", f"STW_{name}: field `{k}` missing or malformed"
        d["utid"] = m.group(1)
        return "U_" + name, d
    if "STW_" in line:
        return "DAMAGED", "damaged witness marker"
    if line.startswith("U8_RECV_TIMEOUT_PLACEMENT_DEFERRED"):
        return "IPC_DEFER", kv(line)
    if line.startswith("U8_RECV_TIMEOUT_SETTLED"):
        return "IPC_SETTLED", kv(line)
    return None, None


def grade(lines):
    fails, facts = [], {}
    fail = fails.append
    ev = []
    for i, raw in enumerate(lines):
        l = raw.strip()
        for f in FATAL:
            if f in l:
                fail(f"fatal marker: {l[:160]}")
        kind, d = parse(l)
        if kind is None:
            continue
        if kind == "DAMAGED":
            fail(f"line {i + 1}: {d}: {l[:140]}")
            continue
        d["at"], d["kind"] = i, kind
        ev.append(d)
    only = lambda k: [e for e in ev if e["kind"] == k]

    def one(k):
        xs = only(k)
        if len(xs) != 1:
            fail(f"expected exactly one {k}, saw {len(xs)}")
            return None
        return xs[0]

    for f in only("U_FAIL"):
        fail(f"cell reported STW_FAIL what={f['what']}")
    th = one("U_THREADS")
    if th is None:
        return fails, facts
    W, H, X, R = th["waiter"], th["helper"], th["victim"], th["receiver"]
    cell = {W, H, X, R}

    # every call
    calls = only("U_CALL")
    rets = {(r["utid"], r["n"]): r for r in only("U_RET")}
    dones = {}
    for dn in only("DONE"):
        dones.setdefault((dn["tid"], dn["gen"]), []).append(dn)
    last_gen = {}
    per_case = {}
    graded = []
    for c in calls:
        r = rets.get((c["utid"], c["n"]))
        if r is None:
            fail(f"call {c['n']} ({c['case']}): never returned")
            continue
        per_case[c["case"]] = per_case.get(c["case"], 0) + 1
        if r["code"] != "0":
            fail(f"call {c['n']} ({c['case']}): code {r['code']}")
        if int(r["t1"]) < int(r["bound"]):
            fail(f"call {c['n']} ({c['case']}): returned EARLY, t1 {r['t1']} < bound {r['bound']}")
        regs = [e for e in only("REG") if e["tid"] == c["utid"] and c["at"] < e["at"] < r["at"]]
        if c["case"] in NO_REGISTER:
            if regs:
                fail(f"call {c['n']} ({c['case']}): a due-at-once sleep registered")
            graded.append(dict(c=c, r=r, g=None))
            continue
        if not regs and c["case"] in MAY_BE_DUE:
            graded.append(dict(c=c, r=r, g=None))
            continue
        if len(regs) != 1:
            fail(f"call {c['n']} ({c['case']}): {len(regs)} registrations")
            continue
        g = regs[0]
        if int(g["gen"]) <= last_gen.get(c["utid"], -1):
            fail(f"call {c['n']}: generation {g['gen']} not fresh")
        last_gen[c["utid"]] = int(g["gen"])
        ds = dones.get((g["tid"], g["gen"]), [])
        if len(ds) != 1:
            fail(f"call {c['n']} ({c['case']}): {len(ds)} completions of gen {g['gen']}")
            continue
        d = ds[0]
        if not (g["at"] < d["at"] < r["at"]):
            fail(f"call {c['n']}: its completion lies outside REGISTER..RET")
        if int(d["now"]) < int(d["deadline"]) or d["deadline"] != g["deadline"]:
            fail(f"call {c['n']}: kernel completed at {d['now']} before deadline {d['deadline']}")
        graded.append(dict(c=c, r=r, g=g, d=d))
    for case, want in COUNTS.items():
        if per_case.get(case, 0) != want:
            fail(f"case {case}: {per_case.get(case, 0)} graded calls, expected {want}")
    by = lambda case: [x for x in graded if x["c"]["case"] == case]

    # 5
    if by("two_h") and by("two_w") and not by("two_h")[0]["r"]["at"] < by("two_w")[0]["r"]["at"]:
        fail("5: the earlier deadline (two_h) did not return first")
    # 6
    xregs = [e for e in only("REG") if e["tid"] == X]
    kills = only("KILLED")
    if len(xregs) != 1 or len(kills) != 1 or kills[0]["tid"] != X or kills[0]["gen"] != xregs[0]["gen"]:
        fail("6: the victim's one sleep was not ended by the exit fixture")
    if any(dn["tid"] == X for dn in only("DONE")):
        fail("6: the dead victim's sleep completed (resurrection)")
    kc = one("U_KILL_CHECK")
    if (kc and kc["x_returned"] != "0") or only("U_X_RETURNED"):
        fail("6: the victim returned from its sleep")
    # 8
    hold = one("U_HOLD_END")
    if hold and hold["bounded"] != "0":
        fail("8: H's hold hit its bound (R never published its deadline, or the hold never ended)")
    hold_at = hold["at"] if hold else -1

    def proving(sat, home):
        """The refusals that prove a full home queue while H still held it, and why others don't."""
        ok, why = [], []
        for e in sat:
            room = int(e["queued"]) + int(e["reserved"]) < int(e["cap"])
            if e["facts"] != "ok":
                why.append("facts lost")
            elif (e["full"] == "1") == room:
                why.append("inconsistent facts")
            elif e["full"] != "1":
                why.append(f"room left ({e['queued']}+{e['reserved']}<{e['cap']})")
            elif home is not None and e["home"] != home:
                why.append(f"home {e['home']} != {home}")
            elif not e["at"] < hold_at:
                why.append("after the hold ended")
            else:
                ok.append(e)
        return ok, why

    full = by("full")
    if full and full[0].get("g"):
        fg, fd = full[0]["g"], full[0]["d"]
        refs = [e for e in only("REFUSED") if e["tid"] == fg["tid"] and e["gen"] == fg["gen"]]
        sat = [e for e in only("SATW")
               if e["class"] == "sleep" and e["tid"] == fg["tid"] and e["gen"] == fg["gen"]]
        sat_due = [e for e in sat if int(e["now"]) >= int(fg["deadline"])]
        good, why = proving(sat_due, fd["home"])
        if not good:
            fail(f"8: no refusal proves W's home queue (cpu {fd['home']}) full at its deadline while "
                 f"held: {len(sat)} refusals, {len(sat_due)} at/after the deadline, rejected: {sorted(set(why))}")
        if not refs:
            fail("8: the full-queue sleep was never refused a placement (queue not full when due)")
        elif not all(e["at"] < fd["at"] for e in refs + sat):
            fail("8: a refusal after the completion")
        facts["full_refusals"] = len(refs)
        facts["full_proving"] = len(good)
    sat = one("U_SATURATED")
    if sat and sat["early"] != "1":
        fail("8: saturation completed after the deadline")
    rot = one("U_ROTATION")
    if rot and rot["parked"] != rot["fillers"]:
        fail(f"8: only {rot['parked']} of {rot['fillers']} fillers ran after the release")
    if sat:
        facts["fillers"] = sat["fillers"]
    # 9
    for x in by("idle"):
        noisy = [e for e in ev if e["kind"].startswith("U_") and e.get("utid") in cell - {W}
                 and x["c"]["at"] < e["at"] < x["r"]["at"]]
        if noisy:
            fail(f"9: {noisy[0]['kind']} from another cell thread during an idle sleep")
    facts["idle_current0"] = sum(1 for x in by("idle") if x.get("d") and x["d"]["current"] == "0")
    # 10
    cross = [dn for dn in only("DONE") if dn["cpu"] != dn["home"]]
    if not cross:
        fail("10: no sleep was placed from another CPU")
    facts["cross_cpu"] = len(cross)
    # 11
    beats = only("BEAT")
    if one("KSTART") is None or len(beats) < 2:
        fail("11: the kernel thread did not run")
    else:
        lo, hi = beats[0]["at"], beats[-1]["at"]
        during = [dn for dn in only("DONE") if lo < dn["at"] < hi]
        if not during:
            fail("11: no sleep completed while the kernel thread ran")
        facts["kthread_beats"], facts["done_during_kthread"] = len(beats), len(during)
    # 12
    errs = only("U_ERR")
    if len(errs) != 5:
        fail(f"12: {len(errs)} error-lane probes, expected 5")
    for e in errs:
        if e["code"] != e["want"]:
            fail(f"12: {e['case']} returned code {e['code']}, expected {e['want']}")
    ck = one("U_CLOCK")
    if ck and ck["rt_before"] != "11":
        fail("realtime readable before it was set")
    rs = one("U_RT_SET")
    if rs and rs["code"] != "0":
        fail(f"realtime set refused: code {rs['code']}")
    # IPC timeout capacity — R's own graded receive, nothing incidental
    cal = one("U_RECV_CAL")
    if cal and cal["outcome"] != "none":
        fail(f"IPC: R's calibration receive returned {cal['outcome']}, not its timeout")
    rc, rr = one("U_RECV_CALL"), one("U_RECV_RET")
    if rc and rr:
        if rc["utid"] != R or rr["utid"] != R:
            fail("IPC: the graded receive was not R's")
        if rc["sat_ready"] != "1":
            fail("IPC: R entered its graded receive before H reported saturation")
        sr = one("U_SAT_RECV")
        if sr and sr["early"] != "1":
            fail("IPC: the queues were not full before R's receive could fall due")
        sat = [e for e in only("SATW") if e["class"] == "recv" and e["tid"] == R
               and rc["at"] < e["at"] < rr["at"] and int(e["now"]) >= int(e["deadline"])]
        good, why = proving(sat, None)
        if not good:
            fail(f"IPC: no refusal proves R's home queue full when its receive timeout was due while "
                 f"held: {len(sat)} refusals, rejected: {sorted(set(why))}")
        else:
            g, first = good[0]["gen"], good[0]
            if any(e["gen"] != g for e in sat):
                fail("IPC: R's refusals name more than one wait generation")
            defers = [e for e in only("IPC_DEFER") if e.get("tid") == R and e.get("wait_generation") == g]
            if not defers:
                fail("IPC: no U8_RECV_TIMEOUT_PLACEMENT_DEFERRED of R's graded receive")
            settles = [e for e in only("IPC_SETTLED") if e.get("tid") == R and e.get("wait_generation") == g]
            if len(settles) != 1:
                fail(f"IPC: {len(settles)} settlements of R's graded receive timeout (gen {g}), expected exactly one")
            elif not first["at"] < settles[0]["at"] < rr["at"]:
                fail("IPC: R's timeout settled outside refusal..return")
            elif any(e["at"] > settles[0]["at"] for e in sat):
                fail("IPC: a refusal of R's receive after it settled")
            facts["ipc_proving"] = len(good)
            tu = only("U_RECV_TOPUP")
            facts["ipc_topup"] = tu[0]["filled"] if tu else "none"
            facts["ipc_deferrals"] = len(defers)
        if rr["outcome"] != "none":
            fail(f"IPC: R's graded receive returned {rr['outcome']}, not its timeout")
    else:
        fail("IPC: R's graded receive did not run")
    # end
    wd = one("U_WAITER_DONE")
    if wd and wd["fails"] != "0":
        fail(f"W reported {wd['fails']} failures")
    dn = one("U_DONE")
    if dn and dn["result"] != "ok":
        fail("STW_DONE result=fail")
    late = [int(x["r"]["late_ns"]) for x in graded]
    if late:
        facts["late_max_ns"] = max(late)
        facts["late_median_ns"] = sorted(late)[len(late) // 2]
    facts["calls"] = len(graded)
    return fails, facts


def core_verdict(core_rc, core_logs):
    reasons = []
    for path in core_logs:
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                for l in fh:
                    t = l.strip()
                    if (t.startswith("[fail]") or t.startswith("[error]")) and "check(s) failed" not in t \
                            and "smoke FAILED" not in t and "witness FAILED" not in t:
                        reasons.append(t)
        except OSError:
            pass
    reasons = sorted(set(reasons))
    return (core_rc in (None, 0)) and not reasons, reasons


# ── self-test ────────────────────────────────────────────────────────────────────────────────
def synth():
    L = []
    u = lambda t, s: L.append(f"USER_LOG tid={t} msg={s}")
    W, H, X, R = 10010, 10011, 10012, 10015
    u(1, "STW_BEGIN s_tid=1 fillers_max=176")
    u(1, f"STW_THREADS waiter={W} helper={H} victim={X} receiver={R}")
    u(W, "STW_CLOCK rt_before=11")
    for c, w in [("rt_uninit_rel", 11), ("rt_uninit_abs", 11), ("bad_clock", 2), ("bad_flags", 2), ("bad_nsec", 2)]:
        u(W, f"STW_ERR case={c} code={w} want={w}")
    u(W, "STW_MONO m0=5 m1=6")
    L.append("STW_KTHREAD_START kthread=0x1 cpu=0")
    L.append("STW_KTHREAD_BEAT kthread=0x1 cpu=0 n=1 mono=1 yields=0")
    st = dict(n=0, gen={}, t=1000)

    def sleep(tid, case, req, register=True, cpu=0, home=0, mid=()):
        st["n"] += 1
        n, t0 = st["n"], st["t"]
        u(tid, f"STW_CALL case={case} n={n} clock=1 flags=0 req={req} t0={t0} bound={t0 + req}")
        if register:
            g = st["gen"].get(tid, 0) + 1
            st["gen"][tid] = g
            L.append(f"STW_SLEEP_REGISTER tid={tid} gen={g} deadline={t0 + req} now={t0}")
            for m in mid:
                L.append(m.format(tid=tid, gen=g, d=t0 + req))
            L.append(f"STW_SLEEP_DONE cpu={cpu} home={home} tid={tid} gen={g} deadline={t0 + req} now={t0 + req + 5} current=0")
        st["t"] = t0 + req + 10
        u(tid, f"STW_RET case={case} n={n} code=0 t0={t0} t1={t0 + req + 10} bound={t0 + req} late_ns=10")

    sleep(W, "zero", 0, register=False)
    for _ in range(3):
        sleep(W, "short", 1000)
    sleep(W, "long", 3000, cpu=1)
    sleep(W, "abs", 500)
    sleep(W, "abs_past", 0, register=False)
    for _ in range(3):
        sleep(W, "idle", 100)
    for _ in range(16):
        sleep(W, "cycle", 10)
    u(W, "STW_SPINNER tid=10013")
    u(W, "STW_SPINNER tid=10014")
    for _ in range(8):
        sleep(W, "cross", 30)
    sleep(H, "two_h", 10)
    sleep(W, "two_w", 100)
    u(X, "STW_X_SLEEP req=7200000000000")
    L.append(f"STW_SLEEP_REGISTER tid={X} gen=1 deadline=99999999 now=1")
    L.append(f"STW_SLEEP_KILLED tid={X} gen=1")
    sleep(W, "kill_wait", 50)
    u(W, "STW_KILL_CHECK x_returned=0")
    u(W, "STW_RT_SET code=0")
    sleep(W, "rt_rel", 20)
    sleep(W, "rt_abs", 30)
    u(R, "STW_RECV_CAL ticks=4 elapsed=30000000 tick_est=10000000 outcome=none")
    L.append(f"U8_RECV_TIMEOUT_SETTLED arch=riscv64 tid={R} asid=3 wait_generation=1 deadline=4 now=4 broad_lock=0 result=ok")
    # an unrelated service's deferral: earns nothing
    L.append("U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid=2 wait_generation=7 cpu=0")
    L.append("U8_RECV_TIMEOUT_SETTLED arch=riscv64 tid=2 asid=1 wait_generation=7 deadline=1 now=2 broad_lock=0 result=ok")
    u(H, "STW_SATURATED fillers=130 first=128 refusals=2 full_at=1 w_deadline=9 early=1")
    u(R, "STW_RECV_CALL ticks=20 t0=5000 earliest=8000 due_by=9000 sat_ready=1")
    u(H, "STW_SAT_RECV r_deadline=8000 full_at=1 early=1")
    u(R, "STW_RECV_TOPUP filled=1")
    L.append(f"U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid={R} wait_generation=2 cpu=1")
    L.append(f"SATW_REFUSED class=recv cpu=1 home=1 tid={R} gen=2 n=1 prio=1 queued=64 reserved=0 cap=64 full=1 deadline=300 now=300 facts=ok")
    st["holdmark"] = len(L)
    sleep(W, "full", 2000, mid=(
        "STW_SLEEP_REFUSED cpu=0 home=0 tid={tid} gen={gen} deadline={d} now={d}",
        "SATW_REFUSED class=sleep cpu=0 home=0 tid={tid} gen={gen} n=1 prio=1 queued=63 reserved=1 cap=64 full=1 deadline={d} now={d} facts=ok",
        f"USER_LOG tid={H} msg=STW_HOLD_END end=99 w_deadline=9 r_due_by=9000 fillers=130 topups=3 refusals=5 holds=9 bounded=0"))
    L.append(f"U8_RECV_TIMEOUT_SETTLED arch=riscv64 tid={R} asid=3 wait_generation=2 deadline=300 now=900 broad_lock=0 result=ok")
    u(R, "STW_RECV_RET outcome=none t0=5000 t1=9500 due_by=9000")
    u(H, "STW_ROTATION fillers=130 parked=130 holds=5 waits=5")
    L.append("STW_KTHREAD_BEAT kthread=0x1 cpu=0 n=2 mono=2 yields=9")
    L.append("STW_KTHREAD_DONE kthread=0x1 cpu=0 beats=2")
    u(W, "STW_WAITER_DONE calls=33 fails=0")
    u(1, f"STW_DONE waiter={W} helper={H} victim={X} calls=33 fails=0 result=ok")
    return L


def self_test():
    good = synth()
    f, _ = grade(good)
    assert not f, f

    def must_fail(lines, needle):
        f, _ = grade(lines)
        assert any(needle in x for x in f), (needle, f)

    def edit(old, new, nth=0):
        L = list(good)
        idx = [i for i, l in enumerate(L) if old in l][nth]
        L[idx] = L[idx].replace(old, new)
        return L

    # early return
    must_fail(edit("t1=1010 bound=1000", "t1=999 bound=1000"), "returned EARLY")
    # double completion
    i = next(i for i, l in enumerate(good) if l.startswith("STW_SLEEP_DONE"))
    must_fail(good[:i + 1] + [good[i]] + good[i + 1:], "completions of gen")
    # lost completion
    must_fail(good[:i] + good[i + 1:], "completions of gen")
    # resurrection of the dead victim
    k = next(i for i, l in enumerate(good) if l.startswith("STW_SLEEP_KILLED"))
    must_fail(good[:k + 1] + ["STW_SLEEP_DONE cpu=0 home=0 tid=10012 gen=1 deadline=9 now=9 current=0"] + good[k + 1:],
              "resurrection")
    # stale generation reused
    must_fail(edit("STW_SLEEP_REGISTER tid=10010 gen=3", "STW_SLEEP_REGISTER tid=10010 gen=2"), "not fresh")
    # never refused on the full queue
    must_fail([l for l in good if not l.startswith("STW_SLEEP_REFUSED")], "never refused")
    # no cross-CPU placement
    must_fail(edit("cpu=1 home=0", "cpu=0 home=0"), "another CPU")
    # wrong error lane
    must_fail(edit("case=bad_clock code=2", "case=bad_clock code=0"), "bad_clock returned")
    # no IPC deferral
    must_fail([l for l in good if not l.startswith("U8_RECV_TIMEOUT_PLACEMENT_DEFERRED")], "IPC")
    # a damaged marker
    must_fail(edit("STW_SLEEP_DONE cpu=0", "STW_SLEEP_DONE cpu=x"), "malformed")
    # rotation incomplete
    must_fail(edit("parked=130", "parked=129"), "fillers ran")
    # SCHED-SAT-EVIDENCE1 — W's home queue
    no_sleep_sat = [l for l in good if not l.startswith("SATW_REFUSED class=sleep")]
    must_fail(no_sleep_sat, "no refusal proves W's home queue")
    must_fail(edit("queued=63 reserved=1 cap=64 full=1", "queued=62 reserved=1 cap=64 full=0"), "room left")
    must_fail(edit("queued=63 reserved=1 cap=64 full=1", "queued=62 reserved=1 cap=64 full=1"), "inconsistent facts")
    must_fail(edit("class=sleep cpu=0 home=0", "class=sleep cpu=0 home=1"), "home 1 != 0")
    i = next(i for i, l in enumerate(good) if l.startswith("SATW_REFUSED class=sleep"))
    lost = good[:i] + [good[i].replace("facts=ok", "facts=lost")] + good[i + 1:]
    must_fail(lost, "facts lost")
    h = next(i for i, l in enumerate(good) if "STW_HOLD_END" in l)
    after_hold = good[:i] + [good[h], good[i]] + [l for k, l in enumerate(good) if k > i and k != h]
    must_fail(after_hold, "after the hold ended")
    must_fail(edit("bounded=0", "bounded=1"), "hold hit its bound")
    # R's receive
    must_fail([l for l in good if not l.startswith("SATW_REFUSED class=recv")], "no refusal proves R's home queue")
    must_fail(edit("queued=64 reserved=0 cap=64 full=1", "queued=63 reserved=0 cap=64 full=0"), "IPC: no refusal")
    must_fail([l for l in good if not l.startswith("U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid=10015")], "PLACEMENT_DEFERRED of R")
    st_r = next(i for i, l in enumerate(good) if l.startswith("U8_RECV_TIMEOUT_SETTLED") and "tid=10015" in l and "wait_generation=2" in l)
    must_fail(good[:st_r + 1] + [good[st_r]] + good[st_r + 1:], "2 settlements")
    must_fail(good[:st_r] + good[st_r + 1:], "0 settlements")
    must_fail(edit("STW_RECV_RET outcome=none", "STW_RECV_RET outcome=msg"), "not its timeout")
    must_fail(edit("full_at=1 early=1", "full_at=9999 early=0"), "not full before R's receive")
    must_fail(edit("sat_ready=1", "sat_ready=0"), "before H reported saturation")
    must_fail(edit("deadline=300 now=300 facts=ok", "deadline=300 now=299 facts=ok"), "IPC: no refusal")
    # only an unrelated thread's deferral: earns nothing
    only_other = [l for l in good if not (l.startswith("SATW_REFUSED class=recv")
                                          or l.startswith("U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid=10015"))]
    must_fail(only_other, "IPC: no refusal")
    must_fail([l for l in good if "STW_RECV_CALL" not in l], "did not run")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch", choices=("x86_64", "aarch64", "riscv64"))
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--core-rc", type=int)
    ap.add_argument("--core-log", action="append", default=[])
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        print("SCHED_TIME_WITNESS_GRADER_SELF_TEST result=ok")
        return 0
    if not a.arch or not a.log:
        ap.error("--arch and a log are required")
    with open(a.log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    fails, facts = grade(lines)
    core_ok, reasons = core_verdict(a.core_rc, a.core_log)
    if not core_ok:
        fails.append(f"core smoke not clean (rc={a.core_rc}): {reasons[:5]}")
    if a.smp < 2:
        fails.append("cross-CPU placement needs SMP >= 2")
    for f in fails:
        print(f"[sched-time-witness][fail] {f}")
    fact_s = " ".join(f"{k}={v}" for k, v in facts.items())
    print(f"SCHED_TIME_WITNESS_SEAL arch={a.arch} smp={a.smp} result={'ok' if not fails else 'fail'} {fact_s}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
