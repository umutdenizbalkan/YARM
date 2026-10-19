#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED1/AP-SCHED2 — grade the RISC-V production SMP-scheduling witness, for every expected CPU.
#
# Usage: grade-riscv64-apsched-witness.py --expect-cpus N BOOT_LOG
#        grade-riscv64-apsched-witness.py --self-test
#
# The witness keeps its records in the kernel and dumps them at P's last step, every line twice
# with a CRC-32 of its body. Only lines whose CRC verifies are evidence (the harts' console output
# interleaves, and a torn line is not a record); both copies of one stamp must agree. The grade is
# by causal stamp, never by console position.
#
# The EXPECTED secondaries are 1..N-1, from the boot's own `-smp N` — never from the kernel's
# report. The kernel's release record must name exactly that set, and every obligation below must
# hold on EVERY expected secondary: a CPU that announced readiness but ran nothing fails.
#
# Obligations (each failure names the one it broke):
#   admission   the boot hart released exactly the expected set, each expected CPU took the
#               PRODUCTION admission (its record), and the diagnostic pinned path never ran;
#   placement   P ran on CPU 0; spinners started on CPU 0 and on every expected secondary; one
#               deadline thread, one sleeper and one IPC thread ran on each expected secondary;
#               the churn children, over all rounds, ran on every expected secondary;
#   preemption  every spinner on a secondary was preempted there by that CPU's own timer, each
#               secondary took at least PRE_MIN such preemptions, and every spinner finished on the
#               CPU it started on (no migration) with all 28 patterned registers intact;
#   time        each deadline receive timed out (9) on the CPU it armed on, after at least its
#               timeout in scheduler ticks; the tick advanced by exactly CPU 0's timer interrupts
#               over its window; and its own CPU took interrupts of its own — no CPU sped time up;
#   idle wake   every (sleeper, round): its CPU idle (an idle arrival, not left since) when P stored
#               its word; then an IPI taken at that CPU's idle boundary; then that CPU leaving idle
#               for the sleeper BY that IPI; then the acknowledgement there; one waiter woken;
#   ipc         every (thread, round): the ping arrived intact on CPU 0 from that thread, and the
#               pong intact on the thread's CPU, in order;
#   churn       every child: ran on a secondary; that CPU then left THAT child's root before the
#               reap; and only then reaped, with its address space destroyed and none quarantined;
#   progress    P reached its end with no failure step; the dump is complete and did not overflow.
import re
import sys
import zlib

PRE0_MIN = 3
PRE_MIN = 4
WAKE_ROUNDS = 3
IPC_ROUNDS = 4
FORK_ROUNDS = 4
WITNESS_CPUS = 8
TIMED_OUT = 9
FAIL_STEPS = {
    "P_NO_AP", "P_PRE_TIMEOUT", "P_A_TIMEOUT", "P_B_TIMEOUT", "P_NO_IDLE", "P_NO_ACK",
    "P_PING_RAW", "P_C_TIMEOUT", "P_REAP_TIMEOUT", "P_SPAWN_FAIL", "P_FORK_FAIL", "EXIT_REFUSED",
    "?",
}
FORBIDDEN = [
    "panicked at", "KERNEL PANIC", "RISCV_SMP3_SECONDARY_ADMITTED", "RISCV_AP_SCHED_ADMISSION_REFUSED",
    "RISCV_SMP3_SECONDARY_UNADMITTED", "TIMER_SPLIT_CURRENT_NOT_RUNNING",
    "VM_ASPACE_TEARDOWN_QUARANTINED", "RISCV_AP_QUIESCENT", "RISCV_HART_EXCLUDED",
    "RISCV_LOGICAL_CPU_MISMATCH", "RISCV_SECONDARY_START_FAILED", "RISCV_SECONDARY_NOT_STARTABLE",
]

LINE = re.compile(r"(APSW_(?:R|DUMP_END) [^\n]*?) crc=([0-9a-f]{8})\s*$")


