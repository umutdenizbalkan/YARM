#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-REQUEUE1 — grade one live compare-and-requeue witness boot.

Input: the normalized serial log of a boot with `yarm.futex_requeue_witness=1`.

The cell's userspace lines are checked against the KERNEL's own markers, never by presence alone:

  per round k
    every waiter parked first        CRQ_WITNESS_ROUND round=k census=10 census_back=10
                                     (two wake-0 NR 17 calls: all ten moved to AUX and back)
    one broadcast, no herd           CRQ_WITNESS_ROUND ... woken=1 requeued=9 calls=1 fallback=0
                                     FUTEX_REQUEUE_SPLIT_DONE ... woken=1 requeued=9 capacity_limited=0
                                     broad_lock=0  (exactly one such kernel line per round, in order)
    every moved waiter released      CRQ_WITNESS_ROUND ... acquired=10k overlap=0
                                     (a moved waiter is blocked on the MUTEX key, so it acquires only
                                     through a mutex wake; mutex_wakes is reported, not graded)
  across the boot
    CRQ_WITNESS_DONE finished=10 result=ok, and the ten TIDs it names
    each named waiter committed real parks, on the CPUs FUTEX_REQUEUE_WITNESS_PARK reports
    every census line is woken=0 requeued=10 (none ever woke anyone)
    no FUTEX_REQUEUE_SPLIT_REFUSED, no fatal condition, the standard core smoke accepted the boot

On a multi-CPU boot the CPUs the waiters parked on are reported; `--require-cross-cpu` makes a
single-CPU population a failure instead of a recorded limitation.

`--race` (FUTEX-REQUEUE-ATOMIC1, a boot built with `futex-requeue-race-witness`): signaller threads
issue FutexWake(seq, 1) while each broadcast is in flight, and the kernel holds NR 17's window
(after its commit, before its placement) open until a wake that STARTED inside it has finished.
Per broadcast the grader then requires, in log order, FUTEX_REQUEUE_RACE_WINDOW_OPEN, at least one
FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE of that window from ANOTHER CPU, and the window's CLOSE with
competing_wakes >= 1 — and every in-window wake must have woken nobody (the wake portion was
already decided and unreachable; the old interval would have let it take the kept waiter). A
signaller wake before the commit may take a waiter legitimately, so a round is accounted as
woken 1 + requeued + sig_woke == 10.

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

W = 10
FATAL = (
    "YARM_AARCH64_TRAP_HANDLE halting",
    "RISCV_TRAP_HANDLE_FAILED",
    "x86 shared trap dispatch failed",
    "FUTEX_WAKE_RESERVED_COMMIT_REFUSED",
    "panicked at",
    "KERNEL PANIC",
)


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s,]+(?:,[^\s=]+)*)", line))


def race_checks(lines, broadcasts, fail):
    """The causal window checks. Returns facts."""
    opens, ins, closes = [], [], []
    for i, l in enumerate(lines):
        if "FUTEX_REQUEUE_RACE_WINDOW_OPEN" in l:
            opens.append((i, kv(l)))
        elif "FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE" in l:
            ins.append((i, kv(l)))
        elif "FUTEX_REQUEUE_RACE_WINDOW_CLOSE" in l:
            closes.append((i, kv(l)))
    if len(opens) != len(broadcasts):
        fail(f"race: {len(opens)} held windows for {len(broadcasts)} broadcasts")
    covered, foreign = 0, set()
    for oi, o in opens:
        wid = o.get("window")
        cl = [(i, c) for i, c in closes if c.get("window") == wid and i > oi]
        if not cl:
            fail(f"race: window {wid} never closed")
            continue
        ci, c = cl[0]
        inside = [(i, w) for i, w in ins if w.get("window") == wid]
        if any(not (oi < i < ci) for i, _ in inside):
            fail(f"race: window {wid}: an in-window wake is logged outside OPEN..CLOSE")
        others = [w for _, w in inside if w.get("cpu") != o.get("cpu")]
        if int(c.get("competing_wakes", "0")) >= 1 and others:
            covered += 1
            foreign |= {w.get("cpu") for w in others}
        else:
            fail(f"race: window {wid} held no wake from another CPU (competing_wakes="
                 f"{c.get('competing_wakes')}, spins={c.get('spins')})")
    for _, w in ins:
        if w.get("woke") != "0":
            fail(f"race: a wake INSIDE the window took a waiter the commit had decided: {w}")
    return {"windows": len(opens), "covered": covered, "in_window_wakes": len(ins),
            "competing_cpus": ",".join(sorted(foreign)) or "-"}


