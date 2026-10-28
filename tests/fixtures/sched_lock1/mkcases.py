#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SCHED-LOCK1-ACCEPTANCE — the SL1W grader corpus.

Writes one boot-log-shaped file per case and `cases.tsv` (`name<TAB>expect<TAB>obligation`):
`expect` is `pass` or `fail`, `obligation` the `reason=` prefix a failure must carry.

Independent of the grader: its own record writer and CRC, and base traces written out by hand
from the producer's timeline (`kernel::schedlock1_witness`):

* a hold begins inside the holder's tick (holder generation = that tick's acquisition);
* the observer's sections are released inside the hold; an installation is observed by `RUN`
  (and `PROGRESS`, during a hold) at the observer's next tick, a later acquisition;
* the holder's own sections come before its tick or after the hold;
* a claim follows the commit it names on the same executing CPU; its notification follows;
* every record appears twice, identically (the producer's `emit_twice`).

Usage: mkcases.py --out DIR
"""
import os
import sys
import zlib

C0, C1, OFF = 0x80001000, 0x80002000, 0x40
SPINS = 400_000_000


def inc(asid):
    return asid + 2


def rec(seq, kind, cpu, ex, f):
    body = "SL1W_R seq=%d kind=%s cpu=%d exec=%d f0=%d f1=%d f2=%d f3=%d f4=%d f5=%d" % (
        seq, kind, cpu, ex, *f)
    return "%s crc=%08x" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)


def render(events, ok, ncpu=2, phase=2, overflow=0, transform=None, census_ok=None):
    """`ok` is the producer's success count, carried by both CENSUS_END and SL1W_END (they are
    written from one counter); `census_ok` overrides CENSUS_END's alone."""
    events = [e if e[0] not in ("CENSUS_END", "CENSUS_BEGIN") else
              (e[0], e[1], e[2], [ok if census_ok is None else census_ok, 2] if e[0] == "CENSUS_END"
               else e[3][:4] + [ok if census_ok is None else census_ok] + e[3][5:]) + tuple(e[4:])
              for e in events]
    labels = {e[4]: i + 1 for i, e in enumerate(events) if len(e) > 4 and e[4]}
    lines = []
    for i, e in enumerate(events):
        f = [labels[x[1]] if isinstance(x, tuple) else x for x in e[3]]
        f += [0] * (6 - len(f))
        lines.append(rec(i + 1, e[0], e[1], e[2], f))
    endb = "SL1W_END count=%d overflow=%d phase=%d ncpu=%d rounds=%d rounds_ok=%d" % (
        len(events), overflow, phase, ncpu, 2 if ncpu == 2 else 2 * ncpu, ok)
    lines.append("%s crc=%08x" % (endb, zlib.crc32(endb.encode()) & 0xFFFFFFFF))
    if transform:
        lines = transform(lines)
    return "boot banner\n" + "\n".join(l + "\n" + l for l in lines) + "\nP_END\n"


def held(n1l=1, n1g=0):
    return n1l | (n1g << 32)


def hb(cpu, rnd, obs, inst, gen, attempt=0, lock=1, irq=0, locks=None):
    return ("HOLD_BEGIN", cpu, cpu, [rnd, obs, inst, gen, locks if locks is not None else held(),
                                     lock | (irq << 1) | (attempt << 8)])


def he(cpu, rnd, commits, prog, rel, spins, ok, gen, lock=1, irq=0):
    return ("HOLD_END", cpu, cpu, [rnd, commits, prog, rel, spins, lock | (ok << 1) | (irq << 2) | (gen << 8)])


def snap():
    return [("SNAP_BEGIN", 0, 0, [2, 3, 3, 10, 0, 0]),
            ("CELL", 0, 0, [C0, C0 + OFF, 0]), ("CELL", 1, 1, [C1, C1 + OFF, 0]),
            ("SNAP_CUR", 0, 0, [0, 5]), ("SNAP_CUR", 1, 1, [101, 7]), ("SNAP_Q", 1, 1, [300, 1])]