def parse(text):
    """The verified records by stamp, and the verified dump trailer (or None)."""
    recs, end, conflicts = {}, None, []
    for raw in text.split("\n"):
        m = LINE.search(raw)
        if not m:
            continue
        body, crc = m.group(1), int(m.group(2), 16)
        if not body.startswith("APSW_") or zlib.crc32(body.encode()) != crc:
            continue
        fields = dict(kv.split("=", 1) for kv in body.split()[1:] if "=" in kv)
        if body.startswith("APSW_DUMP_END"):
            end = fields
            continue
        seq = int(fields["seq"])
        if seq in recs and recs[seq] != fields:
            conflicts.append(seq)
        recs[seq] = fields
    return recs, end, conflicts


def grade(text, ncpus):
    fails = []
    expected = set(range(1, ncpus))
    if not expected:
        return ["admission: the witness needs at least one secondary (-smp >= 2)"]
    if ncpus > WITNESS_CPUS:
        return [f"admission: the witness's mailbox and worker-stack layout covers at most {WITNESS_CPUS} CPUs"]
    recs, end, conflicts = parse(text)
    if conflicts:
        fails.append(f"progress: two verified copies disagree for stamps {conflicts[:5]}")
    if end is None:
        # Without the record, the console still names any CPU that was refused admission.
        named = [f"admission: CPU {m.group(1)} refused ({m.group(2)})"
                 for m in re.finditer(r"RISCV_AP_STATE cpu=(\d+) [^\n]*admitted=0 [^\n]*reason=(\w+)", text)]
        return [f"progress: no verified dump trailer (P never reached its end)"] + named + fails
    count = int(end["count"])
    if end.get("overflow") != "0":
        fails.append("progress: the record overflowed")
    missing = [s for s in range(1, count + 1) if s not in recs]
    if missing:
        fails.append(f"progress: no verified copy of stamps {missing[:8]}")
    R = [dict(r, seq=int(r["seq"]), cpu=int(r["cpu"]), tid=int(r["tid"]),
              v=[int(r[f"v{i}"]) for i in range(5)]) for _, r in sorted(recs.items())]

    def user(step):
        return [r for r in R if r["kind"] == "USER" and r["step"] == step]

    def kind(k, cpu=None):
        return [r for r in R if r["kind"] == k and (cpu is None or r["cpu"] == cpu)]

    for bad in FORBIDDEN:
        if bad in text:
            fails.append(f"forbidden console marker: {bad}")
    # The kernel publishes into the mailbox frame it captured at provisioning: a copy-on-write
    # split of P's mailbox page would leave every later observation in a frame P no longer maps.
    prov = re.search(r"APSW_PROVISIONED p_tid=\d+ asid=(\d+)", text)
    if prov and re.search(rf"VM_COW_SPLIT_COMMITTED [^\n]*asid={prov.group(1)} va=0x20070000\b", text):
        fails.append("mailbox: P's mailbox page was split copy-on-write")

    # admission — the expected set is the boot's, and the kernel must agree with it
    rel = kind("RELEASE")
    want_mask = sum(1 << c for c in expected)
    if len(rel) != 1 or rel[0]["v"][0] != want_mask:
        got = [hex(r["v"][0]) for r in rel]
        fails.append(f"admission: released {got}, expected {hex(want_mask)} for -smp {ncpus}")
    admitted = {r["cpu"] for r in kind("AP_ADMITTED")}
    for c in sorted(expected - admitted):
        fails.append(f"admission: CPU {c} never took the production admission")

    # progress
    if not user("P_END"):
        fails.append("progress: P never reached its end")
    for r in R:
        if r["kind"] == "USER" and r["step"] in FAIL_STEPS:
            fails.append(f"progress: failure step {r['step']} (stamp {r['seq']})")
    p_start = user("P_START")
    if not p_start or p_start[0]["cpu"] != 0:
        fails.append("placement: P did not start on CPU 0")

    # placement + preemption + registers
    starts = user("W_START")
    want_spinners = 2 * ncpus
    if len(starts) != want_spinners:
        fails.append(f"placement: {len(starts)} spinners started, expected {want_spinners}")
    cpus = {r["cpu"] for r in starts}
    for c in sorted((expected | {0}) - cpus):
        fails.append(f"placement: no spinner ran on CPU {c}")
    spin_tids = {r["tid"] for r in starts}
    pre = [r for r in kind("PREEMPT") if r["tid"] in spin_tids]
    for c in sorted(expected):
        n = len([r for r in pre if r["cpu"] == c])
        if n < PRE_MIN:
            fails.append(f"preemption: {n} timer preemptions of spinners on CPU {c}, need {PRE_MIN}")
    for s in starts:
        if s["cpu"] != 0 and not [r for r in pre if r["tid"] == s["tid"] and r["cpu"] == s["cpu"]]:
            fails.append(f"preemption: spinner {s['tid']} on CPU {s['cpu']} was never preempted by its timer")
        done = [r for r in user("W_DONE") if r["tid"] == s["tid"]]
        if len(done) != 1:
            fails.append(f"preemption: spinner {s['tid']} reported {len(done)} completions")
            continue
        d = done[0]
        if d["v"][0] != 0:
            fails.append(f"registers: spinner {s['tid']} found {d['v'][0]} corrupted registers (last x{d['v'][1]})")
        if d["cpu"] != s["cpu"]:
            fails.append(f"placement: spinner {s['tid']} started on CPU {s['cpu']} and finished on CPU {d['cpu']}")
        if d["seq"] < s["seq"]:
            fails.append(f"preemption: spinner {s['tid']} finished before it started")

    # time — one deadline thread per expected secondary
    dl = kind("DEADLINE")
    before = {r["tid"]: r for r in user("D_BEFORE")}
    dl_cpus = sorted(r["cpu"] for r in dl)
    if dl_cpus != sorted(expected):
        fails.append(f"time: deadline records on CPUs {dl_cpus}, expected one on each of {sorted(expected)}")
    for r in dl:
        timeout, result, dtick, dirq0, dirq_own = r["v"]
        tag = f"time: deadline on CPU {r['cpu']} (tid {r['tid']})"
        if result != TIMED_OUT:
            fails.append(f"{tag} returned {result}, not TimedOut")
        if dtick < timeout:
            fails.append(f"{tag} woke after {dtick} ticks, before its {timeout}-tick deadline")
        if dtick != dirq0:
            fails.append(f"{tag}: the tick advanced {dtick} over a window of {dirq0} CPU 0 interrupts")
        if dirq_own < 1:
            fails.append(f"{tag}: its CPU took no timer interrupt in the window")
        b = before.get(r["tid"])
        if b is None or b["cpu"] != r["cpu"]:
            fails.append(f"{tag}: armed on another CPU (no migration)")

    # idle wake — one sleeper per expected secondary
    sleepers = {r["v"][0]: r for r in user("S_START")}
    s_cpus = sorted(r["cpu"] for r in sleepers.values())
    if s_cpus != sorted(expected):
        fails.append(f"placement: sleepers ran on CPUs {s_cpus}, expected one on each of {sorted(expected)}")
    prev = {k: r["seq"] for k, r in sleepers.items()}
    for rnd in range(1, WAKE_ROUNDS + 1):
        for k in range(1, ncpus):
            tag = f"idle wake (sleeper {k}, round {rnd})"
            code = (k << 8) | rnd
            s = sleepers.get(k)
            wake = [r for r in user("P_WAKE") if r["v"][0] == code]
            woke = [r for r in user("S_WOKE") if r["v"][0] == code]
            acked = [r for r in user("P_ACKED") if r["v"][0] == code]
            if s is None or len(wake) != 1 or len(woke) != 1 or len(acked) != 1:
                fails.append(f"{tag}: start/wake/woke/acked records {int(s is not None)}/{len(wake)}/{len(woke)}/{len(acked)}")
                continue
            c, tid = s["cpu"], s["tid"]
            w, kk = wake[0]["seq"], woke[0]["seq"]
            idles = [r for r in kind("IDLE", c) if prev[k] < r["seq"] < w]
            if not idles:
                fails.append(f"{tag}: CPU {c} never went idle before the wake")
            elif [r for r in kind("IDLE_DISPATCH", c) if idles[-1]["seq"] < r["seq"] < w]:
                fails.append(f"{tag}: CPU {c} had left idle before the wake")
            ipi = [r for r in kind("IDLE_IPI", c) if w < r["seq"] < kk and r["v"][0] & 1]
            disp = [r for r in kind("IDLE_DISPATCH", c) if r["tid"] == tid and w < r["seq"] < kk]
            if not ipi:
                fails.append(f"{tag}: no IPI taken at CPU {c}'s idle boundary after the wake")
            if len(disp) != 1:
                fails.append(f"{tag}: CPU {c} left idle for the sleeper {len(disp)} times")
            elif disp[0]["v"][0] != 1:
                fails.append(f"{tag}: CPU {c} left idle for the sleeper by its timer, not the IPI")
            elif ipi and not ipi[0]["seq"] < disp[0]["seq"]:
                fails.append(f"{tag}: the dispatch preceded the IPI")
            if woke[0]["cpu"] != c:
                fails.append(f"{tag}: the sleeper woke on CPU {woke[0]['cpu']}, not its CPU {c}")
            if acked[0]["v"][1] != 1:
                fails.append(f"{tag}: the wake woke {acked[0]['v'][1]} waiters")
            prev[k] = kk

    # ipc — one IPC thread per expected secondary, in turn
    threads = {r["v"][0]: r for r in user("T_START")}
    t_cpus = sorted(r["cpu"] for r in threads.values())
    if t_cpus != sorted(expected):
        fails.append(f"placement: IPC threads ran on CPUs {t_cpus}, expected one on each of {sorted(expected)}")
    last = 0
    for k in range(1, ncpus):
        t = threads.get(k)
        for rnd in range(1, IPC_ROUNDS + 1):
            code = (k << 8) | rnd
            ping = [r for r in user("P_PING") if r["v"][0] == code]
            pong = [r for r in user("T_PONG") if r["v"][0] == code]
            tag = f"ipc (thread {k}, round {rnd})"
            if t is None or len(ping) != 1 or len(pong) != 1:
                fails.append(f"{tag}: ping/pong records {len(ping)}/{len(pong)}")
                continue
            if ping[0]["v"][1] != 1 or ping[0]["cpu"] != 0:
                fails.append(f"{tag}: the ping did not arrive intact on CPU 0")
            if pong[0]["v"][1] != 1 or pong[0]["cpu"] != t["cpu"]:
                fails.append(f"{tag}: the pong did not arrive intact on the thread's CPU {t['cpu']}")
            if not last < ping[0]["seq"] < pong[0]["seq"]:
                fails.append(f"{tag}: out of order")
            last = pong[0]["seq"]

    # churn
    child_cpus = set()
    for rnd in range(1, FORK_ROUNDS + 1):
        for k in range(1, ncpus):
            code = (rnd << 8) | k
            tag = f"churn (round {rnd}, child {k})"
            forked = [r for r in user("P_FORKED") if r["v"][1] == code]
            if len(forked) != 1:
                fails.append(f"{tag}: {len(forked)} fork records")
                continue
            child = forked[0]["v"][0]
            run = [r for r in user("C_RUN") if r["tid"] == child]
            reap = [r for r in kind("REAP") if r["tid"] == child]
            if len(run) != 1 or len(reap) != 1:
                fails.append(f"{tag}: child {child} run/reap records {len(run)}/{len(reap)}")
                continue
            c = run[0]["cpu"]
            child_cpus.add(c)
            if c == 0:
                fails.append(f"{tag}: child {child} ran on CPU 0")
            if not run[0]["seq"] < reap[0]["seq"]:
                fails.append(f"{tag}: reaped before it ran")
            child_asid = run[0]["v"][2]
            left = [r for r in kind("ROOT_LEFT", c) if r["v"][0] == child_asid
                    and run[0]["seq"] < r["seq"] < reap[0]["seq"]]
            if not left:
                fails.append(f"{tag}: CPU {c} never left child {child}'s root (asid {child_asid}) before its reap")
            _, reaped, destroyed, quarantined, _ = reap[0]["v"]
            if reaped != 1 or destroyed != 1 or quarantined != 0:
                fails.append(f"{tag}: reap process_reaped={reaped} destroyed={destroyed} quarantined={quarantined}")
    for c in sorted(expected - child_cpus):
        fails.append(f"churn: no child ever ran on CPU {c}")
    return fails


