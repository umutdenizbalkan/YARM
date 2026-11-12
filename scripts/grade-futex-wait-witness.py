#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-WAIT-ATOMIC1 — grade one live FutexWait witness boot.

Input: the normalized serial log of a boot built with `futex-wait-witness` (kernel and init) and
booted with `yarm.futex_requeue_witness=1`. Init's waiter thread issues FutexWait(WORD, v) for
CALLS cycles; its signaller thread, on the other CPU, issues FutexWake(WORD, 1) in a loop, storing
a new value first on even cycles. The kernel holds each witness wait open twice, until a wake that
STARTED inside the window has finished:

  B  after the wait decided to park, before its compare-and-register (the old gap's position)
  A  after the registration, before the compare-and-clear of its current slot

Causal order on one console: ..._WINDOW_OPEN, the waker's ..._IN_WINDOW_WAKE, ..._WINDOW_CLOSE.
The grader requires, per window: an in-window wake from ANOTHER CPU between OPEN and CLOSE, and
competing_wakes >= 1. Then:

  B   every in-window wake woke nobody (no waiter was registered yet);
      the word B's close read differs from `expected`  -> the wait registered NOTHING: its result is
                                                          value_changed and it has no A window
      the word is unchanged                            -> the wait registered: it has an A window
  A   every in-window wake took the waiter (woke=1), and the wait's result is
      woken_during_publication: the registered waiter was reached, and its read-back recovered it.

The first B rule is what the old separated protocol breaks: it compared before B and registered
after it without comparing again, so a B that saw a store could still register and be taken in A.

A wait the kernel logs FUTEX_WAIT_WITNESS_SKIPPED (no sibling of the waiter was current on another
CPU, so no competing wake could have arrived) opens no window and is not graded by the window
rules; its outcome still counts below.

Userspace: FWAW_DONE result=ok; every cycle 1..CALLS returned exactly once; the number of calls that
reported `blocked=1` equals the number of waits the kernel registered (woken or parked).
Coverage: at least two B windows that saw a store, and at least two A windows. Plus the standard
core smoke, and no FUTEX_WAIT_BROAD_ENTRY (NR 9 never reached the broad dispatcher).

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

FATAL = (
    "YARM_AARCH64_TRAP_HANDLE halting",
    "RISCV_TRAP_HANDLE_FAILED",
    "x86 shared trap dispatch failed",
    "FUTEX_WAKE_RESERVED_COMMIT_REFUSED",
    "FUTEX_WAIT_BROAD_ENTRY",
    "panicked at",
    "KERNEL PANIC",
)
MIN_B_CHANGED = 2
MIN_A = 2


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s,]+)", line))