def grade(lines, rounds, smp, require_cross, race=False):
    fails = []
    fail = fails.append
    rounds_seen = [kv(l[l.find("CRQ_WITNESS_ROUND"):]) for l in lines if "CRQ_WITNESS_ROUND" in l]
    done = [kv(l[l.find("CRQ_WITNESS_DONE"):]) for l in lines if "CRQ_WITNESS_DONE" in l]
    kdone = [kv(l) for l in lines if "FUTEX_REQUEUE_SPLIT_DONE" in l]
    parks = [kv(l) for l in lines if "FUTEX_REQUEUE_WITNESS_PARK" in l]
    if not any("CRQ_WITNESS_BEGIN" in l for l in lines):
        fail("the cell never began (selector 18 not armed, or the feature not built)")
    if len(rounds_seen) != rounds:
        fail(f"expected {rounds} round lines, got {len(rounds_seen)}")
    sig = []
    for i, r in enumerate(rounds_seen, 1):
        s = int(r.get("sig_woke", "0")) if race else 0
        sig.append(s)
        want = {"round": str(i), "census": str(W), "census_back": str(W), "woken": "1",
                "requeued": str(W - 1 - s), "calls": "1", "fallback": "0", "acquired": str(W * i),
                "overlap": "0", "result": "ok"}
        for k, v in want.items():
            if r.get(k) != v:
                fail(f"round {i}: {k}={r.get(k)} (want {v})")
    if len(done) != 1:
        fail(f"expected one CRQ_WITNESS_DONE, got {len(done)}")
        tids = set()
    else:
        d = done[0]
        if d.get("result") != "ok" or d.get("finished") != str(W):
            fail(f"CRQ_WITNESS_DONE result={d.get('result')} finished={d.get('finished')}")
        tids = {t for t in d.get("tids", "").split(",") if t and t != "0"}
        if len(tids) != W:
            fail(f"expected {W} distinct waiter TIDs, got {sorted(tids)}")
    if race:
        broadcasts = [k for k in kdone if k.get("woken") == "1"]
    else:
        broadcasts = [k for k in kdone if k.get("woken") == "1" and k.get("requeued") == str(W - 1)]
    census = [k for k in kdone if k.get("woken") == "0"]
    other = [k for k in kdone if k not in broadcasts and k not in census]
    if len(broadcasts) != rounds:
        fail(f"kernel: {len(broadcasts)} broadcast lines woken=1 (want {rounds})")
    for k, s in zip(broadcasts, sig):
        if k.get("requeued") != str(W - 1 - s):
            fail(f"kernel: broadcast requeued={k.get('requeued')} (the cell accounts {W - 1 - s})")
    for k in kdone:
        if k.get("broad_lock") != "0":
            fail(f"kernel: a requeue was not served off the broad lock: {k}")
        if k.get("capacity_limited") != "0":
            fail(f"kernel: a capacity-limited requeue: {k}")
    if other:
        fail(f"kernel: unexpected requeue outcomes: {other}")
    if any(k.get("requeued") != str(W) for k in census):
        fail("kernel: a census call did not move exactly ten waiters (the cell retried; see its census_calls)")
    if any("FUTEX_REQUEUE_SPLIT_REFUSED" in l for l in lines):
        fail("kernel: a requeue was refused")
    for f in FATAL:
        if any(f in l for l in lines):
            fail(f"fatal marker: {f}")
    witness_parks = [p for p in parks if p.get("tid") in tids]
    park_cpus = sorted({p.get("cpu") for p in witness_parks})
    per_tid = {t: sorted({p.get("cpu") for p in witness_parks if p.get("tid") == t}) for t in tids}
    if tids and any(not v for v in per_tid.values()):
        fail(f"a waiter never committed a split park: {[t for t, v in per_tid.items() if not v]}")
    requeuer_cpus = sorted({k.get("cpu") for k in broadcasts})
    facts = {
        "rounds": len(rounds_seen),
        "broadcasts": len(broadcasts),
        "census_calls": len(census),
        "parks": len(witness_parks),
        "park_cpus": ",".join(park_cpus) or "-",
        "requeuer_cpus": ",".join(requeuer_cpus) or "-",
        "mutex_wakes": ",".join(r.get("mutex_wakes", "?") for r in rounds_seen),
    }
    if race:
        facts.update(race_checks(lines, broadcasts, fail))
        facts["sig_woke"] = ",".join(str(s) for s in sig)
    cross = len(set(park_cpus) | set(requeuer_cpus)) > 1
    facts["cross_cpu"] = "yes" if cross else ("n/a" if smp == 1 else "uncovered")
    if smp > 1 and not cross and require_cross:
        fail("multi-CPU boot, but every waiter and the broadcaster ran on one CPU")
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