# ─────────────────────────────── self-test ───────────────────────────────

def _line(body):
    return f"{body} crc={zlib.crc32(body.encode()):08x}"


def _fixture(ncpus=3):
    """A passing record, in stamp order: (kind, cpu, tid, step, v...)."""
    sec = list(range(1, ncpus))
    mask = sum(1 << c for c in sec)
    ev = [("RELEASE", 0, 0, "-", mask, mask)]
    ev += [("AP_ADMITTED", c, 0, "-", 0) for c in sec]
    ev += [("USER", 0, 9600, "P_START", 0), ("USER", 0, 9600, "P_AP_READY", mask, len(sec))]
    tid = 10000
    dls = []
    for c in sec:
        tid += 1
        dls.append((tid, c))
        ev.append(("USER", c, tid, "D_BEFORE", 0))
    spins = []
    for c in [0] + sec + [0] + sec:
        tid += 1
        spins.append((tid, c))
        ev.append(("USER", c, tid, "W_START", 0))
    for n in range(PRE_MIN):
        for t, c in spins:
            ev.append(("PREEMPT", c, t, "-", n + 1))
    for t, c in dls:
        ev.append(("DEADLINE", c, t, "-", 40, 9, 55, 55, 56))
    for t, c in spins:
        ev.append(("USER", c, t, "W_DONE", 0, 0))
    ev.append(("USER", 0, 9600, "P_A_DONE", len(spins), 0))
    sl = {}
    for k, c in enumerate(sec, 1):
        tid += 1
        sl[k] = (tid, c)
        ev += [("IDLE_IPI", c, 0, "-", 1), ("IDLE_DISPATCH", c, tid, "-", 1), ("USER", c, tid, "S_START", k)]
    for r in range(1, WAKE_ROUNDS + 1):
        for k, (t, c) in sl.items():
            code = (k << 8) | r
            ev += [("IDLE", c, 0, "-", r + 1), ("USER", 0, 9600, "P_WAKE", code), ("IDLE_IPI", c, 0, "-", 1),
                   ("IDLE_DISPATCH", c, t, "-", 1), ("USER", c, t, "S_WOKE", code, r),
                   ("USER", 0, 9600, "P_ACKED", code, 1)]
    th = {}
    for k, c in enumerate(sec, 1):
        tid += 1
        th[k] = (tid, c)
        ev.append(("USER", c, tid, "T_START", k))
    for k, (t, c) in th.items():
        for r in range(1, IPC_ROUNDS + 1):
            code = (k << 8) | r
            ev += [("USER", 0, 9600, "P_PING", code, 1), ("USER", c, t, "T_PONG", code, 1)]
    asid = 40
    for r in range(1, FORK_ROUNDS + 1):
        for k, c in enumerate(sec, 1):
            tid += 1
            asid += 1
            code = (r << 8) | k
            ev += [("USER", 0, 9600, "P_FORKED", tid, code), ("USER", c, tid, "C_RUN", code, code, asid),
                   ("ROOT_LEFT", c, 0, "-", asid), ("IDLE", c, 0, "-", 20 + r), ("REAP", 0, tid, "-", r, 1, 1, 0)]
    ev.append(("USER", 0, 9600, "P_END", 0))
    return ev