def census(cur0, q0, cur1, q1, ok, gen0, gen1):
    ev = [("CENSUS_BEGIN", 0, 0, [2, 3, 3, 40, ok, 2]),
          ("CELL", 0, 0, [C0, C0 + OFF, 1]), ("CELL", 1, 1, [C1, C1 + OFF, 1]),
          ("CENSUS_CUR", 0, 0, [cur0, gen0])]
    ev += [("CENSUS_Q", 0, 0, [t, 1]) for t in q0]
    ev += [("CENSUS_CUR", 1, 1, [cur1, gen1])]
    ev += [("CENSUS_Q", 1, 1, [t, 1]) for t in q1]
    return ev + [("CENSUS_END", 0, 0, [ok, 2])]


def round1_and_tail():
    """Round 1 (cpu 1 holds, cpu 0 selects 500, which runs, and queues 600), then cpu 1 selects
    the claimed 200, which runs in its claimed address space (ASID 9)."""
    return [("SEC", 0, 0, [7, 0, 0, 1, 0, C0]), ("ADD", 0, 0, [7, 500, 1]),
            hb(1, 1, 0, C1, 20),
            ("SEC", 0, 0, [8, 0, 501, 0, 1, C0], "i500"), ("REM", 0, 0, [8, 500]),
            ("SEC", 0, 0, [9, 501, 501, 1, 0, C0], "q600"), ("ADD", 0, 0, [9, 600, 1]),
            ("RUN", 0, 0, [500, inc(5), 8, 10, ("@", "i500")]),
            ("PROGRESS", 0, 0, [1, 500, 10, 8, inc(5), ("@", "i500")]),
            he(1, 1, 2, 1, 0, 70, 1, 20),
            ("SEC", 1, 1, [21, 301, 201, 1, 1, C1], "i200"), ("ADD", 1, 1, [21, 300, 1]),
            ("REM", 1, 1, [21, 200]),
            ("RUN", 1, 1, [200, inc(9), 21, 22, ("@", "i200")])]


def wake200(gen):
    """cpu 0 wakes 200 onto cpu 1: commit, claim (with the owner's staged ASID 9), notify."""
    return [("SEC", 1, 0, [gen, 301, 301, 1, 0, C1], "c200"), ("ADD", 1, 0, [gen, 200, 1]),
            ("CLAIM", 1, 0, [200, inc(9), 1, gen, ("@", "c200")]), ("NOTIFY", 1, 0, [gen])]


def healthy():
    """Round 0 (cpu 0 holds): cpu 1 requeues 100 and selects 300, which runs."""
    ev = snap()
    ev += [hb(0, 0, 1, C0, 6),
           ("SEC", 1, 1, [8, 101, 0, 1, 0, C1], "q100"), ("ADD", 1, 1, [8, 100, 1]),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, C1], "i300"), ("REM", 1, 1, [9, 300]),
           ("RUN", 1, 1, [300, inc(3), 9, 10, ("@", "i300")]),
           ("PROGRESS", 1, 1, [0, 300, 10, 9, inc(3), ("@", "i300")]),
           he(0, 0, 2, 1, 0, 50, 1, 6)]
    ev += wake200(11)
    ev += round1_and_tail()
    ev += census(501, [600], 201, [100, 300], 2, 10, 22)
    return ev


