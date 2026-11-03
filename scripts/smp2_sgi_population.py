#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KTHREAD-CLOSURE1 §A3 — the AArch64 SMP2 witness's SGI population check, host side.

The same rule as the kernel verifier `src/kernel/boot/smp2_record.rs` `check_sgi_population`,
and both are run against one independently specified case corpus
(`tests/fixtures/smp2/sgi_population.cases`, whose header states the physical model).

Per (source, target): the arrivals, in record order, must split the sends, in record order, into
consecutive non-empty groups, each group recorded before its arrival. That is: the k-th arrival
needs at least k sends recorded before it (else it is unexplained), and every send must be
recorded before the pair's last arrival (else it was never delivered). Each arrival needs exactly
one completion carrying its own token before the target's next arrival.

The delivered check let one arrival consume EVERY earlier unconsumed send, so two sends recorded
before an arrival that the GIC delivered as two arrivals left the second "unexplained"
(`sgi_unexplained_arrival`, reproduced on base `a7da04f1` and `40132e1a`).

    check(recs) -> (verdict, seq)   verdict in ok | unexplained | undelivered | completion
    smp2_sgi_population.py --cases FILE   run a corpus; exit 0 iff every case gets its expected verdict

A record is a dict: kind ("sgi_sent" | "sgi_arrived" | "sgi_completed"), seq, cpu, f (list):
sent f[0] = target; arrived f[0] = token, f[1] = source; completed f[0] = token.
"""
import sys


def check(recs):
    sends, arrivals = {}, {}
    for r in recs:
        if r["kind"] == "sgi_sent":
            sends.setdefault((r["cpu"], r["f"][0]), []).append(r)
        elif r["kind"] == "sgi_arrived":
            arrivals.setdefault((r["f"][1], r["cpu"]), []).append(r)
    # Precedence, as the delivered verifier reports: the earliest failing ARRIVAL (unexplained or
    # completion), then the earliest undelivered send.
    first = None
    undelivered = None

    def note(verdict, seq):
        nonlocal first, undelivered
        if verdict == "undelivered":
            if undelivered is None or seq < undelivered[1]:
                undelivered = (verdict, seq)
        elif first is None or seq < first[1]:
            first = (verdict, seq)

    for pair, arr in arrivals.items():
        ss = sends.get(pair, [])
        for k, a in enumerate(arr, start=1):
            if sum(1 for s in ss if s["seq"] < a["seq"]) < k:
                note("unexplained", a["seq"])
                break
    for pair, ss in sends.items():
        arr = arrivals.get(pair, [])
        last = arr[-1]["seq"] if arr else -1
        for s in ss:
            if s["seq"] > last:
                note("undelivered", s["seq"])
                break
    by_cpu = {}
    for i, r in enumerate(recs):
        if r["kind"] == "sgi_arrived":
            by_cpu.setdefault(r["cpu"], []).append(i)
    for cpu, idx in by_cpu.items():
        for n, i in enumerate(idx):
            end = idx[n + 1] if n + 1 < len(idx) else len(recs)
            tok = recs[i]["f"][0]
            done = [r for r in recs[i + 1:end] if r["kind"] == "sgi_completed" and r["cpu"] == cpu]
            if len(done) != 1 or done[0]["f"][0] != tok:
                note("completion", recs[i]["seq"])
    return first or undelivered or ("ok", -1)


def parse_cases(text):
    cases, cur = [], None
    for line in text.splitlines():
        t = line.split("#", 1)[0].split()
        if not t:
            continue
        if t[0] == "case":
            cur = dict(name=t[1], expect=t[2].split("=", 1)[1], recs=[])
        elif t[0] == "end":
            cases.append(cur)
            cur = None
        else:
            seq = len(cur["recs"])
            if t[0] == "sent":
                cur["recs"].append(dict(kind="sgi_sent", seq=seq, cpu=int(t[1]), f=[int(t[2]), 0, 0, 0, 0]))
            elif t[0] == "arrived":
                cur["recs"].append(dict(kind="sgi_arrived", seq=seq, cpu=int(t[1]), f=[int(t[2], 16), int(t[3]), 0, 0, 0]))
            elif t[0] == "completed":
                cur["recs"].append(dict(kind="sgi_completed", seq=seq, cpu=int(t[1]), f=[int(t[2], 16), 0, 0, 0, 0]))
            else:
                raise SystemExit("bad corpus line: %s" % line)
    return cases


def main():
    if len(sys.argv) != 3 or sys.argv[1] != "--cases":
        raise SystemExit(__doc__)
    bad = 0
    cases = parse_cases(open(sys.argv[2]).read())
    for c in cases:
        verdict, _ = check(c["recs"])
        ok = verdict == c["expect"]
        bad += not ok
        print("CASE %-55s expect=%-11s got=%-11s %s" % (c["name"], c["expect"], verdict, "ok" if ok else "MISMATCH"))
    print("SGI_POPULATION_CASES cases=%d mismatches=%d result=%s" % (len(cases), bad, "ok" if bad == 0 else "fail"))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
