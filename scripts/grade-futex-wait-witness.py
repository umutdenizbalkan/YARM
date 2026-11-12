#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-WAIT-ATOMIC1 / RISCV-PARK-RET1 — grade one live FutexWait witness boot.

Input: the normalized serial log of a boot built with `futex-wait-witness` (kernel and init) and
booted with `yarm.futex_requeue_witness=1`. Init's waiter thread issues FutexWait(WORD, v) for
CALLS cycles; two signaller threads issue FutexWake(WORD, 1) in a loop, storing a new value first
on even cycles, while init blocks. The kernel holds a witness wait open twice — but only while a
sibling of the waiter is current on another CPU — until a wake that STARTED inside the window has
finished:

  B  after the wait decided to park, before its compare-and-register (the old gap's position)
  A  after the registration, before the compare-and-clear of its current slot

A wait with no such sibling is logged FUTEX_WAIT_WITNESS_SKIPPED and runs unwindowed: it is a
GENUINE park whenever its word still matches (the waiter leaves the CPU and a later wake resumes it).

Markers fail closed. Every witness marker must be well formed (its prefix intact, every field
present and of its type); a damaged marker fails the boot rather than being skipped, because a
skipped OPEN would leave its window ungraded. Per window: exactly one OPEN, exactly one CLOSE naming
the same kind, wait and CPU, and every in-window wake between them; a CLOSE or a wake naming no OPEN
fails, and so does an OPEN that never closes. A window ends `end=woke` (a wake ran in it),
`end=no_peer` (an explicit REFUSAL: no sibling of the waiter stayed current on another CPU, so none
could come — counted as `refused`, held to no wake, and not counted for coverage) or `end=bound`
(the bound ran out with a sibling running: a failure). Then:

  B   every in-window wake woke nobody (no waiter was registered yet), and at least one ran on
      another CPU; the word B's close read differs from `expected` -> the wait registered NOTHING:
      its result is value_changed and it has no A window; the word is unchanged -> it has an A window
  A   every in-window wake took the waiter (woke=1); the result is woken_during_publication.

Return values (RISCV-PARK-RET1). Each userspace FWAW_CALL is the runtime's verdict on the register
file the kernel returned (`blocked=1` is Ok(true), `blocked=0` Ok(false), `error=..` an Err). The
kernel results logged since the previous call are that call's: none (the decision read returned 0),
or exactly one. A call with two is a repeated `ecall`; one whose result names another task is a
wrong owner. Then value_changed and a decision-read return must be Ok(false); woken_during_publication
must be Ok(true); a GENUINE park (`parked`) must be Ok(true) — an Err (the defect: a parked wait
resumed with its address in a0) or Ok(false) fails. Ok(true) with no kernel wait is a missing
completion. Counted: mismatch, during_publication, parked, parked_ok, parked_err, dup_or_missing.

Coverage: at least two B windows that saw a store, two A windows, and two parked waits that
returned Ok(true). FWAW_DONE result=ok; every cycle 1..CALLS returned exactly once. Plus the
standard core smoke, and no FUTEX_WAIT_BROAD_ENTRY (NR 9 never reached the broad dispatcher).

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
MIN_PARKED_OK = 2

INT = r"\d+"
HEX = r"0x[0-9a-f]+"
# kind -> (canonical line pattern, required fields and their patterns)
SPEC = {
    "OPEN": ("FUTEX_WAIT_WITNESS_WINDOW_OPEN",
             {"kind": "[AB]", "wait": INT, "window": INT, "cpu": INT, "tid": INT}),
    "CLOSE": ("FUTEX_WAIT_WITNESS_WINDOW_CLOSE",
              {"kind": "[AB]", "wait": INT, "window": INT, "cpu": INT, "competing_wakes": INT,
               "end": "woke|bound|no_peer"}),
    "WAKE": ("FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE",
             {"cpu": INT, "tid": INT, "window": INT, "woke": INT}),
    "RESULT": ("FUTEX_WAIT_WITNESS_RESULT",
               {"wait": INT, "tid": INT,
                "outcome": "parked|woken_during_publication|value_changed|word_unreadable|"
                           "victim_changed|incarnation_moved|capacity_exhausted"}),
    "SKIPPED": ("FUTEX_WAIT_WITNESS_SKIPPED",
                {"wait": INT, "cpu": INT, "tid": INT, "reason": r"[a-z_]+"}),
}
# Fragments that make a line a witness marker even when its prefix is damaged.
# (Other witnesses' markers, e.g. FORK_PROOF_COW_WITNESS_SKIPPED, are not this one's.)
FRAGMENTS = ("WINDOW_OPEN", "WINDOW_CLOSE", "IN_WINDOW_WAKE", "WITNESS_RESULT wait=",
             "WITNESS_SKIPPED wait=", "_WAIT_WITNESS", "WAIT_WI", "FWAW_CALL", "FWAW_DONE")


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s,]+)", line))