def retry_after_bound():
    """Round 0's first attempt reaches its bound with one queue-only commit (valid, uncredited);
    the second attempt (the holder's next tick) selects 300, observes it run, and withdraws 100
    (a queue-only commit) — credited."""
    ev = snap()
    ev += [hb(0, 0, 1, C0, 6, attempt=0),
           ("SEC", 1, 1, [8, 101, 0, 1, 0, C1]), ("ADD", 1, 1, [8, 100, 1]),
           he(0, 0, 1, 0, 0, SPINS, 0, 6),
           hb(0, 0, 1, C0, 7, attempt=1),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, C1], "i300"), ("REM", 1, 1, [9, 300]),
           ("RUN", 1, 1, [300, inc(3), 9, 10, ("@", "i300")]),
           ("PROGRESS", 1, 1, [0, 300, 10, 9, inc(3), ("@", "i300")]),
           ("SEC", 1, 1, [11, 301, 301, 0, 1, C1]), ("REM", 1, 1, [11, 100]),
           he(0, 0, 2, 1, 0, 9000, 1, 7)]
    ev += wake200(12)
    ev += [("SEC", 0, 0, [8, 0, 0, 1, 0, C0]), ("ADD", 0, 0, [8, 500, 1]),
           hb(1, 1, 0, C1, 20),
           ("SEC", 0, 0, [9, 0, 501, 0, 1, C0], "i500"), ("REM", 0, 0, [9, 500]),
           ("SEC", 0, 0, [10, 501, 501, 1, 0, C0]), ("ADD", 0, 0, [10, 600, 1]),
           ("RUN", 0, 0, [500, inc(5), 9, 11, ("@", "i500")]),
           ("PROGRESS", 0, 0, [1, 500, 11, 9, inc(5), ("@", "i500")]),
           he(1, 1, 2, 1, 0, 70, 1, 20),
           ("SEC", 1, 1, [21, 301, 201, 1, 1, C1], "i200"), ("ADD", 1, 1, [21, 300, 1]),
           ("REM", 1, 1, [21, 200]),
           ("RUN", 1, 1, [200, inc(9), 21, 22, ("@", "i200")])]
    ev += census(501, [600], 201, [300], 2, 11, 22)
    return ev


def retry_after_related():
    """Round 0's first attempt ends early: the observer needed the holder's lock (RELATED, naming
    the hold's round and attempt; its acquisition closed the hold) — valid, uncredited; the second
    attempt is healthy."""
    ev = snap()
    ev += [hb(0, 0, 1, C0, 6, attempt=0),
           ("RELATED", 0, 1, [0, 0]),
           he(0, 0, 0, 0, 1, 1200, 0, 6),
           hb(0, 0, 1, C0, 7, attempt=1),
           ("SEC", 1, 1, [8, 101, 0, 1, 0, C1]), ("ADD", 1, 1, [8, 100, 1]),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, C1], "i300"), ("REM", 1, 1, [9, 300]),
           ("RUN", 1, 1, [300, inc(3), 9, 10, ("@", "i300")]),
           ("PROGRESS", 1, 1, [0, 300, 10, 9, inc(3), ("@", "i300")]),
           he(0, 0, 2, 1, 0, 50, 1, 7)]
    ev += wake200(11)
    ev += [("SEC", 0, 0, [8, 0, 0, 1, 0, C0]), ("ADD", 0, 0, [8, 500, 1]),
           hb(1, 1, 0, C1, 20),
           ("SEC", 0, 0, [9, 0, 501, 0, 1, C0], "i500"), ("REM", 0, 0, [9, 500]),
           ("SEC", 0, 0, [10, 501, 501, 1, 0, C0]), ("ADD", 0, 0, [10, 600, 1]),
           ("RUN", 0, 0, [500, inc(5), 9, 11, ("@", "i500")]),
           ("PROGRESS", 0, 0, [1, 500, 11, 9, inc(5), ("@", "i500")]),
           he(1, 1, 2, 1, 0, 70, 1, 20),
           ("SEC", 1, 1, [21, 301, 201, 1, 1, C1], "i200"), ("ADD", 1, 1, [21, 300, 1]),
           ("REM", 1, 1, [21, 200]),
           ("RUN", 1, 1, [200, inc(9), 21, 22, ("@", "i200")])]
    ev += census(501, [600], 201, [100, 300], 2, 11, 22)
    return ev


