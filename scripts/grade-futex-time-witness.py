#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-TIME1 / FUTEX-ABS-BITSET1 — grade one live timed-FutexWait witness boot.

Input: the normalized serial log of a boot built with `futex-time-witness` (kernel and init) and
booted with `yarm.futex_requeue_witness=1`. Init's waiter thread W is the only thread that issues
timed waits. Every one passes an ABSOLUTE CLOCK_MONOTONIC deadline the cell computed from its own
clock reading, and a wake mask; it logs `FTW_CALL role n ns now deadline mask addr expected` before
each and `FTW_RET role n result` after it (the user runtime's own decode of the returned registers).
The kernel, from inside the deciding transactions, logs:

  FUTEX_TIME_W_ADMIT     tid deadline now mask result      NR 9's one admission decision (after the
                                                           word matched; now = the reading it used)
  FUTEX_TIME_W_REGISTER  tid gen deadline now va mask      the compare-and-register hold
  FUTEX_TIME_W_WAKE      tid gen va mask                   a wake's claim of the timed cycle
  FUTEX_TIME_W_TIMEOUT   cpu tid gen now current va deadline mask   the collector's timeout claim
                                                           (deadline and mask as the claim found
                                                           them in the record)
  FUTEX_TIME_W_LOST      cpu tid gen                       the collector found it due, lost it
  FUTEX_TIME_W_BITSET_SKIP tid mask wake_mask va           a masked wake passed a live waiter over
  FUTEX_TIME_W_WINDOW_OPEN / _CLOSE / _SKIPPED, FUTEX_TIME_W_IN_WINDOW_WAKE   (sequence G)
  FUTEX_TIMEOUT_EXPIRED_UNPLACED tid slot generation cpu home now tick   (the timeout won, queue
                                                           full; now in monotonic ns)
  SATW_REFUSED class=futex cpu home tid gen prio queued reserved cap full deadline now facts
      (SCHED-SAT-EVIDENCE1, `kernel::sat_witness`: the refused home queue's entries and promised
      slots at the waiter's priority, captured under that CPU's lock BY the refusal; `full=1` iff
      queued + reserved >= cap; gen is the cycle's wait generation, deadline the record's)

Markers fail closed: a marker whose prefix or fields are damaged fails the boot; nothing is
skipped and nothing is repaired.

Every call (W's, in order): exactly one ADMIT of W between its CALL and RET. The kernel took the
caller's instant as it was (ADMIT deadline = CALL deadline, no conversion) with the call's mask,
and read the clock after the caller did (ADMIT now >= CALL now; the clock is one global maximum).
  result=registered: now < deadline; exactly one REGISTER of W after it, same deadline, now and
    mask, the call's word (va = addr), a fresh generation (strictly increasing). The cycle has
    EXACTLY ONE terminal marker in the whole log, a WAKE or a TIMEOUT of its (tid, gen), between
    REGISTER and RET, naming the registered mask; a TIMEOUT names the registered deadline (never
    reset, never converted, also after a requeue) and its now >= deadline (never early). A LOST
    is legal beside either terminal, between REGISTER and RET; it never ends a cycle.
    RET woken <=> WAKE; RET timed_out <=> TIMEOUT.
  result=timed_out: now >= deadline, NO REGISTER (nothing published), RET timed_out. Required for
    P; legal for G (a 1 ms deadline can pass before admission), counted outside race coverage;
    illegal elsewhere.
  result=unavailable fails (this boot has a clock). Any other result fails, and so does FTW_FAIL.
Every BITSET_SKIP passed over a waiter whose mask is DISJOINT from the wake's (a skipped match is
a lost wake), and lies inside sequence M's or C's wakes (no other wake is masked).

Sequences:
  A/F  three A calls end timed_out; during each, no user thread of the cell but W logs anything
       (H and S2 are parked); at least one A timeout was collected on an idle CPU (current=0).
  P    deadline 1 (long past): admitted timed_out with nothing published.
  B    woken (FTW_B_WAKE woke=1); B2 timed_out at now >= B's deadline (B never completed again).
  M    FTW_M_PARKED w=1 s2=1 (W then S2 moved onto the target). H's FTW_M_WAKE k=1 mask=0x2 woke=1,
       k=2 mask=0x2 woke=0, k=3 mask=0x1 woke=1, in that order. Between PARKED and k=1 AND between
       k=1 and k=2: a BITSET_SKIP of W (mask 0x1, wake_mask 0x2, va target) — the non-matching W
       was passed over in place and did not consume the count; S2 (mask 0x2) took it
       (FTW_M_S2_RET result=woken, after PARKED). W's terminal is a WAKE on the target with mask
       0x1, between k=2 and k=3 (only its own mask woke it); M woken.
  C    FTW_C_REQUEUE moved=1 disjoint_wake=0; a BITSET_SKIP of W (mask 0x4, wake_mask 0x1, va
       target) between C's REGISTER and that line — the moved record kept its mask; C timed_out on
       the TARGET word (TIMEOUT va = target != addr) with its registered deadline and mask 0x4;
       FTW_C_AFTER wake_source=0 wake_target=0.
  D    FTW_D_REQUEUE_WAKE moved=1 woke=1; D woken on the target (WAKE va = target, mask 0x8: an
       ordinary wake releases a masked waiter); D2 timed_out at now >= D's deadline.
  G    every parked G call woken or timed_out per its terminal marker. Every G TIMEOUT is followed
       by exactly one WINDOW_OPEN or WINDOW_SKIPPED of its gen (a non-G timeout has neither). Per
       window: one OPEN, one CLOSE (same cpu, window, gen) after it, every IN_WINDOW_WAKE of it
       between them, each woke=0 (the timeout had already claimed the cycle); end=woke needs a
       wake from ANOTHER CPU; end=bound fails; end=no_peer is a refusal (no coverage). G's RET
       comes after its CLOSE. Coverage: >= 1 window ended woke, >= 1 G cycle won by a wake, >= 1
       won by the timeout (parked cycles only).
  E    SCHED-SAT-EVIDENCE1 — W's own home queue, not spawn counts: at least one SATW_REFUSED
       class=futex of W's E cycle (tid, gen) with facts=ok, full=1 (consistent with its numbers),
       deadline = E's registered deadline, home = the EXPIRED marker's home, between E's REGISTER
       and its EXPIRED, and before H's FTW_E_EXPIRED_SEEN (H was still topping up and holding: it
       releases only after that line); exactly one FUTEX_TIMEOUT_EXPIRED_UNPLACED of W between
       E's REGISTER and its one final TIMEOUT (now >= deadline) — the later, exactly-once
       placement; FTW_E_EXPIRED_SEEN gone=1 wake1=0 wake2=0 (no wake found it after the timeout
       won); E timed_out. Diagnostics, which earn nothing: FTW_E_HOLD_END fillers = H's
       SPAWN_THREAD_SPLIT_OK count, its spawn_refused = H's SPAWN_THREAD_SPLIT_FAIL count (>= 1,
       each err=SchedulerFull).
  FTW_WAITER_DONE fails=0, FTW_DONE result=ok, plus the standard core smoke.