def self_test():
    ok = ["CRQ_WITNESS_BEGIN s_tid=1 waiters=10 rounds=1",
          "FUTEX_REQUEUE_SPLIT_DONE arch=x cpu=0 tid=1 woken=0 requeued=10 capacity_limited=0 broad_lock=0",
          "FUTEX_REQUEUE_SPLIT_DONE arch=x cpu=0 tid=1 woken=1 requeued=9 capacity_limited=0 broad_lock=0",
          "CRQ_WITNESS_ROUND round=1 census=10 census_back=10 census_calls=1 woken=1 requeued=9 calls=1 "
          "fallback=0 acquired=10 mutex_wakes=9 overlap=0 result=ok",
          "CRQ_WITNESS_DONE waiters=10 rounds=1 finished=10 returns=10 tids=11,12,13,14,15,16,17,18,19,20 result=ok"]
    ok += [f"FUTEX_REQUEUE_WITNESS_PARK cpu={t % 2} tid={t} addr=0x1" for t in range(11, 21)]
    f, facts = grade(ok, 1, 2, True)
    assert not f and facts["cross_cpu"] == "yes", (f, facts)
    herd = [l.replace("woken=1 requeued=9", "woken=10 requeued=0") for l in ok]
    assert grade(herd, 1, 2, True)[0], "a herd must fail"
    lost = [l.replace("acquired=10", "acquired=9") for l in ok]
    assert grade(lost, 1, 2, True)[0], "a stranded moved waiter must fail"
    one = [l.replace("cpu=1", "cpu=0") for l in ok]
    assert grade(one, 1, 2, True)[0] and not grade(one, 1, 2, False)[0]
    # Race mode: a held window with a foreign in-window wake that woke nobody passes; one that took a
    # waiter, one with no foreign wake, and a missing window each fail.
    race = [l.replace("requeued=9 capacity_limited", "requeued=8 capacity_limited")
            .replace("requeued=9 calls=1", "requeued=8 sig_woke=1 calls=1") for l in ok]
    at = next(i for i, l in enumerate(race) if "woken=1 requeued=8 capacity" in l)
    win = ["FUTEX_REQUEUE_RACE_WINDOW_OPEN cpu=0 window=1 woken=1 requeued=8",
           "FUTEX_REQUEUE_RACE_IN_WINDOW_WAKE cpu=1 tid=30 window=1 woke=0",
           "FUTEX_REQUEUE_RACE_WINDOW_CLOSE cpu=0 window=1 competing_wakes=1 spins=7"]
    good = race[:at] + win + race[at:]
    f, facts = grade(good, 1, 2, True, race=True)
    assert not f and facts["covered"] == 1, (f, facts)
    took = [l.replace("woke=0", "woke=1") for l in good]
    assert grade(took, 1, 2, True, race=True)[0], "an in-window wake that took a waiter must fail"
    same = [l.replace("IN_WINDOW_WAKE cpu=1", "IN_WINDOW_WAKE cpu=0") for l in good]
    assert grade(same, 1, 2, True, race=True)[0], "a same-CPU wake is not a race"
    assert grade(race, 1, 2, True, race=True)[0], "no held window must fail"
    print("[futex-requeue-grade] self-test ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--arch", default="?")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--core-rc", type=int, default=None)
    ap.add_argument("--core-log", action="append", default=[])
    ap.add_argument("--require-cross-cpu", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--race", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    with open(a.log, "rb") as fh:
        lines = [l.decode("utf-8", "replace").rstrip("\n") for l in fh]
    fails, facts = grade(lines, a.rounds, a.smp, a.require_cross_cpu, race=a.race)
    core_ok, reasons = core_verdict(a.core_rc, a.core_log)
    for r in reasons:
        print(f"[futex-requeue-grade][core] {r}")
    facts["witness"] = "fail" if fails else "ok"
    facts["core_smoke"] = "ok" if core_ok else f"rejected(rc={a.core_rc})"
    if not core_ok:
        fails.append("the standard core smoke rejected this boot (see [core] lines)")
    for f in fails:
        print(f"[futex-requeue-grade][fail] {f}")
    extra = " ".join(f"{k}={v}" for k, v in facts.items())
    print(f"FUTEX_REQUEUE_WITNESS_SEAL arch={a.arch} smp={a.smp} {extra} result={'fail' if fails else 'ok'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