def parse_marker(line):
    """(kind, fields) for a well-formed witness marker; (None, None) for a non-marker line;
    ('DAMAGED', reason) for a line that is a marker but not a well-formed one."""
    for kind, (head, fields) in SPEC.items():
        if re.match(rf"^{head} ", line):
            d = kv(line)
            for k, pat in fields.items():
                if k not in d or not re.fullmatch(pat, d[k]):
                    return "DAMAGED", f"{head}: field `{k}` missing or malformed"
            if kind == "CLOSE" and d["kind"] == "B":
                for k in ("expected", "word"):
                    if k not in d or not re.fullmatch(HEX, d[k]):
                        return "DAMAGED", f"{head}: field `{k}` missing or malformed"
            return kind, d
    if "msg=FWAW_CALL " in line and re.match(r"^USER_LOG tid=\d+ msg=FWAW_CALL ", line):
        d = kv(line)
        ok = re.fullmatch(INT, d.get("cycle", "")) and re.fullmatch(HEX, d.get("expected", "")) \
            and (re.fullmatch("[01]", d.get("blocked", "")) or "error" in d)
        return ("CALL", d) if ok else ("DAMAGED", "FWAW_CALL: malformed")
    if "msg=FWAW_DONE " in line and re.match(r"^USER_LOG tid=\d+ msg=FWAW_DONE ", line):
        return "DONE", kv(line)
    if "msg=FWAW_BEGIN " in line:
        return None, None
    if any(f in line for f in FRAGMENTS):
        return "DAMAGED", "damaged witness marker"
    return None, None