No latency is graded: a deadline is a lower bound, and its lateness is reported as a fact only.

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

ARCHES = ("aarch64", "riscv64", "x86_64")
FATAL = (
    "YARM_AARCH64_TRAP_HANDLE halting",
    "RISCV_TRAP_HANDLE_FAILED",
    "x86 shared trap dispatch failed",
    "FUTEX_WAKE_RESERVED_COMMIT_REFUSED",
    "FUTEX_WAIT_BROAD_ENTRY",
    "panicked at",
    "KERNEL PANIC",
)
ROLES_EXPECT = {"A": "timed_out", "P": "timed_out", "B": "woken", "B2": "timed_out", "M": "woken",
                "C": "timed_out", "D": "woken", "D2": "timed_out", "E": "timed_out"}
CALLS_EXPECT = {"A": 3, "P": 1, "B": 1, "B2": 1, "M": 1, "C": 1, "D": 1, "D2": 1, "G": 32, "E": 1}
# Roles whose deadline may already have passed at admission (nothing published).
ADMIT_TIMED_OUT_OK = {"P", "G"}
MASK = {"M": 0x1, "C": 0x4, "D": 0x8}
MATCH_ANY = 0xFFFFFFFF
MIN_FILLERS = 64

INT = r"\d+"
HEX = r"0x[0-9a-f]+"
K = {
    "ADMIT": ("FUTEX_TIME_W_ADMIT",
              {"tid": INT, "deadline": INT, "now": INT, "mask": HEX,
               "result": "registered|timed_out|unavailable"}),
    "REGISTER": ("FUTEX_TIME_W_REGISTER",
                 {"tid": INT, "gen": INT, "deadline": INT, "now": INT, "va": HEX, "mask": HEX}),
    "WAKE": ("FUTEX_TIME_W_WAKE", {"tid": INT, "gen": INT, "va": HEX, "mask": HEX}),
    "TIMEOUT": ("FUTEX_TIME_W_TIMEOUT",
                {"cpu": INT, "tid": INT, "gen": INT, "now": INT, "current": INT, "va": HEX,
                 "deadline": INT, "mask": HEX}),
    "LOST": ("FUTEX_TIME_W_LOST", {"cpu": INT, "tid": INT, "gen": INT}),
    "SKIP": ("FUTEX_TIME_W_BITSET_SKIP", {"tid": INT, "mask": HEX, "wake_mask": HEX, "va": HEX}),
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
                {"tid": INT, "slot": INT, "generation": INT, "cpu": INT, "home": INT, "now": INT,
                 "tick": INT}),
    "SATW": ("SATW_REFUSED", {"class": r"sleep|futex|recv", "cpu": INT, "home": INT, "tid": INT,
                              "gen": INT, "n": INT, "prio": INT, "queued": INT, "reserved": INT, "cap": INT,
                              "full": r"[01]", "deadline": INT, "now": INT, "facts": r"ok|lost"}),
}
U = {
    "CALL": {"role": r"[A-Z][0-9]?", "n": INT, "ns": INT, "now": INT, "deadline": INT, "mask": HEX,
             "addr": HEX, "expected": HEX},
    "RET": {"role": r"[A-Z][0-9]?", "n": INT, "result": r"[a-z_]+"},
    "THREADS": {"waiter": INT, "helper": INT, "signaller": INT},
    "B_WAKE": {"woke": INT, "tries": INT},
    "M_PARKED": {"w": INT, "s2": INT, "target": HEX},
    "M_WAKE": {"k": INT, "mask": HEX, "woke": INT},
    "M_S2_RET": {"result": r"[a-z_]+"},
    "C_REQUEUE": {"moved": INT, "target": HEX, "disjoint_wake": INT},
    "C_AFTER": {"wake_source": INT, "wake_target": INT},
    "D_REQUEUE_WAKE": {"moved": INT, "woke": INT, "target": HEX},
    "G_SUMMARY": {"cycles": INT, "woken": INT, "timed_out": INT},
    "G_SIGNALLER": {"who": r"H|S2|S3", "attempts": INT, "woke": INT},
    "E_SATURATED": {"fillers": INT, "spawn_refused": INT},
    "E_EXPIRED_SEEN": {"gone": INT, "pings": INT, "wake1": INT, "wake2": INT},
    "E_HOLD_END": {"fillers": INT, "topups": INT, "spawn_refused": INT},
    "WAITER_DONE": {"calls": INT, "fails": INT},
    "DONE": {"waiter": INT, "calls": INT, "fails": INT, "result": "ok|fail"},
    "BEGIN": {"s_tid": INT, "g_cycles": INT, "fillers_max": INT},
    "FAIL": {"what": r"\S+"},
}
FRAGMENTS = ("FUTEX_TIME", "UTEX_TIME_W", "TIME_W_", "_W_WINDOW", "IN_WINDOW_WAKE", "BITSET_SKIP",
             "EXPIRED_UNPLACED", "msg=FTW_", "FTW_CALL", "FTW_RET", "FTW_DONE", "SATW_")


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


