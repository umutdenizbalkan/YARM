#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-TIME1 — grade one live timed-FutexWait witness boot.

Input: the normalized serial log of a boot built with `futex-time-witness` (kernel and init) and
booted with `yarm.futex_requeue_witness=1`. Init's waiter thread W is the only thread that issues
timed waits; it logs `FTW_CALL role=.. n=.. ns=.. addr=.. expected=..` before each and
`FTW_RET role=.. n=.. result=..` after it (the user runtime's own decode of the returned registers).
The kernel, from inside the deciding transactions, logs:

  FUTEX_TIME_W_DEADLINE  ns period_ns ticks now deadline   NR 9's one deadline computation
  FUTEX_TIME_W_REGISTER  tid gen deadline now va           the compare-and-register hold
  FUTEX_TIME_W_WAKE      tid gen va                        a wake's claim of the timed cycle
  FUTEX_TIME_W_TIMEOUT   cpu tid gen now current va        the collector's timeout claim
  FUTEX_TIME_W_LOST      cpu tid gen                       the collector found it due, lost it
  FUTEX_TIME_W_WINDOW_OPEN / _CLOSE / _SKIPPED, FUTEX_TIME_W_IN_WINDOW_WAKE   (sequence G)
  FUTEX_TIMEOUT_EXPIRED_UNPLACED tid slot generation cpu home now   (the timeout won, queue full)

Markers fail closed: a marker whose prefix or fields are damaged fails the boot; nothing is
skipped and nothing is repaired.

Every call (W's, in order): exactly one DEADLINE and one REGISTER of W between its CALL and RET;
ns equals the call's, period_ns is this ISA's tick period, ticks = 0 if ns == 0 else
ceil(ns / period) + 1, deadline = now + ticks; REGISTER names the same deadline, the call's word
(va = addr) and a fresh generation (strictly increasing). The cycle has EXACTLY ONE terminal
marker in the whole log, a WAKE or a TIMEOUT of its (tid, gen), between REGISTER and RET. A LOST
(a collector that found the cycle due after another owner — a wake, or the other CPU's collector —
had claimed it) is legal beside either terminal, between REGISTER and RET; it never ends a cycle. RET woken <=> WAKE; RET timed_out <=> TIMEOUT (and its now >= deadline).
Any other result fails, and so does any FTW_FAIL.

Sequences:
  A/F  three A calls end timed_out; during each, no user thread of the cell but W logs anything
       (H and S2 are parked); the clock advanced at least `ticks` between the DEADLINE and the
       TIMEOUT; at least one A timeout was collected on an idle CPU (current=0).
  B    woken (FTW_B_WAKE woke=1); B2 timed_out at now >= B's deadline (B never completed again).
  C    FTW_C_REQUEUE moved=1; C timed_out on the TARGET word (TIMEOUT va = target != addr);
       FTW_C_AFTER wake_source=0 wake_target=0.
  D    FTW_D_REQUEUE_WAKE moved=1 woke=1; D woken on the target (WAKE va = target); D2 timed_out at
       now >= D's deadline.
  G    every G call woken or timed_out per its terminal marker. Every G TIMEOUT is followed by
       exactly one WINDOW_OPEN or WINDOW_SKIPPED of its gen (a non-G timeout has neither). Per
       window: one OPEN, one CLOSE (same cpu, window, gen) after it, every IN_WINDOW_WAKE of it
       between them, each woke=0 (the timeout had already claimed the cycle); end=woke needs a
       wake from ANOTHER CPU; end=bound fails; end=no_peer is a refusal (no coverage). G's RET
       comes after its CLOSE. Coverage: >= 1 window ended woke, >= 1 G cycle won by a wake, >= 1
       won by the timeout.
  E    FTW_E_SATURATED fillers >= 64 spawn_refused=1, matching the kernel: one SPAWN_THREAD_SPLIT_OK
       per filler and exactly one SPAWN_THREAD_SPLIT_FAIL err=SchedulerFull; a FUTEX_TIMEOUT_EXPIRED_UNPLACED of W between
       E's REGISTER and its final TIMEOUT (now >= deadline); FTW_E_EXPIRED_SEEN gone=1 wake1=0
       wake2=0 (no wake found it after the timeout won); E timed_out.
  FTW_WAITER_DONE fails=0, FTW_DONE result=ok, plus the standard core smoke.

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

PERIOD_NS = {"x86_64": 800_000_000, "aarch64": 50_000_000, "riscv64": 10_000_000}
FATAL = (
    "YARM_AARCH64_TRAP_HANDLE halting",
    "RISCV_TRAP_HANDLE_FAILED",
    "x86 shared trap dispatch failed",
    "FUTEX_WAKE_RESERVED_COMMIT_REFUSED",
    "FUTEX_WAIT_BROAD_ENTRY",
    "panicked at",
    "KERNEL PANIC",
)
ROLES_EXPECT = {"A": "timed_out", "B": "woken", "B2": "timed_out", "C": "timed_out",
                "D": "woken", "D2": "timed_out", "E": "timed_out"}
CALLS_EXPECT = {"A": 3, "B": 1, "B2": 1, "C": 1, "D": 1, "D2": 1, "G": 32, "E": 1}
MIN_FILLERS = 64

INT = r"\d+"
HEX = r"0x[0-9a-f]+"
K = {
    "DEADLINE": ("FUTEX_TIME_W_DEADLINE",
                 {"ns": INT, "period_ns": INT, "ticks": INT, "now": INT, "deadline": INT}),
    "REGISTER": ("FUTEX_TIME_W_REGISTER",
                 {"tid": INT, "gen": INT, "deadline": INT, "now": INT, "va": HEX}),
    "WAKE": ("FUTEX_TIME_W_WAKE", {"tid": INT, "gen": INT, "va": HEX}),
    "TIMEOUT": ("FUTEX_TIME_W_TIMEOUT",
                {"cpu": INT, "tid": INT, "gen": INT, "now": INT, "current": INT, "va": HEX}),
    "LOST": ("FUTEX_TIME_W_LOST", {"cpu": INT, "tid": INT, "gen": INT}),
    "OPEN": ("FUTEX_TIME_W_WINDOW_OPEN", {"cpu": INT, "window": INT, "tid": INT, "gen": INT}),
    "CLOSE": ("FUTEX_TIME_W_WINDOW_CLOSE",
              {"cpu": INT, "window": INT, "gen": INT, "competing_wakes": INT,
               "end": "woke|bound|no_peer"}),
    "SKIPPED": ("FUTEX_TIME_W_WINDOW_SKIPPED",
                {"cpu": INT, "tid": INT, "gen": INT, "reason": r"[a-z_]+"}),
    "INWAKE": ("FUTEX_TIME_W_IN_WINDOW_WAKE",
               {"cpu": INT, "tid": INT, "window": INT, "woke": INT}),
    "SPAWN_OK": ("SPAWN_THREAD_SPLIT_OK",
                 {"parent_tid": INT, "tid": INT, "entry": HEX, "stack": HEX, "tls": HEX, "result": "ok"}),
    "SPAWN_FAIL": ("SPAWN_THREAD_SPLIT_FAIL",
                   {"parent_tid": INT, "tid": INT, "err": r"[A-Za-z]+", "result": "compensated"}),
    "EXPIRED": ("FUTEX_TIMEOUT_EXPIRED_UNPLACED",
                {"tid": INT, "slot": INT, "generation": INT, "cpu": INT, "home": INT, "now": INT}),
}
U = {
    "CALL": {"role": r"[A-Z][0-9]?", "n": INT, "ns": INT, "addr": HEX, "expected": HEX},
    "RET": {"role": r"[A-Z][0-9]?", "n": INT, "result": r"[a-z_]+"},
    "THREADS": {"waiter": INT, "helper": INT, "signaller": INT},
    "B_WAKE": {"woke": INT, "tries": INT},
    "C_REQUEUE": {"moved": INT, "target": HEX},
    "C_AFTER": {"wake_source": INT, "wake_target": INT},
    "D_REQUEUE_WAKE": {"moved": INT, "woke": INT, "target": HEX},
    "G_SUMMARY": {"cycles": INT, "woken": INT, "timed_out": INT},
    "G_SIGNALLER": {"who": r"H|S2", "attempts": INT, "woke": INT},
    "E_SATURATED": {"fillers": INT, "spawn_refused": INT},
    "E_EXPIRED_SEEN": {"gone": INT, "pings": INT, "wake1": INT, "wake2": INT},
    "WAITER_DONE": {"calls": INT, "fails": INT},
    "DONE": {"waiter": INT, "calls": INT, "fails": INT, "result": "ok|fail"},
    "BEGIN": {"s_tid": INT, "g_cycles": INT, "fillers_max": INT},
    "FAIL": {"what": r"\S+"},
}
FRAGMENTS = ("FUTEX_TIME", "UTEX_TIME_W", "TIME_W_", "_W_WINDOW", "IN_WINDOW_WAKE", "EXPIRED_UNPLACED", "msg=FTW_",
             "FTW_CALL", "FTW_RET", "FTW_DONE")


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s,]+)", line))