def serialized():
    """Global serialization: while cpu 0 holds, cpu 1 cannot commit. All three round-0 attempts
    reach their bound with empty windows; cpu 1's commits land after each hold; round 1 is
    healthy. Valid throughout — but round 0 is never credited."""
    ev = snap()
    for k, g in enumerate((6, 7, 8)):
        ev += [hb(0, 0, 1, C0, g, attempt=k), he(0, 0, 0, 0, 0, SPINS, 0, g)]
    ev += [("SEC", 1, 1, [8, 101, 0, 1, 0, C1]), ("ADD", 1, 1, [8, 100, 1]),
           ("SEC", 1, 1, [9, 0, 301, 0, 1, C1], "i300"), ("REM", 1, 1, [9, 300]),
           ("RUN", 1, 1, [300, inc(3), 9, 10, ("@", "i300")])]
    ev += wake200(11)
    ev += [("SEC", 0, 0, [9, 0, 0, 1, 0, C0]), ("ADD", 0, 0, [9, 500, 1]),
           hb(1, 1, 0, C1, 20),
           ("SEC", 0, 0, [10, 0, 501, 0, 1, C0], "i500"), ("REM", 0, 0, [10, 500]),
           ("SEC", 0, 0, [11, 501, 501, 1, 0, C0]), ("ADD", 0, 0, [11, 600, 1]),
           ("RUN", 0, 0, [500, inc(5), 10, 12, ("@", "i500")]),
           ("PROGRESS", 0, 0, [1, 500, 12, 10, inc(5), ("@", "i500")]),
           he(1, 1, 2, 1, 0, 70, 1, 20),
           ("SEC", 1, 1, [21, 301, 201, 1, 1, C1], "i200"), ("ADD", 1, 1, [21, 300, 1]),
           ("REM", 1, 1, [21, 200]),
           ("RUN", 1, 1, [200, inc(9), 21, 22, ("@", "i200")])]
    ev += census(501, [600], 201, [100, 300], 1, 12, 22)
    return ev


# ── mutation helpers ───────────────────────────────────────────────────────────────────────────
def copy(ev):
    return [(e[0], e[1], e[2], list(e[3])) + tuple(e[4:]) for e in ev]


def idx(ev, kind, k=0, **where):
    hits = [i for i, e in enumerate(ev) if e[0] == kind and all(
        (e[3][int(key[1:])] if key.startswith("f") else e[{"cpu": 1, "ex": 2}[key]]) == v
        for key, v in where.items())]
    return hits[k]


def setf(ev, i, j, v):
    ev[i][3][j] = v


def lab(ev, label):
    return [i for i, e in enumerate(ev) if len(e) > 4 and e[4] == label][0]