def _render(ev, *, tear=(), drop_both=(), extra=""):
    out = ["RISCV_AP_SCHED_ADMITTED hart=1 cpu=1 mode=production"]
    for i, e in enumerate(ev, 1):
        kind, cpu, tid, step, *v = e
        v = (list(v) + [0] * 5)[:5]
        body = f"APSW_R seq={i} kind={kind} cpu={cpu} tid={tid} step={step} " + " ".join(f"v{j}={v[j]}" for j in range(5))
        copies = [_line(body), _line(body)]
        if i in tear:
            copies[0] = copies[0][:20] + "XRISCV" + copies[0][20:]
        if i in drop_both:
            copies = [c[:25] + "Z" + c[26:] for c in copies]
        out += copies
    end = f"APSW_DUMP_END count={len(ev)} overflow=0 irq=1,1,1,1,0,0,0,0 pre=4,4,4,4,0,0,0,0 admitted=0x6 reap_retries=0"
    out += [_line(end), _line(end), extra]
    return "\n".join(out)


def self_test():
    cases_run = 0
    for n in (2, 3, 4):
        base = _fixture(n)
        ok = grade(_render(base), n)
        assert ok == [], (n, ok)
        cases_run += 1
    base = _fixture(3)
    assert grade(_render(base, tear={7, 30}), 3) == [], "a torn copy with a good twin must pass"
    cases_run += 1

    def idx(pred, after=0):
        return next(i for i, e in enumerate(base) if i >= after and pred(e))

    def mutate(i, new):
        ev = list(base)
        ev[i] = new
        return ev

    def drop(i):
        ev = list(base)
        del ev[i]
        return ev

    def without(pred):
        return [e for e in base if not pred(e)]

    wake1 = idx(lambda e: e[3] == "P_WAKE")
    w2 = idx(lambda e: e[3] == "W_DONE" and e[1] == 2)
    cases = {
        "both copies torn": (_render(base, drop_both={12}), 3),
        "pinned diagnostic admission": (_render(base, extra="RISCV_SMP3_SECONDARY_ADMITTED hart=1 cpu=1"), 3),
        "an expected CPU never admitted": (_render(without(lambda e: e[0] == "AP_ADMITTED" and e[1] == 2)), 3),
        "a released CPU missing from the plan": (_render(mutate(0, ("RELEASE", 0, 0, "-", 0b10, 0b110))), 3),
        "a CPU quiescent on the console": (_render(base, extra="RISCV_AP_QUIESCENT cpu=2 reason=x"), 3),
        "the boot had more CPUs than the run covered": (_render(base), 4),
        "an expected CPU ran no spinner": (_render([e if not (e[3] == "W_START" and e[1] == 2) else
                                                   ("USER", 1, e[2], "W_START", 0) for e in base]), 3),
        "quarantined teardown on console": (_render(base, extra="VM_ASPACE_TEARDOWN_QUARANTINED pid=1 asid=5"), 3),
        "corrupted register": (_render(mutate(w2, ("USER", 2, base[w2][2], "W_DONE", 1, 17))), 3),
        "spinner migrated": (_render(mutate(w2, ("USER", 1, base[w2][2], "W_DONE", 0, 0))), 3),
        "no secondary timer preemption on CPU 2": (_render(without(lambda e: e[0] == "PREEMPT" and e[1] == 2)), 3),
        "clock multiplied": (_render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                            ("DEADLINE", 1, base[idx(lambda e: e[0] == "DEADLINE")][2], "-", 40, 9, 111, 55, 56))), 3),
        "deadline not timed out": (_render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                                  ("DEADLINE", 1, base[idx(lambda e: e[0] == "DEADLINE")][2], "-", 40, 0, 55, 55, 56))), 3),
        "no deadline on CPU 2": (_render(without(lambda e: e[0] == "DEADLINE" and e[1] == 2)), 3),
        "wake by timer": (_render(mutate(idx(lambda e: e[0] == "IDLE_DISPATCH", wake1),
                                         base[idx(lambda e: e[0] == "IDLE_DISPATCH", wake1)][:4] + (0,))), 3),
        "wake without IPI": (_render(drop(idx(lambda e: e[0] == "IDLE_IPI", wake1))), 3),
        "target not idle at wake": (_render(drop(idx(lambda e: e[0] == "IDLE", wake1 - 2))), 3),
        "pong corrupted": (_render(mutate(idx(lambda e: e[3] == "T_PONG"),
                                          base[idx(lambda e: e[3] == "T_PONG")][:5] + (0,))), 3),
        "child on CPU 0": (_render(mutate(idx(lambda e: e[3] == "C_RUN"),
                                          ("USER", 0) + base[idx(lambda e: e[3] == "C_RUN")][2:])), 3),
        "reap quarantined": (_render(mutate(idx(lambda e: e[0] == "REAP"),
                                            base[idx(lambda e: e[0] == "REAP")][:5] + (1, 0, 1))), 3),
        "no root release before reap": (_render(drop(idx(lambda e: e[0] == "ROOT_LEFT"))), 3),
        "failure step": (_render(base[:-1] + [("USER", 0, 9600, "P_NO_ACK", 3), base[-1]]), 3),
        "no end": (_render(base[:-1]), 3),
        "a one-CPU boot": (_render(base), 1),
        "a topology beyond the witness layout": (_render(base), 9),
        "an unadmitted CPU and no trailer": (_render(base[:5]).split("APSW_DUMP_END")[0]
                                              + "\nRISCV_AP_STATE cpu=2 discovered=1 initialized=1 irq_ready=0 admitted=0 dispatching=0 reason=no_irq_ready", 3),
        "the mailbox split copy-on-write": (_render(base, extra="APSW_PROVISIONED p_tid=9600 asid=4 image=1\n"
                                                     "VM_COW_SPLIT_COMMITTED cpu=0 tid=9600 asid=4 va=0x20070000 old_pa=0x1 new_pa=0x2 acked=1"), 3),
    }
    for name, (text, n) in cases.items():
        got = grade(text, n)
        assert got, f"self-test: the '{name}' control passed"
    print(f"APSW_GRADER_SELF_TEST cases={len(cases) + cases_run} result=ok")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    args = sys.argv[1:]
    if len(args) != 3 or args[0] != "--expect-cpus":
        print("usage: grade-riscv64-apsched-witness.py --expect-cpus N BOOT_LOG")
        return 2
    ncpus = int(args[1])
    text = open(args[2], errors="replace").read()
    fails = grade(text, ncpus)
    for f in fails:
        print(f"[apsched-witness][fail] {f}")
    if fails:
        print(f"APSW_WITNESS_SEAL arch=riscv64 cpus={ncpus} result=fail")
        return 1
    recs, end, _ = parse(text)
    R = sorted(recs.values(), key=lambda r: int(r["seq"]))
    starts = {r["tid"] for r in R if r["kind"] == "USER" and r["step"] == "W_START"}
    pre = [sum(1 for r in R if r["kind"] == "PREEMPT" and r["tid"] in starts and int(r["cpu"]) == c)
           for c in range(ncpus)]
    dl = [r for r in R if r["kind"] == "DEADLINE"]
    dtick = ",".join(f"{r['cpu']}:{r['v2']}={r['v3']}/{r['v4']}" for r in sorted(dl, key=lambda r: int(r["cpu"])))
    print(f"APSW_WITNESS_SEAL arch=riscv64 cpus={ncpus} admission=production per_cpu_work=threads,forks "
          f"preempt_recorded={','.join(map(str, pre))} preempt_counted={end.get('pre', '?')} registers=intact deadlines=timed_out dtick=dirq0/own:{dtick} "
          f"idle_wakes={WAKE_ROUNDS * (ncpus - 1)}_ipi ipc={IPC_ROUNDS * (ncpus - 1)} "
          f"churn={FORK_ROUNDS * (ncpus - 1)}_reaped_quarantined=0 records={end['count']} result=ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
