#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED1 — grade the RISC-V production AP-scheduling witness.
#
# Usage: grade-riscv64-apsched-witness.py BOOT_LOG
#        grade-riscv64-apsched-witness.py --self-test
#
# The witness keeps its records in the kernel and dumps them at P's last step, every line twice
# with a CRC-32 of its body. Only lines whose CRC verifies are evidence (the two harts' console
# output interleaves, and a torn line is not a record); both copies of one stamp must agree. The
# grade is by causal stamp, never by console position.
#
# Obligations (each failure names the one it broke):
#   admission   CPU 1 took the PRODUCTION admission (its record), and the diagnostic pinned path
#               never ran;
#   placement   P ran on CPU 0; spinners started on both CPUs; the sleeper, the IPC thread and
#               every churn child ran on CPU 1 — all placed by the production policy;
#   preemption  every spinner that started on CPU 1 was preempted there by CPU 1's timer, CPU 1
#               took at least six such preemptions, and every spinner finished on the CPU it
#               started on (no migration) with all 28 patterned registers intact;
#   time        the deadline receive timed out (9) after at least its timeout in scheduler ticks,
#               the tick advanced by exactly CPU 0's timer interrupts over the window, and CPU 1
#               took interrupts of its own in it — a second CPU did not speed time up;
#   idle wake   every round: CPU 1 idle (an idle arrival, not left since) when P stored F; then an
#               IPI from CPU 0 taken at CPU 1's idle boundary; then CPU 1 leaving idle for the
#               sleeper BY that IPI; then the sleeper's acknowledgement on CPU 1; one waiter woken;
#   ipc         every round's ping arrived intact on CPU 0 and its pong intact on CPU 1, in order;
#   churn       every child: ran on CPU 1; CPU 1 then left THAT child's root (the exit's own
#               transition, before the exit became reapable); and only then reaped, with its address
#               space destroyed and none quarantined;
#   progress    P reached its end with no failure step; the dump is complete and did not overflow.
import re
import sys
import zlib