def hx(v):
    return int(v, 16)


def grade(lines, arch, smp):
    fails = []
    fail = fails.append
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
    for e in only("ADMIT"):
        if e["result"] == "unavailable":
            fail(f"tid {e['tid']}: a timed wait refused for an unavailable clock on a clocked boot")
    cycles = []
    admitted_expired = []
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
        ad = [e for e in span if e["kind"] == "ADMIT" and e["tid"] == W]
        rg = [e for e in span if e["kind"] == "REGISTER" and e["tid"] == W]
        if len(ad) != 1:
            fail(f"call {n} ({c['role']}): {len(ad)} ADMIT markers (need exactly 1)")
            continue
        ad = ad[0]
        if ad["deadline"] != c["deadline"]:
            fail(f"call {n}: kernel admitted deadline {ad['deadline']}, the call passed "
                 f"{c['deadline']} (an instant is never converted)")
        if hx(ad["mask"]) != hx(c["mask"]):
            fail(f"call {n}: kernel admitted mask {ad['mask']}, the call passed {c['mask']}")
        if int(ad["now"]) < int(c["now"]):
            fail(f"call {n}: kernel clock {ad['now']} behind the caller's earlier reading {c['now']}")
        dl = int(c["deadline"])
        if ad["result"] == "timed_out":
            if int(ad["now"]) < dl:
                fail(f"call {n}: admitted timed_out at {ad['now']} before its deadline {dl}")
            if rg:
                fail(f"call {n}: admitted timed_out yet published a REGISTER")
            if r.get("result") != "timed_out":
                fail(f"call {n}: admitted timed_out but ring 3 saw {r.get('result', 'error')}")
            if c["role"] not in ADMIT_TIMED_OUT_OK:
                fail(f"call {n} ({c['role']}): its deadline passed before admission (only "
                     f"{sorted(ADMIT_TIMED_OUT_OK)} may)")
            admitted_expired.append(c)
            continue
        if c["role"] == "P":
            fail(f"call {n} (P): a passed deadline was admitted as {ad['result']}")
        if ad["result"] != "registered":
            continue
        if int(ad["now"]) >= dl:
            fail(f"call {n}: registered at {ad['now']} with its deadline {dl} already reached")
        if len(rg) != 1:
            fail(f"call {n} ({c['role']}): admitted registered with {len(rg)} REGISTER markers")
            continue
        rg = rg[0]
        if rg["deadline"] != ad["deadline"] or rg["now"] != ad["now"] or hx(rg["mask"]) != hx(ad["mask"]):
            fail(f"call {n}: REGISTER {rg['deadline']}/{rg['now']}/{rg['mask']} != ADMIT "
                 f"{ad['deadline']}/{ad['now']}/{ad['mask']}")
        if rg["va"] != c["addr"]:
            fail(f"call {n}: registered on {rg['va']}, called on {c['addr']}")
        if not ad["at"] < rg["at"]:
            fail(f"call {n}: REGISTER before the ADMIT decision")
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
        if hx(t["mask"]) != hx(rg["mask"]):
            fail(f"call {n}: its mask changed from {rg['mask']} to {t['mask']} by its {t['kind']}")
        if t["kind"] == "TIMEOUT":
            if t["deadline"] != rg["deadline"]:
                fail(f"call {n}: its deadline changed from {rg['deadline']} to {t['deadline']} "
                     f"by its TIMEOUT")
            if int(t["now"]) < int(rg["deadline"]):
                fail(f"call {n}: timed out at {t['now']} before its deadline {rg['deadline']}")
        for x in lost.get((W, rg["gen"]), []):
            if not rg["at"] < x["at"] < r["at"]:
                fail(f"call {n}: a LOST of its cycle outside REGISTER..RET")
        exp = ROLES_EXPECT.get(c["role"])
        if exp and r.get("result") != exp:
            fail(f"call {n} ({c['role']}): result {r.get('result')}, sequence requires {exp}")
        if c["role"] in MASK and hx(c["mask"]) != MASK[c["role"]]:
            fail(f"call {n} ({c['role']}): mask {c['mask']}, the sequence uses {hex(MASK[c['role']])}")
        cycles.append(dict(n=n, role=c["role"], call=c, ret=r, ad=ad, rg=rg, term=t, gen=rg["gen"]))
    by_role = {}
    for cy in cycles:
        by_role.setdefault(cy["role"], []).append(cy)
    facts = {}
    late = [int(cy["term"]["now"]) - int(cy["rg"]["deadline"]) for cy in cycles
            if cy["term"]["kind"] == "TIMEOUT"]
    if late:
        facts["timeout_lateness_ns_max"] = max(late)

    # ── every masked skip passed over a disjoint waiter, and only where a masked wake runs ──
    skips = only("SKIP")
    for k in skips:
        if hx(k["mask"]) & hx(k["wake_mask"]):
            fail(f"BITSET_SKIP of tid {k['tid']}: mask {k['mask']} matches the wake's "
                 f"{k['wake_mask']} (a skipped match is a lost wake)")
        if hx(k["wake_mask"]) == MATCH_ANY:
            fail(f"BITSET_SKIP of tid {k['tid']} by an unmasked wake")
    accounted = set()

    # ── A/F: no polling thread; an idle collector ──
    a_idle = 0
    for cy in by_role.get("A", []):
        span = [e for e in ev if cy["call"]["at"] < e["at"] < cy["ret"]["at"]]
        noisy = [e for e in span if e["kind"].startswith("U_") and e.get("utid") in (H, S2)]
        if noisy:
            fail(f"A call {cy['n']}: a helper ran during the wait: {noisy[0]['kind']}")
        if cy["term"]["kind"] == "TIMEOUT" and cy["term"]["current"] == "0":
            a_idle += 1
    if a_idle < 1:
        fail("F: no A timeout was collected on an idle CPU (current=0)")
    facts["a_idle_collections"] = a_idle
    # ── P: required to be the admission-time expiry ──
    if not any(c["role"] == "P" for c in admitted_expired):
        fail("P: the passed deadline was not refused at admission")

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

    # ── M: selective wake ──
    mp = one("U_M_PARKED")
    mw = {e["k"]: e for e in only("U_M_WAKE")}
    if len(mw) != 3 or len(only("U_M_WAKE")) != 3:
        fail(f"M: {len(only('U_M_WAKE'))} FTW_M_WAKE lines (need k=1,2,3 once each)")
    m_ok = mp and len(mw) == 3 and set(mw) == {"1", "2", "3"}
    if mp and (mp.get("w") != "1" or mp.get("s2") != "1"):
        fail(f"M: the waiters were not both parked and moved: {mp}")
    if m_ok:
        want = {"1": (0x2, "1"), "2": (0x2, "0"), "3": (0x1, "1")}
        for k, (mask, woke) in want.items():
            e = mw[k]
            if hx(e["mask"]) != mask or e["woke"] != woke or e["utid"] != H:
                fail(f"M: wake k={k} mask={e['mask']} woke={e['woke']} (by {e['utid']}), "
                     f"need mask={hex(mask)} woke={woke} by H")
        if not mp["at"] < mw["1"]["at"] < mw["2"]["at"] < mw["3"]["at"]:
            fail("M: the wakes are out of order")
        bounds = [(mp["at"], mw["1"]["at"]), (mw["1"]["at"], mw["2"]["at"])]
        for lo, hi in bounds:
            mine = [k for k in skips if lo < k["at"] < hi]
            accounted.update(id(k) for k in mine)
            good = [k for k in mine if k["tid"] == W and hx(k["mask"]) == 0x1
                    and hx(k["wake_mask"]) == 0x2 and k["va"] == mp["target"]]
            if not good:
                fail("M: a mask-0x2 wake did not pass W (mask 0x1) over on the target — the mask "
                     "was ignored, or W was not ahead of S2")
        accounted.update(id(k) for k in skips if mw["2"]["at"] < k["at"] < mw["3"]["at"])
        s2r = [e for e in only("U_M_S2_RET")]
        if len(s2r) != 1 or s2r[0].get("result") != "woken" or s2r[0]["utid"] != S2:
            fail(f"M: S2's masked wait did not return woken exactly once: {s2r}")
        elif not mp["at"] < s2r[0]["at"]:
            fail("M: S2 returned before it was moved")
        for cy in by_role.get("M", []):
            t = cy["term"]
            if t["kind"] != "WAKE":
                continue
            if t["va"] != mp["target"]:
                fail(f"M: W woken on {t['va']}, not the target {mp['target']}")
            if not mw["2"]["at"] < t["at"] < mw["3"]["at"]:
                fail("M: W was woken outside its own-mask wake (k=3)")
    # ── C ──
    cr = one("U_C_REQUEUE")
    if cr and (cr.get("moved") != "1" or cr.get("disjoint_wake") != "0"):
        fail(f"C: requeue moved={cr.get('moved')} disjoint_wake={cr.get('disjoint_wake')} "
             f"(need 1/0)")
    for cy in by_role.get("C", []):
        if cr and cy["term"]["va"] != cr.get("target"):
            fail(f"C: timed out on {cy['term']['va']}, not the requeue target {cr.get('target')}")
        if cy["term"]["va"] == cy["call"]["addr"]:
            fail("C: timed out on the word it was requeued away from")
        if cr:
            mine = [k for k in skips if cy["rg"]["at"] < k["at"] < cr["at"]]
            accounted.update(id(k) for k in mine)
            if not [k for k in mine if k["tid"] == W and hx(k["mask"]) == 0x4
                    and hx(k["wake_mask"]) == 0x1 and k["va"] == cr["target"]]:
                fail("C: the disjoint wake of the target did not pass the moved W (mask 0x4) "
                     "over — its mask did not survive the requeue")
    ca = one("U_C_AFTER")
    if ca and (ca.get("wake_source") != "0" or ca.get("wake_target") != "0"):
        fail(f"C: wakes after the timeout found a waiter: {ca}")
    for k in skips:
        if id(k) not in accounted:
            fail(f"BITSET_SKIP of tid {k['tid']} outside sequence M's and C's masked wakes")
    facts["bitset_skips"] = len(skips)
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
    # FUTEX-ABS-BITSET1: a G deadline (1 ms out) that passed before admission is a timeout the
    # cell counts, with nothing parked: it earns no race coverage.
    g_admit = sum(1 for c in admitted_expired if c["role"] == "G")
    gs = one("U_G_SUMMARY")
    if gs and (int(gs.get("woken", -1)), int(gs.get("timed_out", -1))) != (g_woken, g_timed + g_admit):
        fail(f"G: the cell counted {gs.get('woken')}/{gs.get('timed_out')}, the kernel "
             f"{g_woken}/{g_timed}+{g_admit} at admission")
    facts.update(g_admitted_expired=g_admit)
    facts.update(g_woken=g_woken, g_timed_out=g_timed, g_windows_woke=woke_windows,
                 g_windows_refused=refused, g_skipped=len(only("SKIPPED")),
                 in_window_wakes=len(inw))

    # ── E: W's home queue held full through its refusal (SCHED-SAT-EVIDENCE1) ──
    es, eh = one("U_E_SATURATED"), one("U_E_HOLD_END")
    h_ok = [e for e in only("SPAWN_OK") if e["parent_tid"] == H]
    h_fail = [e for e in only("SPAWN_FAIL") if e["parent_tid"] == H]
    if eh and len(h_ok) != int(eh.get("fillers", -1)):
        fail(f"E: the cell counted {eh.get('fillers')} fillers, the kernel spawned {len(h_ok)} for H")
    if eh and len(h_fail) != int(eh.get("spawn_refused", -1)):
        fail(f"E: the cell counted {eh.get('spawn_refused')} spawn refusals, the kernel {len(h_fail)}")
    if not h_fail or any(e["err"] != "SchedulerFull" for e in h_fail):
        fail(f"E: H's spawning must end in SchedulerFull refusals only (kernel: {sorted({e['err'] for e in h_fail})})")
    facts["e_spawn_refusals"] = len(h_fail)
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
        sat = [e for e in only("SATW") if e["class"] == "futex" and e["tid"] == W
               and e["gen"] == cy["rg"]["gen"]]
        good, why = [], []
        for e in sat:
            room = int(e["queued"]) + int(e["reserved"]) < int(e["cap"])
            if e["facts"] != "ok":
                why.append("facts lost")
            elif (e["full"] == "1") == room:
                why.append("inconsistent facts")
            elif e["full"] != "1":
                why.append(f"room left ({e['queued']}+{e['reserved']}<{e['cap']})")
            elif e["home"] != x["home"]:
                why.append(f"home {e['home']} != {x['home']}")
            elif e["deadline"] != cy["rg"]["deadline"]:
                why.append(f"deadline {e['deadline']} != registered {cy['rg']['deadline']}")
            elif not cy["rg"]["at"] < e["at"] < x["at"]:
                why.append("outside REGISTER..EXPIRED")
            elif not ee or not e["at"] < ee["at"]:
                why.append("after H stopped holding")
            else:
                good.append(e)
        if not good:
            fail(f"E: no refusal proves W's home queue (cpu {x['home']}) full when its timeout won: "
                 f"{len(sat)} refusals, rejected: {sorted(set(why))}")
        facts.update(e_fillers=eh.get("fillers"), e_topups=eh.get("topups"), e_pings=ee.get("pings"),
                     e_proving=len(good))
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
def synth():
    W, H, S2 = "20", "21", "22"
    out = [f"USER_LOG tid=1 msg=FTW_BEGIN s_tid=1 g_cycles=32 fillers_max=176",
           f"USER_LOG tid=1 msg=FTW_THREADS waiter={W} helper={H} signaller={S2}"]
    st = {"n": 0, "gen": 0, "now": 1_000_000_000}
    QA, QB, WM, WM3 = "0x5000", "0x5004", "0x5010", "0x5014"
    ANY = "0xffffffff"

    def call(role, ns, addr, end, va=None, mask=ANY, extra_mid=(), extra_end=(), current="0"):
        st["n"] += 1
        st["gen"] += 1
        n, g = st["n"], st["gen"]
        now = st["now"]
        dl = now + ns
        an = now + 5
        lines = [f"USER_LOG tid={W} msg=FTW_CALL role={role} n={n} ns={ns} now={now} deadline={dl} "
                 f"mask={mask} addr={addr} expected=0x1",
                 f"FUTEX_TIME_W_ADMIT tid={W} deadline={dl} now={an} mask={mask} result=registered",
                 f"FUTEX_TIME_W_REGISTER tid={W} gen={g} deadline={dl} now={an} va={addr} mask={mask}"]
        lines += [x.format(gen=g, dl=dl) for x in extra_mid]
        va = va or addr
        if end == "woken":
            lines.append(f"FUTEX_TIME_W_WAKE tid={W} gen={g} va={va} mask={mask}")
            st["now"] = an + 100
        else:
            st["now"] = dl + 10
            lines.append(f"FUTEX_TIME_W_TIMEOUT cpu=0 tid={W} gen={g} now={dl + 7} current={current} "
                         f"va={va} deadline={dl} mask={mask}")
        lines += [x.format(gen=g, dl=dl) for x in extra_end]
        lines.append(f"USER_LOG tid={W} msg=FTW_RET role={role} n={n} result={end}")
        return lines

    def call_expired(role, addr, ns):
        st["n"] += 1
        n = st["n"]
        now = st["now"]
        dl = 1 if role == "P" else now + ns
        an = max(now + 5, dl)
        st["now"] = an + 10
        return [f"USER_LOG tid={W} msg=FTW_CALL role={role} n={n} ns={ns} now={now} deadline={dl} "
                f"mask={ANY} addr={addr} expected=0x1",
                f"FUTEX_TIME_W_ADMIT tid={W} deadline={dl} now={an} mask={ANY} result=timed_out",
                f"USER_LOG tid={W} msg=FTW_RET role={role} n={n} result=timed_out"]

    for _ in range(3):
        out += call("A", 100_000_000, "0x4000", "timed_out")
    out += call_expired("P", "0x4000", 0)
    out += call("B", 2_000_000_000, "0x4010", "woken")
    out.append(f"USER_LOG tid={H} msg=FTW_B_WAKE woke=1 tries=3")
    out += call("B2", 4_000_000_000, "0x4014", "timed_out", current="21")
    out += call("M", 20_000_000_000, WM, "woken", va=WM3, mask="0x1",
                extra_mid=(f"USER_LOG tid={H} msg=FTW_M_PARKED w=1 s2=1 target={WM3}",
                           f"FUTEX_TIME_W_BITSET_SKIP tid={W} mask=0x1 wake_mask=0x2 va={WM3}",
                           f"USER_LOG tid={H} msg=FTW_M_WAKE k=1 mask=0x2 woke=1",
                           f"USER_LOG tid={S2} msg=FTW_M_S2_RET result=woken",
                           f"FUTEX_TIME_W_BITSET_SKIP tid={W} mask=0x1 wake_mask=0x2 va={WM3}",
                           f"USER_LOG tid={H} msg=FTW_M_WAKE k=2 mask=0x2 woke=0"),
                extra_end=(f"USER_LOG tid={H} msg=FTW_M_WAKE k=3 mask=0x1 woke=1",))
    out += call("C", 2_000_000_000, QA, "timed_out", va=QB, mask="0x4", current="21",
                extra_mid=(f"FUTEX_TIME_W_BITSET_SKIP tid={W} mask=0x4 wake_mask=0x1 va={QB}",
                           f"USER_LOG tid={H} msg=FTW_C_REQUEUE moved=1 target={QB} disjoint_wake=0"))
    out.append(f"USER_LOG tid={W} msg=FTW_C_AFTER wake_source=0 wake_target=0")
    out += call("D", 2_000_000_000, QA, "woken", va=QB, mask="0x8")
    out.append(f"USER_LOG tid={H} msg=FTW_D_REQUEUE_WAKE moved=1 woke=1 target={QB}")
    out += call("D2", 4_000_000_000, "0x4020", "timed_out", current="21")
    win = 0
    for k in range(32):
        if k % 2 == 0:
            out += call("G", 1_000_000, "0x4030", "woken")
        elif k == 31:
            out += call_expired("G", "0x4030", 1_000_000)
        else:
            win += 1
            if k % 4 == 1:
                tail = (f"FUTEX_TIME_W_WINDOW_OPEN cpu=0 window={win} tid={W} gen={{gen}}",
                        f"FUTEX_TIME_W_IN_WINDOW_WAKE cpu=1 tid={S2} window={win} woke=0",
                        f"FUTEX_TIME_W_WINDOW_CLOSE cpu=0 window={win} gen={{gen}} competing_wakes=1 end=woke")
            else:
                tail = (f"FUTEX_TIME_W_WINDOW_SKIPPED cpu=0 tid={W} gen={{gen}} reason=no_peer_running",)
            out += call("G", 1_000_000, "0x4030", "timed_out", extra_end=tail, current="22")
    out.append(f"USER_LOG tid={W} msg=FTW_G_SUMMARY cycles=32 woken=16 timed_out=16")
    out += call("E", 60_000_000_000, "0x4040", "timed_out", current="21",
                extra_mid=tuple(f"SPAWN_THREAD_SPLIT_OK parent_tid={H} tid={30000 + i} entry=0x1 stack=0x10 tls=0x20 result=ok"
                                for i in range(128))
                + (f"SPAWN_THREAD_SPLIT_FAIL parent_tid={H} tid=30200 err=SchedulerFull tasks=0 result=compensated",
                   f"USER_LOG tid={H} msg=FTW_E_SATURATED fillers=128 spawn_refused=1",
                   f"SPAWN_THREAD_SPLIT_OK parent_tid={H} tid=30300 entry=0x1 stack=0x10 tls=0x20 result=ok",
                   f"SPAWN_THREAD_SPLIT_FAIL parent_tid={H} tid=30301 err=SchedulerFull tasks=0 result=compensated",
                   f"SATW_REFUSED class=futex cpu=1 home=0 tid={W} gen={{gen}} n=1 prio=1 queued=64 reserved=0 cap=64 full=1 deadline={{dl}} now={{dl}} facts=ok",
                   f"FUTEX_TIMEOUT_EXPIRED_UNPLACED tid={W} slot=3 generation=9 cpu=1 home=0 now={{dl}} tick=4242",
                   f"USER_LOG tid={H} msg=FTW_E_EXPIRED_SEEN gone=1 pings=40 wake1=0 wake2=0",
                   f"USER_LOG tid={H} msg=FTW_E_HOLD_END fillers=129 topups=1 spawn_refused=2"))
    n = st["n"]
    out += [f"USER_LOG tid={W} msg=FTW_WAITER_DONE calls={n} fails=0",
            f"USER_LOG tid=1 msg=FTW_DONE waiter={W} calls={n} fails=0 result=ok"]
    return out