def grade(lines, smp):
    fails = []
    fail = fails.append
    opens, closes, wakes, results, skipped = {}, {}, [], {}, set()
    calls, done = [], None
    for i, l in enumerate(lines):
        for f in FATAL:
            if f in l:
                fail(f"fatal marker: {l.strip()[:160]}")
        if "FUTEX_WAIT_WITNESS_WINDOW_OPEN" in l:
            d = kv(l); d["at"] = i; opens[d["window"]] = d
        elif "FUTEX_WAIT_WITNESS_WINDOW_CLOSE" in l:
            d = kv(l); d["at"] = i; closes[d["window"]] = d
        elif "FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE" in l:
            d = kv(l); d["at"] = i; wakes.append(d)
        elif "FUTEX_WAIT_WITNESS_SKIPPED" in l:
            skipped.add(kv(l)["wait"])
        elif "FUTEX_WAIT_WITNESS_RESULT" in l:
            d = kv(l); results[d["wait"]] = d
        elif "FWAW_CALL" in l:
            calls.append(kv(l))
        elif "FWAW_DONE" in l:
            done = kv(l)
    facts = {}
    if done is None:
        fail("no FWAW_DONE: the cell never finished")
        done = {}
    elif done.get("result") != "ok":
        fail(f"FWAW_DONE result={done.get('result')}")
    n = int(done.get("calls", "0") or 0)
    cycles = [int(c["cycle"]) for c in calls if "cycle" in c]
    if sorted(cycles) != list(range(1, n + 1)):
        fail(f"every cycle 1..{n} must return exactly once (saw {sorted(cycles)})")
    by_wait = {}
    for wid, o in opens.items():
        if o["wait"] in skipped:
            fail(f"wait {o['wait']} was skipped but opened window {wid}")
        by_wait.setdefault(o["wait"], {})[o["kind"]] = wid
        c = closes.get(wid)
        if c is None:
            fail(f"window {wid} never closed")
            continue
        inside = [w for w in wakes if w["window"] == wid and o["at"] < w["at"] < c["at"]]
        if any(w["window"] == wid and not (o["at"] < w["at"] < c["at"]) for w in wakes):
            fail(f"window {wid}: an in-window wake is logged outside OPEN..CLOSE")
        foreign = [w for w in inside if w["cpu"] != o["cpu"]]
        if not foreign or int(c.get("competing_wakes", "0")) < 1:
            fail(f"window {wid} ({o['kind']}) held no wake from another CPU")
        if o["kind"] == "B" and any(w["woke"] != "0" for w in inside):
            fail(f"window {wid} (B): a wake took a waiter that was not registered yet: {inside}")
        if o["kind"] == "A" and any(w["woke"] != "1" for w in inside):
            fail(f"window {wid} (A): a wake missed the registered waiter: {inside}")
    b_changed = a_windows = 0
    for wait, kinds in sorted(by_wait.items(), key=lambda kv_: int(kv_[0])):
        r = results.get(wait, {}).get("outcome")
        if "B" not in kinds:
            fail(f"wait {wait}: an A window without its B window")
            continue
        c = closes.get(kinds["B"], {})
        changed = c.get("word") != c.get("expected")
        if changed:
            b_changed += 1
            if "A" in kinds:
                fail(f"wait {wait}: registered although its word had changed before the "
                     f"compare-and-register (B saw {c.get('word')} for {c.get('expected')})")
            if r != "value_changed":
                fail(f"wait {wait}: word changed before its registration, result {r}")
        else:
            if "A" not in kinds:
                fail(f"wait {wait}: an unchanged word, but no registration window (result {r})")
            if r != "woken_during_publication":
                fail(f"wait {wait}: registered and woken inside A, result {r}")
        if "A" in kinds:
            a_windows += 1
    registered = sum(1 for r in results.values()
                     if r.get("outcome") in ("woken_during_publication", "parked"))
    if done and int(done.get("blocked", "-1")) != registered:
        fail(f"userspace saw {done.get('blocked')} blocking calls, the kernel registered {registered}")
    if b_changed < MIN_B_CHANGED:
        fail(f"coverage: {b_changed} B windows saw a store (need {MIN_B_CHANGED})")
    if a_windows < MIN_A:
        fail(f"coverage: {a_windows} A windows (need {MIN_A})")
    wcpu = {o["cpu"] for o in opens.values()}
    kcpu = {w["cpu"] for w in wakes}
    facts.update(waits=len(by_wait), skipped=len(skipped), windows=len(opens), b_changed=b_changed, a=a_windows,
                 in_window_wakes=len(wakes), waiter_cpus=",".join(sorted(wcpu)) or "none",
                 waker_cpus=",".join(sorted(kcpu)) or "none", calls=n,
                 returns=done.get("returns", "?"), blocked=done.get("blocked", "?"))
    if smp > 1 and opens and not (kcpu - wcpu or wcpu - kcpu):
        fail("every window and every waker on one CPU")
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
    M = "0xf7a10000"
    def wait(w, b_word, a, result, a_woke="1", b_woke="0", wcpu="0", kcpu="1"):
        base = 10 * w
        out = [f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait={w} window={base} cpu={wcpu} tid=7",
               f"FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu={kcpu} tid=8 window={base} woke={b_woke}",
               f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=B wait={w} window={base} cpu={wcpu} "
               f"competing_wakes=1 expected={M} word={b_word}"]
        if a:
            out += [f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=A wait={w} window={base + 1} cpu={wcpu} tid=7",
                    f"FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu={kcpu} tid=8 window={base + 1} woke={a_woke}",
                    f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=A wait={w} window={base + 1} cpu={wcpu} "
                    f"competing_wakes=1"]
        out.append(f"FUTEX_WAIT_WITNESS_RESULT wait={w} tid=7 outcome={result}")
        return out
    good = (wait(1, "0xf7a10001", False, "value_changed") + wait(2, M, True, "woken_during_publication")
            + wait(3, "0xf7a10002", False, "value_changed") + wait(4, M, True, "woken_during_publication"))
    good += [f"FWAW_CALL cycle={c} expected={M} blocked={1 if c in (2, 4) else 0}" for c in range(1, 5)]
    good += ["FWAW_DONE waiter=7 signaller=8 calls=4 returns=4 blocked=2 errors=0 result=ok"]
    f, facts = grade(good, 2)
    assert not f, f
    # The old separated protocol: a B that saw a store still registers and is taken in A.
    old = (wait(1, "0xf7a10001", True, "woken_during_publication") + good[len(wait(1, "", False, "")):])
    old = [l.replace("blocked=2 errors", "blocked=3 errors") for l in old]
    assert any("registered although" in x for x in grade(old, 2)[0]), grade(old, 2)
    took_b = [l.replace("window=10 woke=0", "window=10 woke=1") for l in good]
    assert grade(took_b, 2)[0], "a B wake that took a waiter must fail"
    missed_a = [l.replace("window=21 woke=1", "window=21 woke=0") for l in good]
    assert grade(missed_a, 2)[0], "an A wake that missed the registered waiter must fail"
    same = [l.replace("IN_WINDOW_WAKE cpu=1", "IN_WINDOW_WAKE cpu=0") for l in good]
    assert grade(same, 2)[0], "a same-CPU wake is not a race"
    dup = good + ["FWAW_CALL cycle=2 expected=0xf7a10000 blocked=1"]
    assert grade(dup, 2)[0], "a cycle that returns twice must fail"
    thin = [l for l in good if "wait=4" not in l and "window=4" not in l]
    assert grade(thin, 2)[0], "coverage below the minimum must fail"
    # A skipped wait opens no window and is not window-graded; its outcome still counts.
    skip = good + ["FUTEX_WAIT_WITNESS_SKIPPED wait=5 cpu=0 tid=7 reason=no_peer_running",
                   "FUTEX_WAIT_WITNESS_RESULT wait=5 tid=7 outcome=parked",
                   f"FWAW_CALL cycle=5 expected={M} blocked=1"]
    skip = [l.replace("calls=4 returns=4 blocked=2", "calls=5 returns=5 blocked=3") for l in skip]
    f, facts = grade(skip, 2)
    assert not f and facts["skipped"] == 1, (f, facts)
    leaky = skip + ["FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait=5 window=50 cpu=0 tid=7"]
    assert grade(leaky, 2)[0], "a skipped wait that opens a window must fail"
    broad = good + ["FUTEX_WAIT_BROAD_ENTRY nr=9 tid=7 result=broad_entry"]
    assert grade(broad, 2)[0], "a broad NR 9 must fail"
    print("[futex-wait-grade] self-test ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--arch", default="?")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--core-rc", type=int, default=None)
    ap.add_argument("--core-log", action="append", default=[])
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    with open(a.log, "rb") as fh:
        lines = [l.decode("utf-8", "replace").rstrip("\n") for l in fh]
    fails, facts = grade(lines, a.smp)
    core_ok, reasons = core_verdict(a.core_rc, a.core_log)
    for r in reasons:
        print(f"[futex-wait-grade][core] {r}")
    facts["witness"] = "fail" if fails else "ok"
    facts["core_smoke"] = "ok" if core_ok else f"rejected(rc={a.core_rc})"
    if not core_ok:
        fails.append("the standard core smoke rejected this boot (see [core] lines)")
    for f in fails:
        print(f"[futex-wait-grade][fail] {f}")
    extra = " ".join(f"{k}={v}" for k, v in facts.items())
    print(f"FUTEX_WAIT_WITNESS_SEAL arch={a.arch} smp={a.smp} {extra} result={'fail' if fails else 'ok'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