def grade(lines, smp):
    fails = []
    fail = fails.append
    opens, closes, wakes, results, skipped = {}, {}, [], {}, set()
    calls, done = [], None
    events = []  # ordered RESULT / CALL events for the return correlation
    for i, l in enumerate(lines):
        for f in FATAL:
            if f in l:
                fail(f"fatal marker: {l.strip()[:160]}")
        kind, d = parse_marker(l)
        if kind is None:
            continue
        if kind == "DAMAGED":
            fail(f"line {i + 1}: {d}: {l.strip()[:120]}")
            continue
        if kind != "DONE":
            d["at"] = i
        if kind == "OPEN":
            if d["window"] in opens:
                fail(f"window {d['window']}: opened twice")
            opens[d["window"]] = d
        elif kind == "CLOSE":
            if d["window"] in closes:
                fail(f"window {d['window']}: closed twice")
            closes[d["window"]] = d
        elif kind == "WAKE":
            wakes.append(d)
        elif kind == "SKIPPED":
            skipped.add(d["wait"])
        elif kind == "RESULT":
            if d["wait"] in results:
                fail(f"wait {d['wait']}: two results")
            results[d["wait"]] = d
            events.append(("RESULT", d))
        elif kind == "CALL":
            calls.append(d)
            events.append(("CALL", d))
        elif kind == "DONE":
            done = d
    facts = {}
    if done is None:
        fail("no FWAW_DONE: the cell never finished")
        done = {}
    elif done.get("result") != "ok":
        fail(f"FWAW_DONE result={done.get('result')}")
    n = int(done.get("calls", "0") or 0)
    cycles = [int(c["cycle"]) for c in calls]
    if sorted(cycles) != list(range(1, n + 1)):
        fail(f"every cycle 1..{n} must return exactly once (saw {sorted(cycles)})")

    # ── windows: identity and causal order, fail closed ────────────────────────────────────
    for wid, c in closes.items():
        if wid not in opens:
            fail(f"window {wid}: CLOSE without OPEN")
    for w in wakes:
        if w["window"] not in opens:
            fail(f"window {w['window']}: in-window wake without OPEN")
    by_wait, refused = {}, set()
    for wid, o in opens.items():
        if o["wait"] in skipped:
            fail(f"wait {o['wait']} was skipped but opened window {wid}")
        if o["kind"] in by_wait.get(o["wait"], {}):
            fail(f"wait {o['wait']}: two {o['kind']} windows")
        by_wait.setdefault(o["wait"], {})[o["kind"]] = wid
        c = closes.get(wid)
        if c is None:
            fail(f"window {wid}: opened and never closed")
            continue
        if (c["kind"], c["wait"], c["cpu"]) != (o["kind"], o["wait"], o["cpu"]):
            fail(f"window {wid}: CLOSE names {c['kind']}/wait {c['wait']}/cpu {c['cpu']}, "
                 f"OPEN {o['kind']}/wait {o['wait']}/cpu {o['cpu']}")
        if not o["at"] < c["at"]:
            fail(f"window {wid}: CLOSE before OPEN")
        if c["end"] == "bound":
            fail(f"window {wid} ({o['kind']}): ran out its bound with a sibling running")
        if c["end"] == "no_peer":
            refused.add(wid)
        inside = [w for w in wakes if w["window"] == wid and o["at"] < w["at"] < c["at"]]
        if any(w["window"] == wid and not (o["at"] < w["at"] < c["at"]) for w in wakes):
            fail(f"window {wid}: an in-window wake is logged outside OPEN..CLOSE")
        foreign = [w for w in inside if w["cpu"] != o["cpu"]]
        if c["end"] == "woke" and (not foreign or int(c["competing_wakes"]) < 1):
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
        b_refused = kinds["B"] in refused
        a_refused = kinds.get("A") in refused
        changed = c.get("word") != c.get("expected")
        if changed:
            b_changed += 0 if b_refused else 1
            if "A" in kinds:
                fail(f"wait {wait}: registered although its word had changed before the "
                     f"compare-and-register (B saw {c.get('word')} for {c.get('expected')})")
            if r != "value_changed":
                fail(f"wait {wait}: word changed before its registration, result {r}")
        elif "A" not in kinds:
            # No wake ran in a refused B, so a store may land after it and before the hold.
            if not (b_refused and r == "value_changed"):
                fail(f"wait {wait}: an unchanged word, but no registration window (result {r})")
        elif a_refused:
            # No wake ran in a refused A: the registered waiter parks, or a later wake reaches it
            # before the read-back.
            if r not in ("woken_during_publication", "parked"):
                fail(f"wait {wait}: registered, A refused, result {r}")
        elif r != "woken_during_publication":
            fail(f"wait {wait}: registered and woken inside A, result {r}")
        if "A" in kinds and not a_refused:
            a_windows += 1

    # ── return values: each call against the kernel waits since the previous one ───────────
    waiter = done.get("waiter")
    counts = dict(mismatch=0, during_publication=0, parked=0, parked_ok=0, parked_err=0,
                  dup_or_missing=0)
    pending = []
    for kind, d in events:
        if kind == "RESULT":
            pending.append(d)
            continue
        c = d
        if waiter and c.get("tid") != waiter:
            fail(f"cycle {c['cycle']}: logged by tid {c.get('tid')}, the waiter is {waiter}")
        got = "err" if "error" in c else ("ok_true" if c["blocked"] == "1" else "ok_false")
        shown = f"Err({c['error']})" if got == "err" else ("Ok(true)" if got == "ok_true" else "Ok(false)")
        if len(pending) > 1:
            counts["dup_or_missing"] += 1
            fail(f"cycle {c['cycle']}: {len(pending)} kernel waits for one call (a repeated ecall)")
        r = pending[0] if pending else None
        pending = []
        if r is not None and waiter and r["tid"] != waiter:
            fail(f"cycle {c['cycle']}: kernel wait {r['wait']} names tid {r['tid']}, not the waiter")
        outcome = r["outcome"] if r else "decision_read"
        if outcome in ("decision_read", "value_changed"):
            if got == "ok_false":
                counts["mismatch"] += 1
            elif got == "ok_true" and r is None:
                counts["dup_or_missing"] += 1
                fail(f"cycle {c['cycle']}: Ok(true) with no kernel wait (a missing completion)")
            else:
                fail(f"cycle {c['cycle']}: {outcome} returned {shown}, not Ok(false)")
        elif outcome == "woken_during_publication":
            if got == "ok_true":
                counts["during_publication"] += 1
            else:
                fail(f"cycle {c['cycle']}: woken during publication returned {shown}, not Ok(true)")
        elif outcome == "parked":
            counts["parked"] += 1
            if got == "ok_true":
                counts["parked_ok"] += 1
            else:
                counts["parked_err"] += 1
                fail(f"cycle {c['cycle']}: a GENUINELY parked wait (kernel wait {r['wait']}) "
                     f"returned {shown}, not Ok(true)")
        else:
            fail(f"cycle {c['cycle']}: kernel outcome {outcome}")
    if pending:
        counts["dup_or_missing"] += 1
        fail(f"{len(pending)} kernel wait(s) after the last call")

    if b_changed < MIN_B_CHANGED:
        fail(f"coverage: {b_changed} B windows saw a store (need {MIN_B_CHANGED})")
    if a_windows < MIN_A:
        fail(f"coverage: {a_windows} A windows (need {MIN_A})")
    if counts["parked_ok"] < MIN_PARKED_OK:
        fail(f"coverage: {counts['parked_ok']} genuinely parked waits returned Ok(true) "
             f"(need {MIN_PARKED_OK})")
    wcpu = {o["cpu"] for o in opens.values()}
    kcpu = {w["cpu"] for w in wakes}
    facts.update(waits=len(by_wait), skipped=len(skipped), windows=len(opens), refused=len(refused),
                 b_changed=b_changed,
                 a=a_windows, in_window_wakes=len(wakes), waiter_cpus=",".join(sorted(wcpu)) or "none",
                 waker_cpus=",".join(sorted(kcpu)) or "none", calls=n,
                 returns=done.get("returns", "?"), blocked=done.get("blocked", "?"), **counts)
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
    W = "10008"

    def call(c, got):
        tail = {"t": "blocked=1", "f": "blocked=0"}.get(got, f"error={got}")
        return f"USER_LOG tid={W} msg=FWAW_CALL cycle={c} expected={M} {tail}"

    def wait(w, b_word, a, result, a_woke="1", b_woke="0", wcpu="0", kcpu="1", end="woke"):
        base = 10 * w
        out = [f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait={w} window={base} cpu={wcpu} tid={W}",
               f"FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu={kcpu} tid=8 window={base} woke={b_woke}",
               f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=B wait={w} window={base} cpu={wcpu} "
               f"competing_wakes=1 end={end} expected={M} word={b_word}"]
        if a:
            out += [f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=A wait={w} window={base + 1} cpu={wcpu} tid={W}",
                    f"FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu={kcpu} tid=8 window={base + 1} woke={a_woke}",
                    f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=A wait={w} window={base + 1} cpu={wcpu} "
                    f"competing_wakes=1 end=woke"]
        out.append(f"FUTEX_WAIT_WITNESS_RESULT wait={w} tid={W} outcome={result}")
        return out

    def parked(w):
        return [f"FUTEX_WAIT_WITNESS_SKIPPED wait={w} cpu=0 tid={W} reason=no_peer_running",
                f"FUTEX_WAIT_WITNESS_RESULT wait={w} tid={W} outcome=parked"]

    def done(calls, blocked, errors=0, result="ok"):
        return (f"USER_LOG tid=1 msg=FWAW_DONE waiter={W} signallers=8,9 calls={calls} "
                f"returns={calls} blocked={blocked} errors={errors} result={result}")

    def boot(cycles):
        out = [f"USER_LOG tid=1 msg=FWAW_BEGIN s_tid=1 calls={len(cycles)}"]
        for c, (kernel, got) in enumerate(cycles, 1):
            out += kernel + [call(c, got)]
        blocked = sum(1 for _, g in cycles if g == "t")
        errors = sum(1 for _, g in cycles if g not in ("t", "f"))
        return out + [done(len(cycles), blocked, errors, "ok" if not errors else "fail")]

    cycles = [(wait(1, "0xf7a10001", False, "value_changed"), "f"),
              (wait(2, M, True, "woken_during_publication"), "t"),
              (wait(3, "0xf7a10002", False, "value_changed"), "f"),
              (wait(4, M, True, "woken_during_publication"), "t"),
              (parked(5), "t"),
              ([], "f"),  # the decision read returned 0
              (parked(6), "t")]
    good = boot(cycles)

    def fails(lines):
        return grade(lines, 2)[0]

    f, facts = grade(good, 2)
    assert not f, f
    assert (facts["parked"], facts["parked_ok"], facts["mismatch"], facts["during_publication"]) == (2, 2, 3, 2), facts

    def must_fail(lines, why, needle=None):
        f = fails(lines)
        assert f, f"must fail: {why}"
        if needle:
            assert any(needle in x for x in f), (why, f)

    def without(lines, needle):
        return [l for l in lines if needle not in l]

    def edit(lines, old, new):
        assert any(old in l for l in lines), old
        return [l.replace(old, new) for l in lines]

    # ── marker integrity ──
    must_fail(without(good, "WINDOW_OPEN kind=B wait=2 "), "missing OPEN", "CLOSE without OPEN")
    must_fail(edit(good, "FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait=2", "FUTEX_WAIT_WI3TNESS_WINDOW_OPEN kind=B wait=2"),
              "corrupted OPEN prefix", "damaged witness marker")
    must_fail(edit(good, "WINDOW_OPEN kind=B wait=2 window=20 cpu=0", "WINDOW_OPEN kind=B wait=2 window=20 cpu=0x"),
              "corrupted OPEN field", "malformed")
    dup_open = good[:3] + [good[good.index(next(l for l in good if "WINDOW_OPEN kind=B wait=1 " in l))]] + good[3:]
    must_fail(dup_open, "duplicate OPEN", "opened twice")
    must_fail(good[:-1] + ["FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=B wait=9 window=90 cpu=0 competing_wakes=1 "
                           f"end=woke expected={M} word={M}", good[-1]], "CLOSE without OPEN", "CLOSE without OPEN")
    must_fail(good[:-1] + ["FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu=1 tid=8 window=99 woke=0", good[-1]],
              "in-window wake without OPEN", "in-window wake without OPEN")
    must_fail(edit(good, "WINDOW_CLOSE kind=A wait=2 window=21", "WINDOW_CLOSE kind=A wait=4 window=21"),
              "wrong window identity", "CLOSE names")
    incomplete = good[:-1] + ["FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait=7 window=70 cpu=0 tid=10008", good[-1]]
    must_fail(incomplete, "incomplete final window", "never closed")
    must_fail(edit(good, "window=40 cpu=0 competing_wakes=1 end=woke", "window=40 cpu=0 competing_wakes=1 end=bound"),
              "a window that ran out its bound", "ran out its bound")
    # A refused window (no peer stayed current) is explicit, counted, and not coverage; its wait's
    # outcome is still graded.
    def refused_a(w, outcome="parked"):
        base = 10 * w
        return [f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait={w} window={base} cpu=0 tid={W}",
                f"FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu=1 tid=8 window={base} woke=0",
                f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=B wait={w} window={base} cpu=0 competing_wakes=1 end=woke expected={M} word={M}",
                f"FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=A wait={w} window={base + 1} cpu=0 tid={W}",
                f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=A wait={w} window={base + 1} cpu=0 competing_wakes=0 end=no_peer",
                f"FUTEX_WAIT_WITNESS_RESULT wait={w} tid={W} outcome={outcome}"]
    f, facts = grade(boot(cycles + [(refused_a(8), "t")]), 2)
    assert not f and facts["refused"] == 1 and facts["a"] == 2 and facts["parked_ok"] == 3, (f, facts)
    must_fail(boot(cycles + [(refused_a(8), "Unknown")]), "a refused-A wait that parked and returned Err",
              "GENUINELY parked")
    must_fail(boot(cycles + [(refused_a(8, "value_changed"), "f")]),
              "a refused A whose wait was registered yet reports value_changed", "A refused")
    refused_b = [l.replace("window=20 cpu=0 competing_wakes=1 end=woke", "window=20 cpu=0 competing_wakes=0 end=no_peer")
                 for l in good if "window=20 woke" not in l]
    f, facts = grade(refused_b, 2)
    assert facts["refused"] == 1 and facts["b_changed"] == 2, facts
    # ── the window rules ──
    old = boot([(wait(1, "0xf7a10001", True, "woken_during_publication"), "t")] + cycles[1:])
    must_fail(old, "the old separated protocol", "registered although")
    must_fail(edit(good, "window=10 woke=0", "window=10 woke=1"), "a B wake that took a waiter")
    must_fail(edit(good, "window=21 woke=1", "window=21 woke=0"), "an A wake that missed the waiter")
    must_fail(edit(good, "IN_WINDOW_WAKE cpu=1", "IN_WINDOW_WAKE cpu=0"), "a same-CPU wake is not a race")
    leaky = good[:-1] + ["FUTEX_WAIT_WITNESS_WINDOW_OPEN kind=B wait=5 window=50 cpu=0 tid=10008",
                         "FUTEX_WAIT_WITNESS_IN_WINDOW_WAKE cpu=1 tid=8 window=50 woke=0",
                         f"FUTEX_WAIT_WITNESS_WINDOW_CLOSE kind=B wait=5 window=50 cpu=0 competing_wakes=1 end=woke expected={M} word={M}",
                         good[-1]]
    must_fail(leaky, "a skipped wait that opens a window", "was skipped")
    # ── return values ──
    pointer = boot(cycles[:4] + [(parked(5), "Unknown")] + cycles[5:])
    must_fail(pointer, "a parked wait resumed with its address in a0 (decoded as an error)", "GENUINELY parked")
    must_fail(boot(cycles[:4] + [(parked(5), "f")] + cycles[5:]), "a parked wait returning Ok(false)", "GENUINELY parked")
    must_fail(boot(cycles[:5] + [([], "t")] + cycles[6:]), "Ok(true) with no kernel wait", "missing completion")
    must_fail(boot(cycles[:4] + [(parked(5) + ["FUTEX_WAIT_WITNESS_SKIPPED wait=8 cpu=0 tid=10008 reason=no_peer_running",
                                               "FUTEX_WAIT_WITNESS_RESULT wait=8 tid=10008 outcome=parked"], "t")] + cycles[5:]),
              "two kernel waits for one call", "repeated ecall")
    must_fail(boot(cycles[:1] + [(wait(2, M, True, "woken_during_publication"), "f")] + cycles[2:]),
              "woken during publication returning Ok(false)", "not Ok(true)")
    must_fail(edit(good, "FUTEX_WAIT_WITNESS_RESULT wait=5 tid=10008", "FUTEX_WAIT_WITNESS_RESULT wait=5 tid=10009"),
              "a kernel wait of another task", "not the waiter")
    must_fail(boot(cycles[:4] + [([], "f")] + cycles[5:6]), "coverage below two parked waits", "genuinely parked waits")
    # ── the rest ──
    must_fail(good + [call(2, "t")], "a cycle that returns twice", "exactly once")
    must_fail(boot(cycles[1:2] + cycles[3:]), "coverage below two changed B windows", "B windows saw a store")
    must_fail(good + ["FUTEX_WAIT_BROAD_ENTRY nr=9 tid=7 result=broad_entry"], "a broad NR 9", "fatal marker")
    must_fail(good[:-1], "no FWAW_DONE", "never finished")
    other = good[:1] + ["USER_LOG tid=1 msg=FORK_PROOF_COW_WITNESS_SKIPPED reason=x selector=18"] + good[1:]
    assert not fails(other), "another witness's marker is not this one's"
    must_fail(edit(good, "FUTEX_WAIT_WITNESS_RESULT wait=6", "FUTEX_WA3IT_WITNESS_RESULT wait=6"),
              "a damaged RESULT", "damaged witness marker")
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