def cases():
    H, R, X = healthy(), retry_after_bound(), retry_after_related()
    out = [("healthy", render(H, 2), "pass", "-"),
           ("valid uncredited attempt (bound) then credited retry", render(R, 2), "pass", "-"),
           ("valid uncredited attempt (interference) then credited retry", render(X, 2), "pass", "-")]

    def late_prog(ev):  # a progress record reserved just after its hold's HOLD_END: not counted
        ev.insert(idx(ev, "HOLD_END") + 1, ("PROGRESS", 1, 1, [0, 300, 11, 9, inc(3), ("@", "i300")]))
    ev = copy(H); late_prog(ev)
    out.append(("late observer progress (accepted)", render(ev, 2), "pass", "-"))

    def late_related(ev):  # the observer closed the hold, then reserved its RELATED after HOLD_END
        r = ev.pop(idx(ev, "RELATED"))
        ev.insert(idx(ev, "HOLD_END") + 1, r)
    ev = copy(X); late_related(ev)
    out.append(("RELATED reserved after its hold's HOLD_END (accepted)", render(ev, 2), "pass", "-"))

    def neg(name, base, fn, ob, ok=2, **kw):
        ev = copy(base)
        fn(ev)
        out.append((name, render(ev, ok, **kw), "fail", ob))

    # ── the four original reproductions ──────────────────────────────────────────────────────
    def a_third(ev):
        for i, e in enumerate(ev):
            if e[0] == "SEC":
                setf(ev, i, 5, 0x7E570000)
    neg("A every section names a third lock instance", H, a_third, "lock-instance:")
    neg("B claim ASID changed", H, lambda ev: setf(ev, idx(ev, "CLAIM"), 1, inc(1009)), "claim-incarnation:")

    def c_invalid(ev):  # held=0, two 1L, ok=0 — then the healthy attempt for the same round
        b = idx(ev, "HOLD_BEGIN")
        ev[b:b] = [hb(0, 0, 1, C0, 5, attempt=0, lock=0, locks=held(2)), he(0, 0, 0, 0, 0, SPINS, 0, 5)]
        setf(ev, b + 2, 5, 1 | (1 << 8))  # the healthy one is now attempt 1
    neg("C invalid uncredited attempt before a healthy one", H, c_invalid, "attempt:")

    def d_noop(ev):  # the observer's round-0 sections replaced by net-no-op ones
        i = idx(ev, "SEC", 0)  # q100
        ev[i:i + 4] = [("SEC", 1, 1, [8, 101, 101, 0, 0, C1]), ("SEC", 1, 1, [9, 101, 101, 0, 0, C1], "i300")]
        ev[idx(ev, "RUN")] = ("RUN", 1, 1, [100, inc(3), 9, 10, ("@", "i300")])
        ev[idx(ev, "PROGRESS")] = ("PROGRESS", 1, 1, [0, 100, 10, 9, inc(3), ("@", "i300")])
    neg("D no scheduling mutation (net-no-op sections)", H, d_noop, "no-op-section:")

    # ── lock ownership ───────────────────────────────────────────────────────────────────────
    neg("another CPU's valid lock instance on a section", H,
        lambda ev: setf(ev, idx(ev, "SEC", 0), 5, C0), "lock-instance:")
    neg("another CPU's valid lock instance on a credited hold", H,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN"), 2, C1), "lock-instance:")
    neg("another CPU's lock instance on an UNCREDITED attempt", R,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN"), 2, C1), "lock-instance:")
    neg("a CPU's cell changes inside the window", H,
        lambda ev: setf(ev, idx(ev, "CELL", 3), 0, C1 + 0x1000), "cell-map:")
    neg("two CPUs declare one cell", H,
        lambda ev: (setf(ev, idx(ev, "CELL", 1), 0, C0), setf(ev, idx(ev, "CELL", 3), 0, C0)), "cell-map:")
    neg("a CPU missing from the cell map", H, lambda ev: ev.pop(idx(ev, "CELL", 1)), "cell-map:")

    # ── claims: commit and incarnation ───────────────────────────────────────────────────────
    neg("claim with no staged incarnation", H, lambda ev: setf(ev, idx(ev, "CLAIM"), 1, 0), "claim-incarnation:")
    neg("claim carries a stale incarnation", H, lambda ev: setf(ev, idx(ev, "CLAIM"), 1, inc(8)),
        "claim-incarnation:")
    neg("claim names another commit", H, lambda ev: setf(ev, idx(ev, "CLAIM"), 4, ("@", "i300")), "claim-commit:")
    neg("claim names its commit with another generation", H,
        lambda ev: setf(ev, idx(ev, "CLAIM"), 3, 12), "claim-commit:")
    neg("claim substitutes the task", H, lambda ev: setf(ev, idx(ev, "CLAIM"), 0, 100), "claim-commit:")
    neg("claim substitutes the destination", H,
        lambda ev: ev.__setitem__(idx(ev, "CLAIM"), ("CLAIM", 0, 0, list(ev[idx(ev, "CLAIM")][3]))),
        "claim-commit:")

    def dup_claim(ev):
        i = idx(ev, "CLAIM")
        ev.insert(i + 1, (ev[i][0], ev[i][1], ev[i][2], list(ev[i][3])))
    neg("one commit claimed twice (a repeated claim)", H, dup_claim, "claim-commit:")

    def dropped_pub(ev):  # the owner reported a commit that never reached the queue: no section,
        # so the claim names no commit (LAST_ADD did not name this target); 200 never runs on cpu 1
        i = lab(ev, "c200")
        del ev[i:i + 2]
        c = idx(ev, "CLAIM")
        setf(ev, c, 3, 0)
        setf(ev, c, 4, 0)
        j = lab(ev, "i200")
        del ev[j:j + 4]
        ev[idx(ev, "CENSUS_CUR", 1)] = ("CENSUS_CUR", 1, 1, [301, 22])
        del ev[idx(ev, "CENSUS_Q", 2)]
    neg("dropped publication", H, dropped_pub, "claim-commit:")

    # ── progress and runs ────────────────────────────────────────────────────────────────────
    neg("progress linked to the wrong installation (a queue-only commit)", H,
        lambda ev: setf(ev, idx(ev, "PROGRESS"), 5, ("@", "q100")), "progress:")
    neg("progress linked to another CPU's installation", H,
        lambda ev: (setf(ev, idx(ev, "PROGRESS"), 5, ("@", "i500")), setf(ev, idx(ev, "PROGRESS"), 3, 8)),
        "progress:")
    neg("progress names an installation from before its hold", H,
        lambda ev: setf(ev, idx(ev, "PROGRESS", 1), 5, ("@", "q600")), "progress:")
    neg("progress reports another incarnation than its run", H,
        lambda ev: setf(ev, idx(ev, "PROGRESS"), 4, inc(4)), "progress:")
    neg("run names no installation", H, lambda ev: setf(ev, idx(ev, "RUN"), 4, ("@", "q100")), "run:")

    def run_twice(ev):
        i = idx(ev, "RUN")
        ev.insert(i + 1, ("RUN", 1, 1, [300, inc(3), 9, 11, ("@", "i300")]))
    neg("one installation observed running twice", H, run_twice, "run:")

    # ── commits ──────────────────────────────────────────────────────────────────────────────
    def one_commit(ev):  # the requeue happens before the hold; the window keeps only the install
        b = idx(ev, "HOLD_BEGIN")
        sec = ev.pop(b + 1)
        add = ev.pop(b + 1)
        ev[b:b] = [sec, add]
    neg("missing genuine commits (one in the window)", H, one_commit, "coverage:")

    out.append(("global serialization (every round-0 attempt reaches its bound)", render(serialized(), 1),
                "fail", "coverage:"))

    # ── attempts: validity before coverage ──────────────────────────────────────────────────
    neg("invalid uncredited attempt: interrupts enabled (hidden behind a credited retry)", R,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN"), 5, 1 | (1 << 1)), "attempt:")
    neg("invalid uncredited attempt: two locks held (hidden behind a credited retry)", R,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN"), 4, held(2)), "attempt:")
    neg("invalid uncredited attempt: admission lock held", R,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN"), 4, held(1, 1)), "attempt:")
    neg("invalid uncredited attempt: lock reacquired", R,
        lambda ev: setf(ev, idx(ev, "HOLD_END"), 5, 1 | (9 << 8)), "attempt:")
    neg("invalid uncredited attempt: failure with neither interference nor bound", R,
        lambda ev: setf(ev, idx(ev, "HOLD_END"), 4, 12), "attempt:")
    neg("invalid uncredited attempt: success claimed with one commit", R,
        lambda ev: setf(ev, idx(ev, "HOLD_END"), 5, 1 | 2 | (6 << 8)), "attempt:")
    # One closer per hold (the Q9 x86 2-CPU loaded sample of 64c9fdcf: success reported with
    # related=1, the observer having acquired the holder's lock after the holder's decision).
    def success_related(ev):
        e = idx(ev, "HOLD_END")
        setf(ev, e, 3, 1)
        ev.insert(e, ("RELATED", 0, 1, [0, 0]))
    neg("success reported together with a RELATED (two closers)", H, success_related, "attempt:")
    neg("success with a RELATED reserved after its HOLD_END", H,
        lambda ev: ev.insert(idx(ev, "HOLD_END") + 1, ("RELATED", 0, 1, [0, 0])), "attempt:")
    neg("interference reported with no RELATED record", X, lambda ev: ev.pop(idx(ev, "RELATED")), "attempt:")
    neg("RELATED naming no hold of the trace", X, lambda ev: setf(ev, idx(ev, "RELATED"), 1, 2), "attempt:")
    neg("attempt number out of order", R, lambda ev: setf(ev, idx(ev, "HOLD_BEGIN", 1), 5, 1 | (2 << 8)),
        "attempt:")
    neg("holder generation disagrees with its sections", H,
        lambda ev: setf(ev, idx(ev, "HOLD_BEGIN", 1), 3, 3), "attempt:")

    def holder_sec(ev):
        b = idx(ev, "HOLD_BEGIN", 1)
        ev[b + 1:b + 1] = [("SEC", 1, 1, [19, 301, 301, 1, 0, C1]), ("ADD", 1, 1, [19, 700, 1])]
        c = idx(ev, "CENSUS_END")
        ev.insert(c, ("CENSUS_Q", 1, 1, [700, 1]))
    neg("a section on the holder's CPU inside its hold", H, holder_sec, "attempt:")
    neg("success count disagrees with the producer", H, lambda ev: None, "attempt:", ok=1)
    neg("CENSUS_END disagrees with SL1W_END", H, lambda ev: None, "validity:", census_ok=1)

    # ── natural and transport ────────────────────────────────────────────────────────────────
    def dup_cons(ev):
        i = lab(ev, "i200")
        ev.insert(i + 3, ("REM", 1, 1, [21, 300]))
        setf(ev, i, 4, 2)
    neg("duplicate consumption", H, dup_cons, "validity:")
    neg("dropped wake notification", H, lambda ev: ev.pop(idx(ev, "NOTIFY")), "natural:")
    neg("lost wake at census", H, lambda ev: ev.pop(idx(ev, "CENSUS_Q", 1)), "natural:")
    neg("window not closed", H, lambda ev: None, "validity:", phase=1)
    neg("ring overflowed", H, lambda ev: None, "validity:", overflow=1)

    def torn(lines):
        return [l.replace("kind=SEC cpu=1 exec=0", "kind=SEC cpu=1 exec=1", 1) for l in lines]
    out.append(("torn record (bad CRC)", render(H, 2, transform=torn), "fail", "validity:"))

    def conflicting(lines):  # two different CRC-valid copies of one record
        i = [k for k, l in enumerate(lines) if "kind=NOTIFY" in l][0]
        body = lines[i].rsplit(" crc=", 1)[0].replace("f0=11", "f0=12")
        alt = "%s crc=%08x" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)
        return lines[:i + 1] + [alt] + lines[i + 1:]
    out.append(("conflicting redundant copies", render(H, 2, transform=conflicting), "fail", "validity:"))
    return out


def main(argv):
    if argv[1:2] != ["--out"] or len(argv) != 3:
        print(__doc__)
        return 2
    out = argv[2]
    os.makedirs(out, exist_ok=True)
    rows = []
    for k, (name, text, expect, ob) in enumerate(cases()):
        fn = "case%02d.log" % k
        with open(os.path.join(out, fn), "w") as f:
            f.write(text)
        rows.append("%s\t%s\t%s\t%s" % (fn, expect, ob, name))
    with open(os.path.join(out, "cases.tsv"), "w") as f:
        f.write("\n".join(rows) + "\n")
    print("%d cases" % len(rows))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