def self_test():
    good = synth()
    for arch in ARCHES:
        f, _ = grade(good, arch, 2)
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

    def edit_where(pred, old, new):
        hits = [i for i, l in enumerate(good) if pred(l) and old in l]
        assert hits, old
        out = list(good)
        for i in hits:
            out[i] = out[i].replace(old, new)
        return out

    def drop(needle, nth=0):
        hits = [i for i, l in enumerate(good) if needle in l]
        assert len(hits) > nth, needle
        return good[:hits[nth]] + good[hits[nth] + 1:]

    def drop_where(pred):
        assert any(pred(l) for l in good)
        return [l for l in good if not pred(l)]

    def insert_after(needle, line, nth=0):
        hits = [i for i, l in enumerate(good) if needle in l]
        assert len(hits) > nth, needle
        return good[:hits[nth] + 1] + [line] + good[hits[nth] + 1:]

    def gen_of(role):
        n = next(l for l in good if f"msg=FTW_CALL role={role} " in l).split(" n=")[1].split()[0]
        i = next(i for i, l in enumerate(good) if f"msg=FTW_CALL role={role} n={n} " in l)
        r = next(l for l in good[i:] if l.startswith("FUTEX_TIME_W_REGISTER"))
        return r.split(" gen=")[1].split()[0]

    gm, gc, gd = gen_of("M"), gen_of("C"), gen_of("D")
    # integrity
    must_fail(edit("FUTEX_TIME_W_TIMEOUT cpu=0", "FUTEX_TIME_W_TIMEOUT cpu=x"), "damaged field", "malformed")
    must_fail(edit("FUTEX_TIME_W_REGISTER", "FUTEX_TIME_W_REG1STER"), "damaged prefix", "damaged")
    must_fail(edit("FUTEX_TIME_W_REGISTER", "FUTEX_TIMEX_W_REGISTER"), "damaged prefix 2", "damaged")
    must_fail(edit("FUTEX_TIME_W_BITSET_SKIP", "FUTEX_TIME_W_BITSET_SKlP"), "damaged skip", "damaged")
    must_fail(edit("FUTEX_TIME_W_ADMIT tid=20", "FUTEX_TIME_W_ADMIT tid=x"), "damaged admit", "malformed")
    must_fail(good + ["USER_LOG tid=20 msg=FTW_FAIL what=A"], "cell failure", "FTW_FAIL")
    must_fail([l for l in good if "FTW_DONE" not in l], "no DONE", "exactly one U_DONE")
    must_fail(good + ["KERNEL PANIC at foo"], "fatal", "fatal marker")
    # per-call terminal ownership
    must_fail(drop("FUTEX_TIME_W_TIMEOUT"), "missing terminal", "terminal markers")
    dup = insert_after("FUTEX_TIME_W_TIMEOUT", "FUTEX_TIME_W_WAKE tid=20 gen=1 va=0x4000 mask=0xffffffff")
    must_fail(dup, "double terminal", "terminal markers")
    must_fail(edit_where(lambda l: "msg=FTW_RET role=B " in l, "result=woken", "result=timed_out"),
              "woken reported as timeout", "ring 3 saw")
    must_fail(edit("msg=FTW_RET role=A n=1 result=timed_out", "msg=FTW_RET role=A n=1 result=woken"),
              "timeout reported as woken (the old 1)", "ring 3 saw")
    f, _ = grade(insert_after("FUTEX_TIME_W_TIMEOUT", "FUTEX_TIME_W_LOST cpu=1 tid=20 gen=1"), "riscv64", 2)
    assert not f, ("a second collector losing the race is legal", f)
    must_fail(insert_after("msg=FTW_BEGIN", "FUTEX_TIME_W_LOST cpu=1 tid=20 gen=1"),
              "lost before its cycle registered", "outside REGISTER..RET")
    # FUTEX-ABS-BITSET1: the instant, exactly as passed, in the monotonic domain
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_ADMIT") and "result=registered" in l,
                         "deadline=1100000000 ", "deadline=1100000001 "),
              "the kernel converted (rounded) the instant", "kernel admitted deadline")
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_REGISTER") and " gen=1 " in l,
                         "deadline=1100000000 ", "deadline=1100000001 "),
              "registered another deadline", "REGISTER")
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_ADMIT") and "mask=0x1 " in l,
                         "mask=0x1 ", "mask=0xffffffff "), "admitted another mask", "admitted mask")
    must_fail(edit("FUTEX_TIME_W_ADMIT tid=20 deadline=1100000000 now=1000000005",
                   "FUTEX_TIME_W_ADMIT tid=20 deadline=1100000000 now=999999999"),
              "kernel clock behind the caller", "behind the caller")
    must_fail(edit("result=registered", "result=unavailable"), "unavailable clock", "unavailable clock")
    # (tick-domain deadline: a TIMEOUT that names a converted deadline)
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_TIMEOUT") and f" gen={gc} " in l,
                         "deadline=", "deadline=9"), "deadline changed (reset/converted) by the requeue",
              "deadline changed")
    bad = edit("FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=1 now=1100000007",
               "FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=1 now=1099999999")
    must_fail(bad, "timeout before deadline (early expiry)", "before its deadline")
    # P: a passed deadline refused at admission, nothing published
    i_p = next(i for i, l in enumerate(good) if "msg=FTW_CALL role=P " in l)
    must_fail(good[:i_p + 2] + ["FUTEX_TIME_W_REGISTER tid=20 gen=99 deadline=1 now=1300000025 va=0x4000 mask=0xffffffff"]
              + good[i_p + 2:], "P published", "published a REGISTER")
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_ADMIT") and "deadline=1 " in l,
                         "result=timed_out", "result=registered"), "P registered", "P")
    must_fail(drop_where(lambda l: "msg=FTW_CALL role=P" in l or "msg=FTW_RET role=P" in l
                         or (l.startswith("FUTEX_TIME_W_ADMIT") and "deadline=1 " in l)),
              "no P", "calls per role")
    must_fail([l.replace("result=registered", "result=timed_out")
               if l.startswith("FUTEX_TIME_W_ADMIT") and "deadline=1100000000 " in l else l for l in good],
              "an unreached deadline admitted as expired", "admitted timed_out at")
    # a passed deadline admitted as expired where the sequence must park (A's own deadline passed)
    must_fail([l.replace("now=1000000005 mask=0xffffffff result=registered",
                         "now=1100000005 mask=0xffffffff result=timed_out")
               if l.startswith("FUTEX_TIME_W_ADMIT") and "deadline=1100000000 " in l else l for l in good],
              "A refused at admission", "only")
    # A/F
    must_fail([l.replace("current=0 ", "current=21 ") if "gen=1 " in l or "gen=2 " in l or "gen=3 " in l else l
               for l in good], "no idle collection", "idle CPU")
    must_fail(insert_after("FUTEX_TIME_W_REGISTER tid=20 gen=2", "USER_LOG tid=21 msg=FTW_B_WAKE woke=0 tries=1"),
              "a helper polled during A", "helper ran")
    # B/C/D
    must_fail(edit("msg=FTW_B_WAKE woke=1", "msg=FTW_B_WAKE woke=0"), "B helper wake", "B: helper")
    must_fail([l.replace("va=0x5004", "va=0x5000") if "FUTEX_TIME_W_TIMEOUT" in l and f"gen={gc} " in l else l
               for l in good], "C timed out on the source", "requeue target")
    must_fail(edit("wake_source=0", "wake_source=1"), "ghost waiter after C", "C: wakes")
    must_fail(edit("disjoint_wake=0", "disjoint_wake=1"), "ignored mask: disjoint wake took C", "disjoint_wake=1")
    must_fail(drop("mask=0x4 wake_mask=0x1"), "C's mask reset by the requeue (no skip)", "mask did not survive")
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_TIMEOUT") and f" gen={gc} " in l,
                         "mask=0x4", "mask=0xffffffff"), "C's mask reset by the requeue", "mask changed")
    must_fail(edit("moved=1 woke=1", "moved=1 woke=0"), "D wake", "D: requeue/wake")
    must_fail(edit_where(lambda l: l.startswith("FUTEX_TIME_W_WAKE") and f" gen={gd} " in l,
                         "mask=0x8", "mask=0xffffffff"), "D's mask lost", "mask changed")
    # M
    must_fail(edit("k=1 mask=0x2 woke=1", "k=1 mask=0x2 woke=0"), "non-matching consumed the count", "M: wake k=1")
    must_fail(edit("k=2 mask=0x2 woke=0", "k=2 mask=0x2 woke=1"), "disjoint wake took W", "M: wake k=2")
    must_fail(drop("mask=0x1 wake_mask=0x2"), "ignored wake mask (no first skip)", "did not pass W")
    must_fail(drop("mask=0x1 wake_mask=0x2", 1), "ignored wake mask (no second skip)", "did not pass W")
    must_fail(edit("mask=0x1 wake_mask=0x2", "mask=0x1 wake_mask=0x3"), "skipped a matching waiter", "lost wake")
    must_fail(insert_after("msg=FTW_D_REQUEUE_WAKE", "FUTEX_TIME_W_BITSET_SKIP tid=20 mask=0x8 wake_mask=0x1 va=0x5004"),
              "a skip outside the masked wakes", "outside sequence M")
    must_fail(edit("msg=FTW_M_S2_RET result=woken", "msg=FTW_M_S2_RET result=value_changed"), "S2 not woken", "S2's")
    i_k2 = next(i for i, l in enumerate(good) if "k=2 mask=0x2" in l)
    i_wm = next(i for i, l in enumerate(good) if l.startswith("FUTEX_TIME_W_WAKE") and f" gen={gm} " in l)
    moved = [l for k, l in enumerate(good) if k != i_wm]
    moved.insert(i_k2, good[i_wm])
    must_fail(moved, "W woken by a disjoint wake", "outside its own-mask wake")
    must_fail(edit("w=1 s2=1", "w=1 s2=0"), "S2 never parked", "both parked")
    # G
    must_fail(edit("FUTEX_TIME_W_IN_WINDOW_WAKE cpu=1", "FUTEX_TIME_W_IN_WINDOW_WAKE cpu=0"),
              "window wake from the same CPU", "another CPU")
    must_fail(edit("window=1 woke=0", "window=1 woke=1"), "in-window wake took the cycle", "took a cycle")
    must_fail(edit("competing_wakes=1 end=woke", "competing_wakes=0 end=bound"), "bound window", "bound")
    must_fail(drop("FUTEX_TIME_W_WINDOW_CLOSE"), "unclosed window", "never closed")
    must_fail(drop("FUTEX_TIME_W_WINDOW_SKIPPED"), "timeout without window decision", "window decision")
    i_open = next(i for i, l in enumerate(good) if "WINDOW_OPEN" in l)
    swapped = list(good)
    swapped[i_open], swapped[i_open + 1] = swapped[i_open + 1], swapped[i_open]
    must_fail(swapped, "in-window wake before the window opened", "outside OPEN..CLOSE")
    must_fail([l.replace("end=woke", "end=no_peer") for l in good], "no woken window", "G coverage")
    must_fail(edit("cycles=32 woken=16 timed_out=16", "cycles=32 woken=16 timed_out=15"),
              "admission expiry not counted", "the cell counted")
    # E
    must_fail(drop("FUTEX_TIMEOUT_EXPIRED_UNPLACED"), "E without saturation", "not full")
    must_fail(edit("err=SchedulerFull", "err=WrongObject"), "spawn refused for another reason", "SchedulerFull refusals only")
    must_fail(drop("SATW_REFUSED class=futex"), "no home-queue proof", "no refusal proves W's home queue")
    must_fail(edit("queued=64 reserved=0 cap=64 full=1", "queued=63 reserved=0 cap=64 full=0"), "room left", "room left")
    must_fail(edit("queued=64 reserved=0 cap=64 full=1", "queued=63 reserved=0 cap=64 full=1"), "lying full", "inconsistent")
    must_fail(edit("class=futex cpu=1 home=0", "class=futex cpu=1 home=1"), "another CPU's queue", "home 1 != 0")
    must_fail([l.replace("facts=ok", "facts=lost") if l.startswith("SATW_REFUSED") else l for l in good],
              "facts lost", "facts lost")
    must_fail(edit_where(lambda l: l.startswith("SATW_REFUSED"), "full=1 deadline=", "full=1 deadline=7"),
              "refusal names another deadline", "!= registered")
    i_sat = next(i for i, l in enumerate(good) if l.startswith("SATW_REFUSED class=futex"))
    i_seen = next(i for i, l in enumerate(good) if "FTW_E_EXPIRED_SEEN" in l)
    late = [l for k, l in enumerate(good) if k != i_sat]
    late.insert(i_seen, good[i_sat])
    must_fail(late, "refusal after the hold", "outside REGISTER..EXPIRED")
    must_fail(edit("fillers=129 topups=1", "fillers=130 topups=1"), "cell overcounts its fillers", "the kernel spawned")
    must_fail(edit("topups=1 spawn_refused=2", "topups=1 spawn_refused=3"), "refusal count", "spawn refusals")
    must_fail(edit("wake1=0", "wake1=1"), "E wake found the expired waiter", "E: {")
    # calls
    must_fail(drop("msg=FTW_RET role=G"), "a G call never returned", "never returned")
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch", choices=sorted(ARCHES))
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