SPINNERS = 4
PRE1_MIN = 6
WAKE_ROUNDS = 5
IPC_ROUNDS = 6
FORK_ROUNDS = 6
TIMED_OUT = 9
FAIL_STEPS = {
    "P_NO_AP", "P_PRE_TIMEOUT", "P_A_TIMEOUT", "P_B_TIMEOUT", "P_NO_IDLE", "P_NO_ACK",
    "P_PING_RAW", "P_C_TIMEOUT", "P_REAP_TIMEOUT", "P_SPAWN_FAIL", "P_FORK_FAIL", "EXIT_REFUSED",
    "?",
}
FORBIDDEN = [
    "panicked at", "KERNEL PANIC", "RISCV_SMP3_SECONDARY_ADMITTED", "RISCV_AP_SCHED_ADMISSION_REFUSED",
    "RISCV_SMP3_SECONDARY_UNADMITTED", "TIMER_SPLIT_CURRENT_NOT_RUNNING",
    "VM_ASPACE_TEARDOWN_QUARANTINED",
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


def grade(text):
    fails = []
    recs, end, conflicts = parse(text)
    if conflicts:
        fails.append(f"progress: two verified copies disagree for stamps {conflicts[:5]}")
    if end is None:
        return [f"progress: no verified dump trailer (P never reached its end)"] + fails
    count = int(end["count"])
    if end.get("overflow") != "0":
        fails.append("progress: the record overflowed")
    missing = [s for s in range(1, count + 1) if s not in recs]
    if missing:
        fails.append(f"progress: no verified copy of stamps {missing[:8]}")
    R = [dict(r, seq=int(r["seq"]), cpu=int(r["cpu"]), tid=int(r["tid"]),
              v=[int(r[f"v{i}"]) for i in range(5)]) for _, r in sorted(recs.items())]

    def user(step, **kw):
        out = [r for r in R if r["kind"] == "USER" and r["step"] == step]
        for k, val in kw.items():
            out = [r for r in out if (r["v"][0] if k == "v0" else r["v"][1] if k == "v1" else r[k]) == val]
        return out

    for bad in FORBIDDEN:
        if bad in text:
            fails.append(f"forbidden console marker: {bad}")

    # admission
    if not [r for r in R if r["kind"] == "AP_ADMITTED" and r["cpu"] == 1]:
        fails.append("admission: CPU 1 never took the production admission")

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
    if len(starts) != SPINNERS:
        fails.append(f"placement: {len(starts)} spinners started, expected {SPINNERS}")
    cpus = {r["cpu"] for r in starts}
    if cpus != {0, 1}:
        fails.append(f"placement: spinners started only on CPUs {sorted(cpus)}")
    pre = [r for r in R if r["kind"] == "PREEMPT"]
    spin_tids = {r["tid"] for r in starts}
    pre1 = [r for r in pre if r["cpu"] == 1 and r["tid"] in spin_tids]
    if len(pre1) < PRE1_MIN:
        fails.append(f"preemption: {len(pre1)} timer preemptions of spinners on CPU 1, need {PRE1_MIN}")
    for s in starts:
        if s["cpu"] == 1 and not [r for r in pre1 if r["tid"] == s["tid"]]:
            fails.append(f"preemption: spinner {s['tid']} on CPU 1 was never preempted by CPU 1's timer")
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

    # time
    dl = [r for r in R if r["kind"] == "DEADLINE"]
    if len(dl) != 1:
        fails.append(f"time: {len(dl)} deadline records")
    else:
        timeout, result, dtick, dirq0, dirq1 = dl[0]["v"]
        if result != TIMED_OUT:
            fails.append(f"time: the deadline receive returned {result}, not TimedOut")
        if dtick < timeout:
            fails.append(f"time: woke after {dtick} ticks, before its {timeout}-tick deadline")
        if dtick != dirq0:
            fails.append(f"time: the tick advanced {dtick} over a window of {dirq0} CPU 0 interrupts")
        if dirq1 < 1:
            fails.append("time: CPU 1 took no timer interrupt in the deadline window")

    # idle wake
    s_start = user("S_START")
    if not s_start or s_start[0]["cpu"] != 1:
        fails.append("placement: the sleeper did not run on CPU 1")
    s_tid = s_start[0]["tid"] if s_start else None
    prev = s_start[0]["seq"] if s_start else 0
    for rnd in range(1, WAKE_ROUNDS + 1):
        wake = user("P_WAKE", v0=rnd)
        woke = [r for r in user("S_WOKE", v0=rnd) if r["tid"] == s_tid]
        acked = user("P_ACKED", v0=rnd)
        if len(wake) != 1 or len(woke) != 1 or len(acked) != 1:
            fails.append(f"idle wake {rnd}: wake/woke/acked records {len(wake)}/{len(woke)}/{len(acked)}")
            continue
        w, k = wake[0]["seq"], woke[0]["seq"]
        idles = [r for r in R if r["kind"] == "IDLE" and r["cpu"] == 1 and prev < r["seq"] < w]
        if not idles:
            fails.append(f"idle wake {rnd}: CPU 1 never went idle before the wake")
        elif [r for r in R if r["kind"] == "IDLE_DISPATCH" and r["cpu"] == 1 and idles[-1]["seq"] < r["seq"] < w]:
            fails.append(f"idle wake {rnd}: CPU 1 had left idle before the wake")
        ipi = [r for r in R if r["kind"] == "IDLE_IPI" and r["cpu"] == 1 and w < r["seq"] < k and r["v"][0] & 1]
        disp = [r for r in R if r["kind"] == "IDLE_DISPATCH" and r["cpu"] == 1 and r["tid"] == s_tid and w < r["seq"] < k]
        if not ipi:
            fails.append(f"idle wake {rnd}: no IPI from CPU 0 taken at CPU 1's idle boundary after the wake")
        if len(disp) != 1:
            fails.append(f"idle wake {rnd}: CPU 1 left idle for the sleeper {len(disp)} times")
        elif disp[0]["v"][0] != 1:
            fails.append(f"idle wake {rnd}: CPU 1 left idle for the sleeper by its timer, not the IPI")
        elif ipi and not ipi[0]["seq"] < disp[0]["seq"]:
            fails.append(f"idle wake {rnd}: the dispatch preceded the IPI")
        if woke[0]["cpu"] != 1:
            fails.append(f"idle wake {rnd}: the sleeper woke on CPU {woke[0]['cpu']}")
        if acked[0]["v"][1] != 1:
            fails.append(f"idle wake {rnd}: the wake woke {acked[0]['v'][1]} waiters")
        prev = k

    # ipc
    t_start = user("T_START")
    if not t_start or t_start[0]["cpu"] != 1:
        fails.append("placement: the IPC thread did not run on CPU 1")
    last = 0
    for rnd in range(1, IPC_ROUNDS + 1):
        ping, pong = user("P_PING", v0=rnd), user("T_PONG", v0=rnd)
        if len(ping) != 1 or len(pong) != 1:
            fails.append(f"ipc {rnd}: ping/pong records {len(ping)}/{len(pong)}")
            continue
        if ping[0]["v"][1] != 1 or ping[0]["cpu"] != 0:
            fails.append(f"ipc {rnd}: the ping did not arrive intact on CPU 0")
        if pong[0]["v"][1] != 1 or pong[0]["cpu"] != 1:
            fails.append(f"ipc {rnd}: the pong did not arrive intact on CPU 1")
        if not last < ping[0]["seq"] < pong[0]["seq"]:
            fails.append(f"ipc {rnd}: out of order")
        last = pong[0]["seq"]

    # churn
    for rnd in range(1, FORK_ROUNDS + 1):
        forked = user("P_FORKED", v1=rnd)
        if len(forked) != 1:
            fails.append(f"churn {rnd}: {len(forked)} fork records")
            continue
        child = forked[0]["v"][0]
        run = [r for r in user("C_RUN") if r["tid"] == child]
        reap = [r for r in R if r["kind"] == "REAP" and r["tid"] == child]
        if len(run) != 1 or len(reap) != 1:
            fails.append(f"churn {rnd}: child {child} run/reap records {len(run)}/{len(reap)}")
            continue
        if run[0]["cpu"] != 1:
            fails.append(f"churn {rnd}: child {child} ran on CPU {run[0]['cpu']}")
        # The parent reports P_FORKED after the fork committed; the child may already have run.
        if not run[0]["seq"] < reap[0]["seq"]:
            fails.append(f"churn {rnd}: reaped before it ran")
        child_asid = run[0]["v"][2]
        left = [r for r in R if r["kind"] == "ROOT_LEFT" and r["cpu"] == 1 and r["v"][0] == child_asid
                and run[0]["seq"] < r["seq"] < reap[0]["seq"]]
        if not left:
            fails.append(f"churn {rnd}: CPU 1 never left child {child}'s root (asid {child_asid}) before its reap")
        _, reaped, destroyed, quarantined, _ = reap[0]["v"]
        if reaped != 1 or destroyed != 1 or quarantined != 0:
            fails.append(f"churn {rnd}: reap process_reaped={reaped} destroyed={destroyed} quarantined={quarantined}")
    return fails


# ─────────────────────────────── self-test ───────────────────────────────

def _line(body):
    return f"{body} crc={zlib.crc32(body.encode()):08x}"


def _fixture():
    """A passing record, in stamp order: (kind, cpu, tid, step, v...)."""
    ev = [("AP_ADMITTED", 1, 0, "-", 0), ("IDLE", 1, 0, "-", 1), ("USER", 0, 9600, "P_START", 0),
          ("USER", 0, 9600, "P_AP_READY", 0), ("USER", 1, 10008, "D_BEFORE", 0)]
    spins = [(10009, 1), (10010, 0), (10011, 1), (10012, 0)]
    for tid, cpu in spins:
        ev.append(("USER", cpu, tid, "W_START", 0))
    for n in range(4):
        ev.append(("PREEMPT", 1, 10009, "-", n + 1))
        ev.append(("PREEMPT", 1, 10011, "-", n + 5))
        ev.append(("PREEMPT", 0, 10010, "-", n + 1))
    ev.append(("DEADLINE", 1, 10008, "-", 40, 9, 55, 55, 56))
    for tid, cpu in spins:
        ev.append(("USER", cpu, tid, "W_DONE", 0, 0))
    ev.append(("USER", 0, 9600, "P_A_DONE", 4, 0))
    ev.append(("IDLE_IPI", 1, 0, "-", 1))
    ev.append(("IDLE_DISPATCH", 1, 10013, "-", 1))
    ev.append(("USER", 1, 10013, "S_START", 0))
    for r in range(1, WAKE_ROUNDS + 1):
        ev += [("IDLE", 1, 0, "-", r + 1), ("USER", 0, 9600, "P_WAKE", r), ("IDLE_IPI", 1, 0, "-", 1),
               ("IDLE_DISPATCH", 1, 10013, "-", 1), ("USER", 1, 10013, "S_WOKE", r, r),
               ("USER", 0, 9600, "P_ACKED", r, 1)]
    ev += [("IDLE_IPI", 1, 0, "-", 1), ("IDLE_DISPATCH", 1, 10014, "-", 1), ("USER", 1, 10014, "T_START", 0)]
    for r in range(1, IPC_ROUNDS + 1):
        ev += [("USER", 0, 9600, "P_PING", r, 1), ("USER", 1, 10014, "T_PONG", r, 1)]
    for r in range(1, FORK_ROUNDS + 1):
        c = 10014 + r
        ev += [("USER", 0, 9600, "P_FORKED", c, r), ("USER", 1, c, "C_RUN", r, r, 40 + r),
               ("ROOT_LEFT", 1, 0, "-", 40 + r), ("IDLE", 1, 0, "-", 20 + r), ("REAP", 0, c, "-", r, 1, 1, 0)]
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
    end = f"APSW_DUMP_END count={len(ev)} overflow=0 irq0=1 irq1=1 pre0=4 pre1=8 idle1=12 reap_retries=0"
    out += [_line(end), _line(end), extra]
    return "\n".join(out)


def self_test():
    base = _fixture()
    ok = grade(_render(base))
    assert ok == [], ok
    # One torn copy: the other verifies.
    assert grade(_render(base, tear={7, 30})) == [], "a torn copy with a good twin must pass"

    def idx(pred, after=0):
        return next(i for i, e in enumerate(base) if i >= after and pred(e))

    wake1 = idx(lambda e: e[3] == "P_WAKE")

    def mutate(i, new):
        ev = list(base)
        ev[i] = new
        return ev

    def drop(i):
        ev = list(base)
        del ev[i]
        return ev

    cases = {
        "both copies torn": _render(base, drop_both={12}),
        "pinned diagnostic admission": _render(base, extra="RISCV_SMP3_SECONDARY_ADMITTED hart=1 cpu=1"),
        "no production admission": _render(drop(0)),
        "quarantined teardown on console": _render(base, extra="VM_ASPACE_TEARDOWN_QUARANTINED pid=1 asid=5"),
        "corrupted register": _render(mutate(idx(lambda e: e[3] == "W_DONE" and e[2] == 10011),
                                             ("USER", 1, 10011, "W_DONE", 1, 17))),
        "spinner migrated": _render(mutate(idx(lambda e: e[3] == "W_DONE" and e[2] == 10009),
                                           ("USER", 0, 10009, "W_DONE", 0, 0))),
        "no AP timer preemption": _render([e for e in base if not (e[0] == "PREEMPT" and e[1] == 1)]),
        "clock multiplied": _render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                           ("DEADLINE", 1, 10008, "-", 40, 9, 111, 55, 56))),
        "deadline not timed out": _render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                                 ("DEADLINE", 1, 10008, "-", 40, 0, 55, 55, 56))),
        "wake by timer": _render(mutate(idx(lambda e: e[0] == "IDLE_DISPATCH", wake1),
                                        ("IDLE_DISPATCH", 1, 10013, "-", 0))),
        "wake without IPI": _render(drop(idx(lambda e: e[0] == "IDLE_IPI", wake1))),
        "CPU 1 not idle at wake": _render(drop(idx(lambda e: e[0] == "IDLE" and e[4] == 3))),
        "pong corrupted": _render(mutate(idx(lambda e: e[3] == "T_PONG" and e[4] == 3),
                                         ("USER", 1, 10014, "T_PONG", 3, 0))),
        "child on CPU 0": _render(mutate(idx(lambda e: e[3] == "C_RUN" and e[4] == 2),
                                         ("USER", 0, 10016, "C_RUN", 2, 2))),
        "reap quarantined": _render(mutate(idx(lambda e: e[0] == "REAP" and e[4] == 4),
                                           ("REAP", 0, 10018, "-", 4, 1, 0, 1))),
        "no root release before reap": _render(drop(idx(lambda e: e[0] == "ROOT_LEFT" and e[4] == 45))),
        "another root released": _render(mutate(idx(lambda e: e[0] == "ROOT_LEFT" and e[4] == 43),
                                                ("ROOT_LEFT", 1, 0, "-", 4))),
        "failure step": _render(base[:-1] + [("USER", 0, 9600, "P_NO_IDLE", 3), base[-1]]),
        "no end": _render(base[:-1]),
    }
    for name, text in cases.items():
        got = grade(text)
        assert got, f"self-test: the '{name}' control passed"
    print(f"APSW_GRADER_SELF_TEST cases={len(cases) + 2} result=ok")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    text = open(sys.argv[1], errors="replace").read()
    fails = grade(text)
    for f in fails:
        print(f"[apsched-witness][fail] {f}")
    if fails:
        print("APSW_WITNESS_SEAL arch=riscv64 result=fail")
        return 1
    recs, end, _ = parse(text)
    R = sorted(recs.values(), key=lambda r: int(r["seq"]))
    pre1 = sum(1 for r in R if r["kind"] == "PREEMPT" and r["cpu"] == "1")
    pre0 = sum(1 for r in R if r["kind"] == "PREEMPT" and r["cpu"] == "0")
    dl = next(r for r in R if r["kind"] == "DEADLINE")
    print(f"APSW_WITNESS_SEAL arch=riscv64 admission=production cpu1_work=threads,forks preempt_cpu1={pre1} "
          f"preempt_cpu0={pre0} registers=intact deadline=timed_out dtick={dl['v2']}=dirq0 dirq1={dl['v4']} "
          f"idle_wakes={WAKE_ROUNDS}/{WAKE_ROUNDS}_ipi ipc={IPC_ROUNDS}/{IPC_ROUNDS} "
          f"churn={FORK_ROUNDS}/{FORK_ROUNDS}_reaped_quarantined=0 records={end['count']} result=ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