def parse(line):
    """(kind, fields); (None, None) for a non-marker; ('DAMAGED', why) for a damaged marker."""
    for kind, (head, fields) in K.items():
        if re.match(rf"^{head} ", line):
            d = kv(line)
            for k, pat in fields.items():
                if k not in d or not re.fullmatch(pat, d[k]):
                    return "DAMAGED", f"{head}: field `{k}` missing or malformed"
            return kind, d
    m = re.match(r"^USER_LOG tid=(\d+) msg=FTW_([A-Z0-9_]+?) (.*)$", line)
    if m:
        name = m.group(2)
        if name not in U:
            return "DAMAGED", f"FTW_{name}: unknown cell line"
        d = kv(m.group(3))
        for k, pat in U[name].items():
            if k not in d or not re.fullmatch(pat, d[k]):
                if not (name == "RET" and k == "result" and "err" in d):
                    return "DAMAGED", f"FTW_{name}: field `{k}` missing or malformed"
        d["utid"] = m.group(1)
        return "U_" + name, d
    if any(f in line for f in FRAGMENTS):
        return "DAMAGED", "damaged witness marker"
    return None, None


def ticks_for(ns, period):
    return 0 if ns == 0 else -(-ns // period) + 1


def grade(lines, arch, smp):
    fails = []
    fail = fails.append
    period = PERIOD_NS[arch]
    ev = []
    for i, l in enumerate(lines):
        for f in FATAL:
            if f in l:
                fail(f"fatal marker: {l.strip()[:160]}")
        kind, d = parse(l.strip())
        if kind is None:
            continue
        if kind == "DAMAGED":
            fail(f"line {i + 1}: {d}: {l.strip()[:140]}")
            continue
        d["at"] = i
        d["kind"] = kind
        ev.append(d)

    def only(kind):
        return [e for e in ev if e["kind"] == kind]

    def one(kind):
        xs = only(kind)
        if len(xs) != 1:
            fail(f"expected exactly one {kind}, saw {len(xs)}")
            return {}
        return xs[0]

    for e in only("U_FAIL"):
        fail(f"cell reported FTW_FAIL what={e['what']}")
    threads = one("U_THREADS")
    W, H, S2 = threads.get("waiter"), threads.get("helper"), threads.get("signaller")
    done = one("U_DONE")
    if done and done.get("result") != "ok":
        fail(f"FTW_DONE result={done.get('result')}")
    wdone = one("U_WAITER_DONE")
    if wdone and wdone.get("fails") != "0":
        fail(f"FTW_WAITER_DONE fails={wdone.get('fails')}")

    # ── the calls: pair CALL n / RET n, both from W, in order ─────────────────────────────
    calls = only("U_CALL")
    rets = {e["n"]: e for e in only("U_RET")}
    if len(rets) != len(only("U_RET")):
        fail("a call returned twice")
    nums = [int(c["n"]) for c in calls]
    if nums != list(range(1, len(calls) + 1)):
        fail(f"calls not numbered 1..N in order: {nums}")
    if wdone and int(wdone.get("calls", -1)) != len(calls):
        fail(f"FTW_WAITER_DONE calls={wdone.get('calls')} but {len(calls)} calls logged")
    roles = {}
    for c in calls:
        roles[c["role"]] = roles.get(c["role"], 0) + 1
    if roles != CALLS_EXPECT:
        fail(f"calls per role {roles}, expected {CALLS_EXPECT}")

    terminals = {}  # (tid, gen) -> [events]
    for e in ev:
        if e["kind"] in ("WAKE", "TIMEOUT"):
            terminals.setdefault((e["tid"], e["gen"]), []).append(e)
    lost = {}
    for e in only("LOST"):
        lost.setdefault((e["tid"], e["gen"]), []).append(e)
    cycles = []
    last_gen = -1
    for c in calls:
        n = c["n"]
        r = rets.get(n)
        if c["utid"] != W:
            fail(f"call {n}: logged by tid {c['utid']}, the waiter is {W}")
        if r is None:
            fail(f"call {n} ({c['role']}): never returned")
            continue
        if r["role"] != c["role"] or r["utid"] != W or not c["at"] < r["at"]:
            fail(f"call {n}: RET does not match its CALL")
            continue
        span = [e for e in ev if c["at"] < e["at"] < r["at"]]
        dl = [e for e in span if e["kind"] == "DEADLINE"]
        rg = [e for e in span if e["kind"] == "REGISTER" and e["tid"] == W]
        if len(dl) != 1 or len(rg) != 1:
            fail(f"call {n} ({c['role']}): {len(dl)} DEADLINE / {len(rg)} REGISTER markers (need 1/1)")
            continue
        dl, rg = dl[0], rg[0]
        ns = int(c["ns"])
        if int(dl["ns"]) != ns:
            fail(f"call {n}: kernel saw ns={dl['ns']}, the call passed {ns}")
        if int(dl["period_ns"]) != period:
            fail(f"call {n}: tick period {dl['period_ns']} ns, {arch} ticks every {period} ns")
        if int(dl["ticks"]) != ticks_for(ns, period):
            fail(f"call {n}: ticks={dl['ticks']} for ns={ns}, expected {ticks_for(ns, period)}")
        if int(dl["deadline"]) != int(dl["now"]) + int(dl["ticks"]):
            fail(f"call {n}: deadline {dl['deadline']} != now {dl['now']} + ticks {dl['ticks']}")
        if rg["deadline"] != dl["deadline"]:
            fail(f"call {n}: REGISTER deadline {rg['deadline']} != computed {dl['deadline']}")
        if rg["va"] != c["addr"]:
            fail(f"call {n}: registered on {rg['va']}, called on {c['addr']}")
        if not dl["at"] < rg["at"]:
            fail(f"call {n}: REGISTER before the DEADLINE computation")
        gen = int(rg["gen"])
        if gen <= last_gen:
            fail(f"call {n}: generation {gen} not fresh (previous {last_gen})")
        last_gen = gen
        t = terminals.get((W, rg["gen"]), [])
        if len(t) != 1:
            fail(f"call {n} ({c['role']}) gen {gen}: {len(t)} terminal markers (need exactly 1): "
                 f"{[x['kind'] for x in t]}")
            continue
        t = t[0]
        if not rg["at"] < t["at"] < r["at"]:
            fail(f"call {n}: terminal {t['kind']} outside REGISTER..RET")
        want = {"WAKE": "woken", "TIMEOUT": "timed_out"}[t["kind"]]
        if r.get("result") != want:
            fail(f"call {n} ({c['role']}): kernel {t['kind']} but ring 3 saw "
                 f"{r.get('result', 'error')}{' ' + r['err'] if 'err' in r else ''}")
        if t["kind"] == "TIMEOUT":
            if int(t["now"]) < int(rg["deadline"]):
                fail(f"call {n}: timed out at {t['now']} before its deadline {rg['deadline']}")
        for x in lost.get((W, rg["gen"]), []):
            if not rg["at"] < x["at"] < r["at"]:
                fail(f"call {n}: a LOST of its cycle outside REGISTER..RET")
        exp = ROLES_EXPECT.get(c["role"])
        if exp and r.get("result") != exp:
            fail(f"call {n} ({c['role']}): result {r.get('result')}, sequence requires {exp}")
        cycles.append(dict(n=n, role=c["role"], call=c, ret=r, dl=dl, rg=rg, term=t, gen=rg["gen"]))
    by_role = {}
    for cy in cycles:
        by_role.setdefault(cy["role"], []).append(cy)
    facts = {}

    # ── A/F: no polling thread; the clock advanced while everything waited; an idle collector ──
    a_idle = 0
    for cy in by_role.get("A", []):
        span = [e for e in ev if cy["call"]["at"] < e["at"] < cy["ret"]["at"]]
        noisy = [e for e in span if e["kind"].startswith("U_") and e.get("utid") in (H, S2)]
        if noisy:
            fail(f"A call {cy['n']}: a helper ran during the wait: {noisy[0]['kind']}")
        t = cy["term"]
        if t["kind"] == "TIMEOUT":
            if int(t["now"]) - int(cy["dl"]["now"]) < int(cy["dl"]["ticks"]):
                fail(f"A call {cy['n']}: clock advanced {int(t['now']) - int(cy['dl']['now'])} < "
                     f"{cy['dl']['ticks']} ticks")
            if t["current"] == "0":
                a_idle += 1
    if a_idle < 1:
        fail("F: no A timeout was collected on an idle CPU (current=0)")
    facts["a_idle_collections"] = a_idle

    def after_deadline(later, earlier, name):
        if later and earlier and later[0]["term"]["kind"] == "TIMEOUT":
            if int(later[0]["term"]["now"]) < int(earlier[0]["rg"]["deadline"]):
                fail(f"{name}: expired at {later[0]['term']['now']}, before the earlier cycle's "
                     f"deadline {earlier[0]['rg']['deadline']} passed")

    # ── B ──
    bw = one("U_B_WAKE")
    if bw and bw.get("woke") != "1":
        fail(f"B: helper's wake woke={bw.get('woke')}")
    after_deadline(by_role.get("B2"), by_role.get("B"), "B2")
    # ── C ──
    cr = one("U_C_REQUEUE")
    if cr and cr.get("moved") != "1":
        fail("C: the requeue moved nobody")
    for cy in by_role.get("C", []):
        if cr and cy["term"]["va"] != cr.get("target"):
            fail(f"C: timed out on {cy['term']['va']}, not the requeue target {cr.get('target')}")
        if cy["term"]["va"] == cy["call"]["addr"]:
            fail("C: timed out on the word it was requeued away from")
    ca = one("U_C_AFTER")
    if ca and (ca.get("wake_source") != "0" or ca.get("wake_target") != "0"):
        fail(f"C: wakes after the timeout found a waiter: {ca}")
    # ── D ──
    dr = one("U_D_REQUEUE_WAKE")
    if dr and (dr.get("moved") != "1" or dr.get("woke") != "1"):
        fail(f"D: requeue/wake moved={dr.get('moved')} woke={dr.get('woke')}")
    for cy in by_role.get("D", []):
        if dr and cy["term"]["va"] != dr.get("target"):
            fail(f"D: woken on {cy['term']['va']}, not the requeue target {dr.get('target')}")
    after_deadline(by_role.get("D2"), by_role.get("D"), "D2")

    # ── G: windows, causally ordered ──
    gens_g = {cy["gen"]: cy for cy in by_role.get("G", [])}
    opens = {}
    for o in only("OPEN"):
        if o["window"] in opens:
            fail(f"window {o['window']}: opened twice")
        opens[o["window"]] = o
    closes = {}
    for c in only("CLOSE"):
        if c["window"] in closes:
            fail(f"window {c['window']}: closed twice")
        closes[c["window"]] = c
    inw = only("INWAKE")
    for c in closes:
        if c not in opens:
            fail(f"window {c}: CLOSE without OPEN")
    for w in inw:
        if w["window"] not in opens:
            fail(f"window {w['window']}: in-window wake without OPEN")
    decided = {}
    for e in only("OPEN") + only("SKIPPED"):
        if e["gen"] in decided:
            fail(f"gen {e['gen']}: two window decisions")
        decided[e["gen"]] = e
    woke_windows = refused = 0
    for g, cy in gens_g.items():
        t = cy["term"]
        dec = decided.get(g)
        if t["kind"] == "TIMEOUT":
            if dec is None:
                fail(f"G call {cy['n']}: a G timeout with no window decision")
                continue
            if not t["at"] < dec["at"] < cy["ret"]["at"]:
                fail(f"G call {cy['n']}: window decision outside TIMEOUT..RET")
            if dec["kind"] == "OPEN":
                c = closes.get(dec["window"])
                if c is None:
                    fail(f"window {dec['window']}: opened and never closed")
                    continue
                if (c["cpu"], c["gen"]) != (dec["cpu"], dec["gen"]) or dec["cpu"] != t["cpu"]:
                    fail(f"window {dec['window']}: OPEN/CLOSE/TIMEOUT disagree on cpu or gen")
                if not dec["at"] < c["at"] < cy["ret"]["at"]:
                    fail(f"window {dec['window']}: CLOSE out of order")
                mine = [w for w in inw if w["window"] == dec["window"]]
                if any(not dec["at"] < w["at"] < c["at"] for w in mine):
                    fail(f"window {dec['window']}: an in-window wake logged outside OPEN..CLOSE")
                if any(w["woke"] != "0" for w in mine):
                    fail(f"window {dec['window']}: a wake took a cycle the timeout had claimed")
                if c["end"] == "bound":
                    fail(f"window {dec['window']}: ran out its bound with a peer running")
                elif c["end"] == "no_peer":
                    refused += 1
                    if mine:
                        fail(f"window {dec['window']}: no_peer but a wake ran inside")
                else:
                    foreign = [w for w in mine if w["cpu"] != dec["cpu"]]
                    if not foreign or int(c["competing_wakes"]) < 1:
                        fail(f"window {dec['window']}: end=woke without a wake from another CPU")
                    else:
                        woke_windows += 1
        elif dec is not None:
            fail(f"G call {cy['n']}: woken, yet a window decision names its gen")
    for g in decided:
        if g not in gens_g:
            fail(f"gen {g}: a window decision for a non-G cycle")
    g_woken = sum(1 for cy in gens_g.values() if cy["term"]["kind"] == "WAKE")
    g_timed = sum(1 for cy in gens_g.values() if cy["term"]["kind"] == "TIMEOUT")
    if woke_windows < 1:
        fail("G coverage: no window held a wake from another CPU")
    if g_woken < 1 or g_timed < 1:
        fail(f"G coverage: woken={g_woken} timed_out={g_timed} (need both >= 1)")
    gs = one("U_G_SUMMARY")
    if gs and (int(gs.get("woken", -1)), int(gs.get("timed_out", -1))) != (g_woken, g_timed):
        fail(f"G: the cell counted {gs.get('woken')}/{gs.get('timed_out')}, the kernel {g_woken}/{g_timed}")
    facts.update(g_woken=g_woken, g_timed_out=g_timed, g_windows_woke=woke_windows,
                 g_windows_refused=refused, g_skipped=len(only("SKIPPED")),
                 in_window_wakes=len(inw))

    # ── E: genuine saturation ──
    es = one("U_E_SATURATED")
    if es and (int(es.get("fillers", 0)) < MIN_FILLERS or es.get("spawn_refused") != "1"):
        fail(f"E: fillers={es.get('fillers')} spawn_refused={es.get('spawn_refused')} — not saturated")
    h_ok = [e for e in only("SPAWN_OK") if e["parent_tid"] == H]
    h_fail = [e for e in only("SPAWN_FAIL") if e["parent_tid"] == H]
    if es and len(h_ok) != int(es.get("fillers", -1)):
        fail(f"E: the cell counted {es.get('fillers')} fillers, the kernel spawned {len(h_ok)} for H")
    if len(h_fail) != 1 or h_fail[0]["err"] != "SchedulerFull":
        fail(f"E: spawning must stop at exactly one SchedulerFull refusal (kernel: {[e['err'] for e in h_fail]})")
    facts["e_spawn_refusal"] = h_fail[0]["err"] if h_fail else "none"
    ee = one("U_E_EXPIRED_SEEN")
    if ee and (ee.get("gone") != "1" or ee.get("wake1") != "0" or ee.get("wake2") != "0"):
        fail(f"E: {ee}")
    for cy in by_role.get("E", []):
        ex = [e for e in only("EXPIRED") if e["tid"] == W and cy["rg"]["at"] < e["at"]]
        if not ex:
            fail("E: no FUTEX_TIMEOUT_EXPIRED_UNPLACED for the waiter — the queue was not full")
            continue
        if len(ex) != 1:
            fail(f"E: {len(ex)} expiries of one cycle")
        x = ex[0]
        if not x["at"] < cy["term"]["at"]:
            fail("E: the timeout was placed before it was recorded as expired-unplaced")
        if int(x["now"]) < int(cy["rg"]["deadline"]):
            fail("E: expired before its deadline")
        facts.update(e_fillers=es.get("fillers"), e_pings=ee.get("pings"))
    for x in only("EXPIRED"):
        if x["tid"] != W or not by_role.get("E") or x["at"] < by_role["E"][0]["rg"]["at"]:
            fail(f"an expired-unplaced timeout outside sequence E: tid {x['tid']}")
    facts.update(calls=len(calls), cycles=len(cycles))
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


# ── self-test: a synthetic good boot, then mutations that must each fail ─────────────────────
def synth(arch="riscv64"):
    period = PERIOD_NS[arch]
    W, H, S2 = "20", "21", "22"
    out = [f"USER_LOG tid=1 msg=FTW_BEGIN s_tid=1 g_cycles=32 fillers_max=144",
           f"USER_LOG tid=1 msg=FTW_THREADS waiter={W} helper={H} signaller={S2}"]
    st = {"n": 0, "gen": 0, "now": 100}
    QA, QB = "0x5000", "0x5004"

    def call(role, ns, addr, end, va=None, extra_mid=(), extra_end=(), current="0"):
        st["n"] += 1
        st["gen"] += 1
        n, g = st["n"], st["gen"]
        ticks = ticks_for(ns, period)
        now = st["now"]
        dl = now + ticks
        lines = [f"USER_LOG tid={W} msg=FTW_CALL role={role} n={n} ns={ns} addr={addr} expected=0x1",
                 f"FUTEX_TIME_W_DEADLINE ns={ns} period_ns={period} ticks={ticks} now={now} deadline={dl}",
                 f"FUTEX_TIME_W_REGISTER tid={W} gen={g} deadline={dl} now={now} va={addr}"]
        lines += [x.format(gen=g, dl=dl) for x in extra_mid]
        va = va or addr
        if end == "woken":
            lines.append(f"FUTEX_TIME_W_WAKE tid={W} gen={g} va={va}")
            st["now"] = now + 1
        else:
            st["now"] = dl + 1
            lines.append(f"FUTEX_TIME_W_TIMEOUT cpu=0 tid={W} gen={g} now={dl} current={current} va={va}")
        lines += [x.format(gen=g, dl=dl) for x in extra_end]
        lines.append(f"USER_LOG tid={W} msg=FTW_RET role={role} n={n} result={end}")
        return lines

    for _ in range(3):
        out += call("A", 100_000_000, "0x4000", "timed_out")
    out += call("B", 2_000_000_000, "0x4010", "woken")
    out.append(f"USER_LOG tid={H} msg=FTW_B_WAKE woke=1 tries=3")
    out += call("B2", 4_000_000_000, "0x4014", "timed_out", current="21")
    out += call("C", 1_000_000_000, QA, "timed_out", va=QB, current="21")
    out.append(f"USER_LOG tid={H} msg=FTW_C_REQUEUE moved=1 target={QB}")
    out.append(f"USER_LOG tid={W} msg=FTW_C_AFTER wake_source=0 wake_target=0")
    out += call("D", 2_000_000_000, QA, "woken", va=QB)
    out.append(f"USER_LOG tid={H} msg=FTW_D_REQUEUE_WAKE moved=1 woke=1 target={QB}")
    out += call("D2", 4_000_000_000, "0x4020", "timed_out", current="21")
    win = 0
    for k in range(32):
        if k % 2 == 0:
            out += call("G", 1, "0x4030", "woken")
        else:
            win += 1
            if k % 4 == 1:
                tail = (f"FUTEX_TIME_W_WINDOW_OPEN cpu=0 window={win} tid={W} gen={{gen}}",
                        f"FUTEX_TIME_W_IN_WINDOW_WAKE cpu=1 tid={S2} window={win} woke=0",
                        f"FUTEX_TIME_W_WINDOW_CLOSE cpu=0 window={win} gen={{gen}} competing_wakes=1 end=woke")
            else:
                tail = (f"FUTEX_TIME_W_WINDOW_SKIPPED cpu=0 tid={W} gen={{gen}} reason=no_peer_running",)
            out += call("G", 1, "0x4030", "timed_out", extra_end=tail, current="22")
    out.append(f"USER_LOG tid={W} msg=FTW_G_SUMMARY cycles=32 woken=16 timed_out=16")
    out += call("E", 6_000_000_000, "0x4040", "timed_out", current="21",
                extra_mid=tuple(f"SPAWN_THREAD_SPLIT_OK parent_tid={H} tid={30000 + i} entry=0x1 stack=0x10 tls=0x20 result=ok"
                                for i in range(128))
                + (f"SPAWN_THREAD_SPLIT_FAIL parent_tid={H} tid=30200 err=SchedulerFull tasks=0 result=compensated",
                   f"USER_LOG tid={H} msg=FTW_E_SATURATED fillers=128 spawn_refused=1",
                           f"FUTEX_TIMEOUT_EXPIRED_UNPLACED tid={W} slot=3 generation=9 cpu=1 home=0 now={{dl}}",
                           f"USER_LOG tid={H} msg=FTW_E_EXPIRED_SEEN gone=1 pings=40 wake1=0 wake2=0"))
    n = st["n"]
    out += [f"USER_LOG tid={W} msg=FTW_WAITER_DONE calls={n} fails=0",
            f"USER_LOG tid=1 msg=FTW_DONE waiter={W} calls={n} fails=0 result=ok"]
    return out


def self_test():
    good = synth()
    f, facts = grade(good, "riscv64", 2)
    assert not f, f
    for arch in PERIOD_NS:
        f, _ = grade(synth(arch), arch, 2)
        assert not f, (arch, f)

    def must_fail(lines, why, needle):
        f, _ = grade(lines, "riscv64", 2)
        assert f, f"must fail: {why}"
        assert any(needle in x for x in f), (why, needle, f)

    def edit(old, new, nth=0):
        hits = [i for i, l in enumerate(good) if old in l]
        assert len(hits) > nth, old
        out = list(good)
        out[hits[nth]] = out[hits[nth]].replace(old, new)
        return out

    def drop(needle, nth=0):
        hits = [i for i, l in enumerate(good) if needle in l]
        assert len(hits) > nth, needle
        return good[:hits[nth]] + good[hits[nth] + 1:]

    def insert_after(needle, line, nth=0):
        hits = [i for i, l in enumerate(good) if needle in l]
        assert len(hits) > nth, needle
        return good[:hits[nth] + 1] + [line] + good[hits[nth] + 1:]

    # integrity
    must_fail(edit("FUTEX_TIME_W_TIMEOUT cpu=0", "FUTEX_TIME_W_TIMEOUT cpu=x"), "damaged field", "malformed")
    must_fail(edit("FUTEX_TIME_W_REGISTER", "FUTEX_TIME_W_REG1STER"), "damaged prefix", "damaged")
    must_fail(edit("FUTEX_TIME_W_REGISTER", "FUTEX_TIMEX_W_REGISTER"), "damaged prefix 2", "damaged")
    must_fail(good + ["USER_LOG tid=20 msg=FTW_FAIL what=A"], "cell failure", "FTW_FAIL")
    must_fail([l for l in good if "FTW_DONE" not in l], "no DONE", "exactly one U_DONE")
    must_fail(good + ["KERNEL PANIC at foo"], "fatal", "fatal marker")
    # per-call terminal ownership
    must_fail(drop("FUTEX_TIME_W_TIMEOUT"), "missing terminal", "terminal markers")
    dup = insert_after("FUTEX_TIME_W_TIMEOUT", "FUTEX_TIME_W_WAKE tid=20 gen=1 va=0x4000")
    must_fail(dup, "double terminal", "terminal markers")
    must_fail(edit("msg=FTW_RET role=B n=4 result=woken", "msg=FTW_RET role=B n=4 result=timed_out"),
              "woken reported as timeout", "ring 3 saw")
    must_fail(edit("msg=FTW_RET role=A n=1 result=timed_out", "msg=FTW_RET role=A n=1 result=woken"),
              "timeout reported as woken (the old 1)", "ring 3 saw")
    f, _ = grade(insert_after("FUTEX_TIME_W_TIMEOUT", "FUTEX_TIME_W_LOST cpu=1 tid=20 gen=1"), "riscv64", 2)
    assert not f, ("a second collector losing the race is legal", f)
    must_fail(insert_after("msg=FTW_BEGIN", "FUTEX_TIME_W_LOST cpu=1 tid=20 gen=1"),
              "lost before its cycle registered", "outside REGISTER..RET")
    # units and clock
    must_fail(edit("period_ns=10000000", "period_ns=20000000"), "wrong period", "tick period")
    must_fail(edit("ticks=11 ", "ticks=10 "), "wrong ticks", "ticks=")
    must_fail(edit("ns=100000000 period", "ns=100000001 period"), "ns mismatch", "kernel saw ns")
    bad = edit("FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=1 now=111", "FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=1 now=105")
    must_fail(bad, "timeout before deadline", "before its deadline")
    # A/F
    must_fail([l.replace("current=0 ", "current=21 ") if "gen=1 " in l or "gen=2 " in l or "gen=3 " in l else l
               for l in good], "no idle collection", "idle CPU")
    must_fail(insert_after("FUTEX_TIME_W_REGISTER tid=20 gen=2", "USER_LOG tid=21 msg=FTW_B_WAKE woke=0 tries=1"),
              "a helper polled during A", "helper ran")
    # B/C/D
    must_fail(edit("msg=FTW_B_WAKE woke=1", "msg=FTW_B_WAKE woke=0"), "B helper wake", "B: helper")
    must_fail(edit("gen=6 now=", "gen=6 now=", 0) and
              [l.replace("va=0x5004", "va=0x5000") if "FUTEX_TIME_W_TIMEOUT" in l and "gen=6 " in l else l
               for l in good], "C timed out on the source", "requeue target")
    must_fail(edit("wake_source=0", "wake_source=1"), "ghost waiter after C", "C: wakes")
    must_fail(edit("moved=1 woke=1", "moved=1 woke=0"), "D wake", "D: requeue/wake")
    # G
    must_fail(edit("FUTEX_TIME_W_IN_WINDOW_WAKE cpu=1", "FUTEX_TIME_W_IN_WINDOW_WAKE cpu=0"),
              "window wake from the same CPU", "another CPU")
    must_fail(edit("window=1 woke=0", "window=1 woke=1"), "in-window wake took the cycle", "took a cycle")
    must_fail(edit("competing_wakes=1 end=woke", "competing_wakes=0 end=bound"), "bound window", "bound")
    must_fail(drop("FUTEX_TIME_W_WINDOW_CLOSE"), "unclosed window", "never closed")
    must_fail(drop("FUTEX_TIME_W_WINDOW_SKIPPED"), "timeout without window decision", "window decision")
    i_open = next(i for i, l in enumerate(good) if "WINDOW_OPEN" in l)
    i_wake = i_open + 1
    swapped = list(good)
    swapped[i_open], swapped[i_wake] = swapped[i_wake], swapped[i_open]
    must_fail(swapped, "in-window wake before the window opened", "outside OPEN..CLOSE")
    must_fail([l.replace("end=woke", "end=no_peer") for l in good], "no woken window", "G coverage")
    # E
    must_fail(drop("FUTEX_TIMEOUT_EXPIRED_UNPLACED"), "E without saturation", "not full")
    must_fail(edit("fillers=128 spawn_refused=1", "fillers=10 spawn_refused=1"), "too few fillers", "not saturated")
    must_fail(edit("err=SchedulerFull", "err=WrongObject"), "spawn refused for another reason", "SchedulerFull refusal")
    must_fail(drop("SPAWN_THREAD_SPLIT_OK parent_tid=21"), "cell overcounts its fillers", "the kernel spawned")
    must_fail(edit("wake1=0", "wake1=1"), "E wake found the expired waiter", "E: {")
    # calls
    must_fail(drop("msg=FTW_RET role=G"), "a G call never returned", "never returned")
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch", choices=sorted(PERIOD_NS))
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--core-rc", type=int)
    ap.add_argument("--core-log", action="append", default=[])
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        print("FUTEX_TIME_WITNESS_GRADER_SELF_TEST result=ok")
        return 0
    if not a.arch or not a.log:
        ap.error("--arch and a log are required")
    with open(a.log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    fails, facts = grade(lines, a.arch, a.smp)
    core_ok, reasons = core_verdict(a.core_rc, a.core_log)
    if not core_ok:
        fails.append(f"core smoke not clean (rc={a.core_rc}): {reasons[:5]}")
    if a.smp < 2:
        fails.append("the race sequence needs SMP >= 2")
    for f in fails:
        print(f"[futex-time-witness][fail] {f}")
    fact_s = " ".join(f"{k}={v}" for k, v in facts.items())
    result = "ok" if not fails else "fail"
    print(f"FUTEX_TIME_WITNESS_SEAL arch={a.arch} smp={a.smp} result={result} {fact_s}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
