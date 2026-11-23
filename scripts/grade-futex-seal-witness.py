#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-SEAL1 — grade one live futex seal boot.

Input: the normalized serial log of a boot whose kernel is built with `futex-seal-witness`
(the FUTEX-TIME1 markers plus `kernel::seal_witness`) and whose init runs the seal cells named by
--cells, booted with `yarm.futex_requeue_witness=1` (and, for --profile rtb, the RT profile with
budgets). Kernel markers used: FUTEX_TIME_W_REGISTER tid gen deadline now va mask,
FUTEX_TIME_W_WAKE tid gen va mask, FUTEX_TIME_W_BITSET_SKIP tid mask wake_mask va,
FUTEX_TIME_W_TIMEOUT cpu tid gen now current va deadline mask, FSEAL_K_BUDGET1/2.
Markers fail closed: a damaged required marker fails the rule that needs it.

Rules (each failure names its rule):
  CORE  the core smoke passed, no fatal marker, FSEAL_ALL_END cells = the number of cells.
  OS    selective-wake order. A(0x1), B(0x2), C(0x1) register on one word in that order; the
        kernel's WAKE markers on it select B (mask 0x2), then A, then C (mask 0x1); A was passed
        over (BITSET_SKIP, wake mask 0x2) before B's WAKE; every wake woke 1; no TIMEOUT; FSO_END ok.
  NR17  V (0x8) is woken on N1 by NR 17's wake portion, never skipped; V2 (0x4) and X (0x2)
        register on N1 and are moved (requeued=2); V2 is woken ON N2 with mask 0x4; X times out ON
        N2 with its registered deadline and mask 0x2; nobody is left; FSN_END ok.
  LH    late sole-wake handoff. Every round: W's cycle (its REGISTER deadline = FSH_OP deadline)
        ends by exactly one of WAKE / TIMEOUT; an acquiring W owned the mutex; F completed with
        ownership after W's return; the word was free at the round's end. LATE rounds (WAKE and
        W's resume >= deadline) must acquire. At least one LATE round. FSH_END ok.
  RB    budget. W's one timed condition wait: one REGISTER, one TIMEOUT (now >= deadline), W
        returns timed_out holding and owning the mutex. FSEAL_K_BUDGET names the same tid and gen,
        one arm. rtb: exactly one INDUCED drain (enabled=1, before>0), stamps drain < park <
        commit, commit_parked=1, one park at a tick before its release instant, the first release
        stamped after the commit (else INCONCLUSIVE), one rejoin for that same release instant,
        applied at or after it. gp: no budget context (enabled=0), nothing parked or rejoined.
  PB    paired broadcast, the same oracle for both arms: each round's cohort was confirmed parked,
        every worker completed every round exactly once, in order, seeing the round's predicate and
        holding the mutex, and the round completed. Arm schedule ABBA from rq. rq rounds: one NR 17
        call, woken=1 and requeued=cohort-1 (clean), else classified capacity-limited (calls>1) or
        fallback; an rq round without an NR 17 call, or with wake-all's signature, fails. wa rounds:
        no NR 17 call and woken=cohort.
Exit status 0 = sealed, 1 = not sealed. --self-test checks the rules on synthetic boots.
"""
import argparse
import json
import re
import statistics
import sys

ARCHES = ("aarch64", "riscv64", "x86_64")
CELLS = ("os", "nr17", "lh", "pb", "rb")
FATAL = (
    "YARM_AARCH64_TRAP_HANDLE halting",
    "RISCV_TRAP_HANDLE_FAILED",
    "x86 shared trap dispatch failed",
    "KERNEL_PANIC",
    "panicked at",
)
KV = re.compile(r"(\w+)=(\S+)")


def kv(line):
    return dict(KV.findall(line))


def num(v):
    if v is None:
        return None
    try:
        return int(v, 0)
    except ValueError:
        return None


class Log:
    def __init__(self, lines):
        self.lines = [l.rstrip("\n") for l in lines]

    def find(self, tag):
        """(index, kv) of every line carrying `tag` as a word."""
        out = []
        pat = re.compile(r"(?:^|\s|msg=)" + re.escape(tag) + r"(?:\s|$)")
        for i, l in enumerate(self.lines):
            if pat.search(l):
                out.append((i, kv(l)))
        return out


def grade(lines, cells, profile, core_rc=0, summary=None):
    log = Log(lines)
    fails = []
    stats = {}

    def bad(rule, msg):
        fails.append(f"{rule}: {msg}")

    # CORE
    if core_rc != 0:
        bad("CORE", f"core smoke rc={core_rc}")
    for l in log.lines:
        for f in FATAL:
            if f in l:
                bad("CORE", f"fatal marker: {f}")
    end = log.find("FSEAL_ALL_END")
    if len(end) != 1 or num(end[0][1].get("cells")) != len(cells):
        bad("CORE", f"FSEAL_ALL_END cells != {len(cells)}: {[e[1] for e in end]}")

    reg = log.find("FUTEX_TIME_W_REGISTER")
    wake = log.find("FUTEX_TIME_W_WAKE")
    skip = log.find("FUTEX_TIME_W_BITSET_SKIP")
    tmo = log.find("FUTEX_TIME_W_TIMEOUT")

    def tid_of(tag, role):
        for _, d in log.find(tag):
            if d.get("role") == role:
                return num(d.get("tid"))
        return None

    def ended_ok(tag, rule):
        e = log.find(tag)
        if len(e) != 1 or e[0][1].get("result") != "ok":
            bad(rule, f"{tag} not result=ok: {[x[1] for x in e]}")

    if "os" in cells:
        ended_ok("FSO_END", "OS")
        t = {r: tid_of("FSO_THREAD", r) for r in "ABC"}
        word = num(log.find("FSO_BEGIN")[0][1].get("word")) if log.find("FSO_BEGIN") else None
        regs = [(i, d) for i, d in reg if num(d.get("va")) == word]
        order = [num(d.get("tid")) for _, d in regs]
        if order != [t["A"], t["B"], t["C"]]:
            bad("OS", f"registration order {order} != A,B,C {t}")
        masks = {num(d.get("tid")): num(d.get("mask")) for _, d in regs}
        if masks != {t["A"]: 1, t["B"]: 2, t["C"]: 1}:
            bad("OS", f"registered masks {masks}")
        wk = [(i, d) for i, d in wake if num(d.get("tid")) in t.values()]
        sel = [num(d.get("tid")) for _, d in wk]
        if sel != [t["B"], t["A"], t["C"]]:
            bad("OS", f"selection order {sel} != B,A,C")
        if [num(d.get("mask")) for _, d in wk] != [2, 1, 1]:
            bad("OS", f"selected masks {[d.get('mask') for _, d in wk]}")
        sk = [i for i, d in skip if num(d.get("tid")) == t["A"] and num(d.get("wake_mask")) == 2]
        if not sk or not wk or sk[0] > wk[0][0]:
            bad("OS", "A not passed over before B's selection")
        if any(num(d.get("tid")) in t.values() for _, d in tmo):
            bad("OS", "an order waiter timed out")
        if [num(d.get("woke")) for _, d in log.find("FSO_WAKE")] != [1, 1, 1]:
            bad("OS", "a masked wake did not wake exactly one")

    if "nr17" in cells:
        ended_ok("FSN_END", "NR17")
        b = log.find("FSN_BEGIN")
        n1 = num(b[0][1].get("n1")) if b else None
        n2 = num(b[0][1].get("n2")) if b else None
        t = {r: tid_of("FSN_THREAD", r) for r in ("V", "V2", "X")}
        rq = {d.get("step"): d for _, d in log.find("FSN_RQ")}
        vw = [d for _, d in wake if num(d.get("tid")) == t["V"]]
        if len(vw) != 1 or num(vw[0].get("va")) != n1 or num(vw[0].get("mask")) != 8:
            bad("NR17", f"V not woken once on N1 with mask 0x8: {vw}")
        if any(num(d.get("tid")) == t["V"] for _, d in skip):
            bad("NR17", "NR 17's wake skipped V for its mask")
        if num(rq.get("1", {}).get("woken")) != 1:
            bad("NR17", "step 1 NR 17 woke != 1")
        if num(rq.get("2", {}).get("requeued")) != 2:
            bad("NR17", "step 2 NR 17 requeued != 2")
        r2 = {num(d.get("tid")): d for _, d in reg if num(d.get("tid")) in (t["V2"], t["X"])}
        if any(num(d.get("va")) != n1 for d in r2.values()) or len(r2) != 2:
            bad("NR17", "V2/X not registered on N1")
        v2 = [d for _, d in wake if num(d.get("tid")) == t["V2"]]
        if len(v2) != 1 or num(v2[0].get("va")) != n2 or num(v2[0].get("mask")) != 4:
            bad("NR17", f"V2 not woken on N2 with mask 0x4: {v2}")
        xt = [d for _, d in tmo if num(d.get("tid")) == t["X"]]
        xr = r2.get(t["X"], {})
        if (
            len(xt) != 1
            or num(xt[0].get("va")) != n2
            or num(xt[0].get("deadline")) != num(xr.get("deadline"))
            or num(xt[0].get("mask")) != 2
            or num(xt[0].get("now")) < num(xt[0].get("deadline"))
        ):
            bad("NR17", f"X did not expire on N2 with its deadline and mask: {xt} reg={xr}")
        if any(num(d.get("tid")) == t["X"] for _, d in wake):
            bad("NR17", "X was woken")
        left = log.find("FSN_LEFT")
        if not left or num(left[0][1].get("woken")) != 0:
            bad("NR17", "waiters left behind")

    if "lh" in cells:
        ended_ok("FSH_END", "LH")
        w = tid_of("FSH_THREAD", "W")
        ops = {d.get("round"): d for _, d in log.find("FSH_OP") if d.get("role") == "W"}
        rets = {d.get("round"): (i, d) for i, d in log.find("FSH_RET") if d.get("role") == "W"}
        frets = {d.get("round"): (i, d) for i, d in log.find("FSH_FRET")}
        rounds = {d.get("round"): d for _, d in log.find("FSH_ROUND")}
        classes = []
        for r, op in sorted(ops.items(), key=lambda x: int(x[0])):
            dl = num(op.get("deadline"))
            g = [num(d.get("gen")) for _, d in reg if num(d.get("tid")) == w and num(d.get("deadline")) == dl]
            if len(g) != 1:
                bad("LH", f"round {r}: W's registration not found once")
                continue
            ws = [d for _, d in wake if num(d.get("tid")) == w and num(d.get("gen")) == g[0]]
            ts = [d for _, d in tmo if num(d.get("tid")) == w and num(d.get("gen")) == g[0]]
            if len(ws) + len(ts) != 1:
                bad("LH", f"round {r}: W's cycle ended {len(ws)} wakes / {len(ts)} timeouts")
                continue
            ret = rets.get(r)
            if not ret:
                bad("LH", f"round {r}: W did not return")
                continue
            rd = ret[1]
            acquired = rd.get("result") == "acquired"
            if acquired and num(rd.get("own")) != 1:
                bad("LH", f"round {r}: W acquired without ownership")
            if rd.get("result") not in ("acquired", "timed_out"):
                bad("LH", f"round {r}: W result {rd.get('result')}")
            resume = num(rd.get("resume")) or 0
            if ws and resume >= dl:
                cls = "late"
                if not acquired:
                    bad("LH", f"round {r}: late sole wake abandoned the mutex")
            elif ws:
                cls = "early"
            else:
                cls = "timeout_acquired" if acquired else "timeout"
            classes.append(cls)
            fr = frets.get(r)
            if not fr or num(fr[1].get("own")) != 1:
                bad("LH", f"round {r}: F did not complete with ownership")
            elif acquired and num(fr[1].get("now")) <= num(rd.get("now")):
                bad("LH", f"round {r}: F's completion does not follow W's")
            if num(rounds.get(r, {}).get("free")) != 1:
                bad("LH", f"round {r}: mutex not free at the end")
        if "late" not in classes:
            bad("LH", f"no late handoff round observed: {classes}")
        stats["lh"] = {c: classes.count(c) for c in ("late", "early", "timeout_acquired", "timeout")}

    if "rb" in cells:
        ended_ok("FSB_END", "RB")
        w = tid_of("FSB_THREAD", "W")
        op = log.find("FSB_OP")
        dl = num(op[0][1].get("deadline")) if op else None
        rg = [d for _, d in reg if num(d.get("tid")) == w and num(d.get("deadline")) == dl]
        tm = [d for _, d in tmo if num(d.get("tid")) == w]
        gen = num(rg[0].get("gen")) if len(rg) == 1 else None
        if len(rg) != 1:
            bad("RB", "W's wait not registered once")
        if len(tm) != 1 or num(tm[0].get("gen")) != gen or num(tm[0].get("now")) < dl:
            bad("RB", f"W's wait not settled once by its timeout: {tm}")
        if any(num(d.get("tid")) == w for _, d in wake):
            bad("RB", "W's wait was woken")
        ret = log.find("FSB_RET")
        if (
            len(ret) != 1
            or ret[0][1].get("result") != "timed_out"
            or num(ret[0][1].get("held")) != 1
            or num(ret[0][1].get("own")) != 1
        ):
            bad("RB", f"W did not return timed_out holding the mutex: {[x[1] for x in ret]}")
        k1 = log.find("FSEAL_K_BUDGET1")
        k2 = log.find("FSEAL_K_BUDGET2")
        if len(k1) != 1 or len(k2) != 1:
            bad("RB", "kernel budget record missing or repeated")
        else:
            a, b = k1[0][1], k2[0][1]
            g = lambda d, k: num(d.get(k))
            if g(a, "tid") != w or g(a, "gen") != gen or g(a, "arms") != 1:
                bad("RB", "record does not name W's one marked wait")
            if profile == "rtb":
                if g(a, "enabled") != 1 or g(a, "drains") != 1 or not g(a, "before"):
                    bad("RB", "no single induced drain of a live budget")
                if not (g(a, "drain_stamp") < g(b, "park_stamp") < g(a, "commit_stamp")):
                    bad("RB", "stamps not drain < park < commit")
                if g(a, "commit_parked") != 1 or g(b, "parks") != 1:
                    bad("RB", "settlement did not park the waiter exactly once")
                if g(b, "park_next") != g(a, "next_release") or g(b, "park_tick") >= g(b, "park_next"):
                    bad("RB", "not parked before its release instant")
                if not g(b, "first_release_stamp") or g(b, "first_release_stamp") < g(a, "commit_stamp"):
                    bad("RB", "INCONCLUSIVE: a release preceded the settlement's commit")
                if (
                    g(b, "rejoins") != 1
                    or g(b, "rejoin_release") != g(b, "park_next")
                    or g(b, "rejoin_tick") < g(b, "rejoin_release")
                    or g(b, "rejoin_stamp") <= g(a, "commit_stamp")
                ):
                    bad("RB", "not one rejoin for that release instant after the commit")
                stats["rb"] = {
                    "induced_exhaustion": 1,
                    "drain_tick": g(a, "drain_tick"),
                    "park_tick": g(b, "park_tick"),
                    "release": g(b, "park_next"),
                    "rejoin_tick": g(b, "rejoin_tick"),
                    "release_lateness_ticks": g(b, "rejoin_tick") - g(b, "rejoin_release"),
                    "park_cpu": g(b, "park_cpu"),
                    "rejoin_cpu": g(b, "rejoin_cpu"),
                    "deadline_ns": dl,
                    "settled_ns": num(tm[0].get("now")) if tm else None,
                }
            else:
                if g(a, "enabled") != 0 or g(a, "commit_parked") != 0 or g(b, "parks") or g(b, "rejoins"):
                    bad("RB", "budgets off, yet a context was drained, parked or rejoined")
                stats["rb"] = {"induced_exhaustion": 0}

    if "pb" in cells:
        ended_ok("FSL_END", "PB")
        beg = log.find("FSL_BEGIN")
        n = num(beg[0][1].get("workers")) if beg else 0
        rounds = num(beg[0][1].get("rounds")) if beg else 0
        parked = {num(d.get("round")): d for _, d in log.find("FSL_PARKED")}
        bc = {num(d.get("round")): d for _, d in log.find("FSL_BCAST")}
        rd = {num(d.get("round")): d for _, d in log.find("FSL_ROUND")}
        done = log.find("FSL_DONE")
        unl = log.find("FSL_UNLOCK")
        per = {}
        last = {}
        for _, d in done:
            r, k = num(d.get("round")), num(d.get("worker"))
            per.setdefault(r, []).append(d)
            if num(d.get("seen")) != r or num(d.get("held")) != 1 or num(d.get("own")) != 1:
                bad("PB", f"round {r} worker {k}: seen={d.get('seen')} held={d.get('held')} own={d.get('own')}")
            if last.get(k, 0) + 1 != r:
                bad("PB", f"worker {k}: round {r} out of order (after {last.get(k, 0)})")
            last[k] = r
        arms = {}
        for r in range(1, rounds + 1):
            want = "rq" if (((r - 1) // 2) % 2 == 0) == ((r - 1) % 2 == 0) else "wa"
            b = bc.get(r, {})
            if b.get("arm") != want:
                bad("PB", f"round {r}: arm {b.get('arm')} != schedule {want}")
            if num(parked.get(r, {}).get("ok")) != 1:
                bad("PB", f"round {r}: cohort not confirmed parked")
            ws = per.get(r, [])
            if sorted(num(d.get("worker")) for d in ws) != list(range(n)):
                bad("PB", f"round {r}: completions {sorted(num(d.get('worker')) for d in ws)}")
            if num(rd.get(r, {}).get("all")) != 1:
                bad("PB", f"round {r}: incomplete")
            woken, rqd = num(b.get("woken")), num(b.get("requeued"))
            calls, fb, rqc = num(b.get("calls")), num(b.get("fallback")), num(b.get("rq_calls"))
            if want == "rq":
                if not rqc:
                    bad("PB", f"round {r}: rq arm made no NR 17 call")
                    cls = "invalid"
                elif fb:
                    cls = "fallback"
                elif calls and calls > 1:
                    cls = "capacity_limited"
                elif woken == 1 and rqd == n - 1:
                    cls = "clean"
                else:
                    bad("PB", f"round {r}: rq woken={woken} requeued={rqd}")
                    cls = "invalid"
            else:
                if rqc:
                    bad("PB", f"round {r}: wa arm made an NR 17 call")
                cls = "clean" if woken == n else "capacity_limited"
                if woken != n and calls and calls <= 1:
                    bad("PB", f"round {r}: wa woke {woken} of {n}")
            u = [d for _, d in unl if num(d.get("round")) == r]
            a = arms.setdefault(want, [])
            a.append(
                {
                    "round": r,
                    "class": cls,
                    "direct_woken": woken,
                    "requeued": rqd,
                    "mutex_reparks": sum(num(d.get("mtx_parked")) or 0 for d in ws),
                    "seq_parks": sum(num(d.get("seq_parked")) or 0 for d in ws),
                    "mutex_value_changed": sum(num(d.get("mtx_vc")) or 0 for d in ws),
                    "worker_unlock_wake_calls": sum(num(d.get("wakes")) or 0 for d in u),
                    "worker_unlock_woke": sum(num(d.get("woke")) or 0 for d in u),
                    "broadcaster_unlock_wake_calls": num(b.get("unlock_wakes")),
                    "broadcaster_unlock_woke": num(b.get("unlock_woke")),
                    "elapsed_ns": num(b.get("elapsed_ns")),
                    "completions": len(ws),
                }
            )
        summ = {}
        for arm, rs in arms.items():
            clean = [x for x in rs if x["class"] == "clean"]
            el = [x["elapsed_ns"] for x in clean if x["elapsed_ns"] is not None]
            summ[arm] = {
                "rounds": len(rs),
                "clean": len(clean),
                "classes": {c: sum(1 for x in rs if x["class"] == c) for c in set(x["class"] for x in rs)},
                "direct_woken": [x["direct_woken"] for x in clean],
                "requeued": [x["requeued"] for x in clean],
                "mutex_reparks": [x["mutex_reparks"] for x in clean],
                "worker_unlock_wake_calls": [x["worker_unlock_wake_calls"] for x in clean],
                "worker_unlock_woke": [x["worker_unlock_woke"] for x in clean],
                "elapsed_ns_median": int(statistics.median(el)) if el else None,
                "elapsed_ns_min": min(el) if el else None,
                "elapsed_ns_max": max(el) if el else None,
            }
        stats["pb"] = {"cohort": n, "arms": summ, "rounds": arms}
        if not summ.get("rq", {}).get("clean") or not summ.get("wa", {}).get("clean"):
            bad("PB", "an arm has no clean round")

    if summary:
        with open(summary, "w") as f:
            json.dump(stats, f, indent=1, sort_keys=True)
    return fails, stats


def seal_line(arch, smp, profile, cells, fails, stats):
    extra = ""
    if "lh" in stats:
        extra += " lh_late={late} lh_early={early} lh_timeout={timeout} lh_timeout_acquired={timeout_acquired}".format(**stats["lh"])
    if "rb" in stats:
        extra += f" rb_induced={stats['rb']['induced_exhaustion']}"
        if "release_lateness_ticks" in stats["rb"]:
            extra += f" rb_release_lateness_ticks={stats['rb']['release_lateness_ticks']}"
    if "pb" in stats:
        for arm in ("rq", "wa"):
            a = stats["pb"]["arms"].get(arm, {})
            extra += f" pb_{arm}_clean={a.get('clean')}/{a.get('rounds')} pb_{arm}_reparks={sum(a.get('mutex_reparks', []))}"
    res = "ok" if not fails else "fail"
    return f"FUTEX_SEAL_WITNESS_SEAL arch={arch} smp={smp} profile={profile} cells={','.join(cells)}{extra} result={res}"


# ── self-test ───────────────────────────────────────────────────────────────────────────────
def synth(profile, n=10, rounds=8):
    L = []
    u = lambda tid, m: L.append(f"USER_LOG tid={tid} msg={m}")
    # OS
    u(1, "FSO_BEGIN s_tid=1 word=0x100 workers=3")
    for r, t in zip("ABC", (11, 12, 13)):
        u(1, f"FSO_THREAD role={r} tid={t}")
    for t, m in ((11, 1), (12, 2), (13, 1)):
        L.append(f"FUTEX_TIME_W_REGISTER tid={t} gen=1 deadline=900 now=1 va=0x100 mask=0x{m:x}")
    L.append("FUTEX_TIME_W_BITSET_SKIP tid=11 mask=0x1 wake_mask=0x2 va=0x100")
    L.append("FUTEX_TIME_W_WAKE tid=12 gen=1 va=0x100 mask=0x2")
    u(1, "FSO_WAKE step=1 mask=0x2 woke=1")
    L.append("FUTEX_TIME_W_WAKE tid=11 gen=1 va=0x100 mask=0x1")
    u(1, "FSO_WAKE step=2 mask=0x1 woke=1")
    L.append("FUTEX_TIME_W_WAKE tid=13 gen=1 va=0x100 mask=0x1")
    u(1, "FSO_WAKE step=3 mask=0x1 woke=1")
    u(1, "FSO_END fails=0 result=ok")
    # NR17
    u(1, "FSN_BEGIN s_tid=1 n1=0x200 n2=0x204 n3=0x208")
    for r, t in (("V", 21), ("V2", 22), ("X", 23)):
        u(1, f"FSN_THREAD role={r} tid={t}")
    L.append("FUTEX_TIME_W_REGISTER tid=21 gen=1 deadline=900 now=1 va=0x200 mask=0x8")
    L.append("FUTEX_TIME_W_WAKE tid=21 gen=1 va=0x200 mask=0x8")
    u(1, "FSN_RQ step=1 woken=1 requeued=0")
    L.append("FUTEX_TIME_W_REGISTER tid=22 gen=1 deadline=900 now=2 va=0x200 mask=0x4")
    L.append("FUTEX_TIME_W_REGISTER tid=23 gen=1 deadline=50 now=3 va=0x200 mask=0x2")
    u(1, "FSN_RQ step=2 woken=0 requeued=2")
    L.append("FUTEX_TIME_W_WAKE tid=22 gen=1 va=0x204 mask=0x4")
    u(1, "FSN_RQ step=3 woken=1 requeued=0")
    L.append("FUTEX_TIME_W_TIMEOUT cpu=0 tid=23 gen=1 now=55 current=0 va=0x204 deadline=50 mask=0x2")
    u(1, "FSN_LEFT woken=0")
    u(1, "FSN_END fails=0 result=ok")
    # LH: round 1 late, round 2 timeout
    u(1, "FSH_BEGIN s_tid=1")
    u(1, "FSH_THREAD role=W tid=31")
    u(1, "FSH_THREAD role=F tid=32")
    u(31, "FSH_OP round=1 role=W deadline=1000 now=1")
    L.append("FUTEX_TIME_W_REGISTER tid=31 gen=1 deadline=1000 now=1 va=0x300 mask=0xffffffff")
    L.append("FUTEX_TIME_W_WAKE tid=31 gen=1 va=0x300 mask=0xffffffff")
    u(31, "FSH_RET round=1 role=W result=acquired own=1 resume=1010 waits=1 deadline=1000 now=1011")
    u(32, "FSH_FRET round=1 own=1 now=1020")
    u(1, "FSH_ROUND round=1 w_done=1 f_done=1 free=1")
    u(31, "FSH_OP round=2 role=W deadline=2000 now=1100")
    L.append("FUTEX_TIME_W_REGISTER tid=31 gen=2 deadline=2000 now=1100 va=0x300 mask=0xffffffff")
    L.append("FUTEX_TIME_W_TIMEOUT cpu=0 tid=31 gen=2 now=2001 current=0 va=0x300 deadline=2000 mask=0xffffffff")
    u(31, "FSH_RET round=2 role=W result=timed_out own=0 resume=2002 waits=1 deadline=2000 now=2003")
    u(32, "FSH_FRET round=2 own=1 now=2010")
    u(1, "FSH_ROUND round=2 w_done=1 f_done=1 free=1")
    u(1, "FSH_END fails=0 result=ok")
    # PB
    u(1, f"FSL_BEGIN s_tid=1 workers={n} rounds={rounds}")
    for r in range(1, rounds + 1):
        arm = "rq" if (((r - 1) // 2) % 2 == 0) == ((r - 1) % 2 == 0) else "wa"
        u(1, f"FSL_PARKED round={r} arm={arm} n={n} ok=1")
        for k in range(n):
            rp = 0 if arm == "rq" else (1 if k else 0)
            u(40 + k, f"FSL_DONE round={r} arm={arm} worker={k} seen={r} held=1 own=1 seq_parked=1 seq_vc=0 mtx_parked={rp} mtx_vc=0")
            u(40 + k, f"FSL_UNLOCK round={r} worker={k} wakes=1 woke={1 if k < n - 1 else 0}")
        if arm == "rq":
            u(1, f"FSL_BCAST round={r} arm=rq woken=1 requeued={n-1} calls=1 fallback=0 rq_calls=1 wake_calls=0 unlock_wakes=1 unlock_woke=1 elapsed_ns=100")
        else:
            u(1, f"FSL_BCAST round={r} arm=wa woken={n} requeued=0 calls=1 fallback=0 rq_calls=0 wake_calls=1 unlock_wakes=0 unlock_woke=0 elapsed_ns=120")
        u(1, f"FSL_ROUND round={r} arm={arm} done={n} all=1")
    u(1, "FSL_END fails=0 result=ok")
    # RB
    u(1, "FSB_BEGIN s_tid=1")
    u(51, "FSB_OP role=W deadline=5000 now=4000 seq=0x5eb00001")
    u(1, "FSB_THREAD role=W tid=51")
    L.append("FUTEX_TIME_W_REGISTER tid=51 gen=1 deadline=5000 now=4001 va=0x500 mask=0xffffffff")
    L.append("FUTEX_TIME_W_TIMEOUT cpu=0 tid=51 gen=1 now=5005 current=0 va=0x500 deadline=5000 mask=0xffffffff")
    u(51, "FSB_RET role=W result=timed_out held=1 own=1 resume=5100 deadline=5000 now=5200")
    if profile == "rtb":
        L.append("FSEAL_K_BUDGET1 tid=51 gen=1 arms=1 drains=1 enabled=1 drain_stamp=1 drain_tick=400 before=63 next_release=470 commits=1 commit_parked=1 commit_stamp=3")
        L.append("FSEAL_K_BUDGET2 tid=51 parks=1 park_stamp=2 park_tick=460 park_next=470 park_cpu=1 releases=1 first_release_stamp=4 rejoins=1 rejoin_stamp=5 rejoin_tick=471 rejoin_release=470 rejoin_cpu=1")
    else:
        L.append("FSEAL_K_BUDGET1 tid=51 gen=1 arms=1 drains=1 enabled=0 drain_stamp=1 drain_tick=0 before=0 next_release=0 commits=1 commit_parked=0 commit_stamp=2")
        L.append("FSEAL_K_BUDGET2 tid=51 parks=0 park_stamp=0 park_tick=0 park_next=0 park_cpu=4294967295 releases=0 first_release_stamp=0 rejoins=0 rejoin_stamp=0 rejoin_tick=0 rejoin_release=0 rejoin_cpu=4294967295")
    u(1, "FSB_END fails=0 result=ok")
    u(1, "FSEAL_ALL_END cells=5")
    return L


def self_test():
    cells = list(CELLS)
    for prof in ("gp", "rtb"):
        f, _ = grade(synth(prof), cells, prof)
        assert not f, (prof, f)

    def mutant(prof, rule, edit):
        L = edit(synth(prof))
        f, _ = grade(L, cells, prof)
        assert any(x.startswith(rule + ":") for x in f), (rule, f)

    def sub(a, b, count=1):
        return lambda L: [l.replace(a, b, count) if a in l else l for l in L]

    def swap_lines(a, b):
        def e(L):
            i = next(k for k, l in enumerate(L) if a in l)
            j = next(k for k, l in enumerate(L) if b in l)
            L[i], L[j] = L[j], L[i]
            return L
        return e

    # OS: A selected before B (order or mask ignored).
    mutant("gp", "OS", swap_lines("WAKE tid=12 gen=1 va=0x100", "WAKE tid=11 gen=1 va=0x100"))
    mutant("gp", "OS", lambda L: [l for l in L if "BITSET_SKIP tid=11" not in l])
    # NR17: V skipped for its mask; X's deadline reset after the move.
    mutant("gp", "NR17", lambda L: L + ["FUTEX_TIME_W_BITSET_SKIP tid=21 mask=0x8 wake_mask=0xffffffff va=0x200"])
    mutant("gp", "NR17", sub("tid=23 gen=1 now=55 current=0 va=0x204 deadline=50", "tid=23 gen=1 now=55 current=0 va=0x204 deadline=52"))
    # LH: the late sole wake abandoned; no late round at all.
    mutant("gp", "LH", sub("FSH_RET round=1 role=W result=acquired own=1", "FSH_RET round=1 role=W result=timed_out own=0"))
    mutant("gp", "LH", sub("resume=1010", "resume=990"))
    # PB: wake-all substituted in the rq arm; a duplicate completion; reacquisition omitted.
    def wa_in_rq(L):
        return [
            re.sub(r"arm=rq woken=1 requeued=9 calls=1 fallback=0 rq_calls=1 wake_calls=0", "arm=rq woken=10 requeued=0 calls=1 fallback=0 rq_calls=0 wake_calls=1", l)
            if "FSL_BCAST round=1 " in l else l
            for l in L
        ]
    mutant("gp", "PB", wa_in_rq)
    mutant("gp", "PB", lambda L: L + ["USER_LOG tid=40 msg=FSL_DONE round=2 arm=wa worker=0 seen=2 held=1 own=1 seq_parked=1 seq_vc=0 mtx_parked=0 mtx_vc=0"])
    mutant("gp", "PB", sub("FSL_DONE round=3 arm=wa worker=4 seen=3 held=1", "FSL_DONE round=3 arm=wa worker=4 seen=3 held=0"))
    # A completion that saw the word held, but by another owner.
    mutant("gp", "PB", sub("FSL_DONE round=6 arm=wa worker=2 seen=6 held=1 own=1", "FSL_DONE round=6 arm=wa worker=2 seen=6 held=1 own=0"))
    # RB: W returned while the main thread still held the mutex (held=1, own=0).
    mutant("rtb", "RB", sub("FSB_RET role=W result=timed_out held=1 own=1", "FSB_RET role=W result=timed_out held=1 own=0"))
    # RB: replenished before settlement (inconclusive); not parked; drained twice; gp with a park.
    mutant("rtb", "RB", sub("first_release_stamp=4", "first_release_stamp=2"))
    mutant("rtb", "RB", sub("commit_parked=1", "commit_parked=0"))
    mutant("rtb", "RB", sub("drains=1 enabled=1", "drains=2 enabled=1"))
    mutant("rtb", "RB", sub("rejoins=1", "rejoins=2"))
    mutant("gp", "RB", sub("parks=0", "parks=1"))
    mutant("rtb", "RB", sub("FSB_RET role=W result=timed_out held=1", "FSB_RET role=W result=timed_out held=0"))
    # CORE
    mutant("gp", "CORE", sub("FSEAL_ALL_END cells=5", "FSEAL_ALL_END cells=4"))
    print("self-test ok: 2 passing boots, 21 mutants each caught by its rule")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch", choices=ARCHES)
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--profile", choices=("gp", "rtb"), default="gp")
    ap.add_argument("--cells", default="pb")
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("--core-log", action="append", default=[])
    ap.add_argument("--summary")
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    cells = [c for c in a.cells.replace(",", " ").split() if c]
    for c in cells:
        if c not in CELLS:
            print(f"unknown cell {c}")
            return 1
    with open(a.log, errors="replace") as f:
        lines = f.readlines()
    fails, stats = grade(lines, cells, a.profile, a.core_rc, a.summary)
    for x in fails:
        print(f"[futex-seal-witness][fail] {x}")
    print(seal_line(a.arch, a.smp, a.profile, cells, fails, stats))
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
