#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""RISCV-ECALL-A7-RET1 — grade one boot of the live RISC-V ecall register-contract witness.

Input: the normalized serial log of a boot built with `riscv-ecall-witness` (kernel and init).

Every probed `ecall` is logged by userspace as `EA7W role=.. cycle=.. key=.. nr=.. a0.. a3 a7
want_a3 want_a7 bad pc` — `bad` a mask of every register the contract preserves that the probe did
NOT get back (a3..a7, s2..s7, t3..t6, sp, tp, gp) — and by the kernel as
`RISCV_ECALL_WITNESS_RESUME origin=.. arm=.. tid=.. key=.. sepc=.. a0 a3 a7`, once per keyed
write-back (arm `same` | `switched` | `continuation` | `async`).

A boot passes only if ALL of these hold (each failure is reported; nothing is repaired):

* the core smoke passed (`--core-rc 0`);
* every witness line parses exactly; a line that names a witness marker but does not parse (two
  harts' output interleaved, a truncated line) FAILS the boot — it is never re-read or repaired;
* `EA7W_BEGIN` and `EA7W_DONE ... result=ok` appear exactly once;
* every role's first registers (`EA7W_FRESH`, one per role, nine roles) are all zero — a spawned
  thread's startup arguments are zero and it has issued no ecall, so `a7 = 0`;
* every role logs every cycle exactly once (no missing, no duplicate key);
* every probe has `bad = 0`, and its result is the class's: Yield `0`, FutexWait `0` or `1`, the
  requeued wait `1`, the receive `0` with its message;
* every probe has EXACTLY ONE non-async kernel resume for its key, whose `sepc` is the probe's
  `ecall` + 4 and whose `a0`, `a3`, `a7` are what userspace read back (async resumes of the same
  key — a timer inside the probe's own instructions — are allowed and counted);
* no kernel marker names a probe key that userspace never reported;
* coverage: a Yield resumed by a SWITCH and one through its own trap; a FutexWait that genuinely
  parked (`1`, resumed by another arm than `same`), one woken before it physically parked (`1`
  through its own frame), and the deliberate mismatches (every fourth cycle: `0`, own frame); every
  requeue round moved the waiter, woke it and saw it back; a receive resumed after blocking; a
  register spin resumed by the `async` arm, every spin intact.
"""
import argparse
import re
import sys

ROLES = ["Y0", "Y1", "Y2", "W", "S", "RW", "RQ", "R", "P"]
CALLS = {"Y0": 32, "Y1": 24, "Y2": 24, "W": 48, "RW": 8, "R": 8}
NR = {"Y0": 0, "Y1": 0, "Y2": 0, "W": 9, "RW": 9, "R": 2}
RCALLS = 8
PCALLS = 3
HX = r"0x([0-9a-f]+)"

K_RE = re.compile(
    r"RISCV_ECALL_WITNESS_RESUME origin=(bridge|idle) arm=(same|switched|continuation|async) "
    r"tid=(\d+) key=" + HX + " sepc=" + HX + " a0=" + HX + " a3=" + HX + " a7=" + HX + r"\s*$")
U_RE = re.compile(
    r"EA7W role=(\w+) cycle=(\d+) key=" + HX + r" nr=(\d+) a0=" + HX + " a1=" + HX + " a2=" + HX
    + " a3=" + HX + " a7=" + HX + " want_a3=" + HX + " want_a7=" + HX + " bad=" + HX + " pc=" + HX
    + r"\s*$")
FRESH_RE = re.compile(r"EA7W_FRESH role=(\S+)" + "".join(f" a{i}=" + HX for i in range(8)) + r"\s*$")
RQ_RE = re.compile(r"EA7W_RQ cycle=(\d+) (?:requeued=(\d+) woke=(\d+) back=(\d)|result=fail reason=\w+)\s*$")
IPC_RE = re.compile(r"EA7W_IPC cycle=(\d+) a0=" + HX + " status=" + HX + r" len=(\d+) body_ok=(\d)\s*$")
SPIN_RE = re.compile(r"EA7W_SPIN cycle=(\d+) key=" + HX + " bad=" + HX + r"\s*$")
BEGIN_RE = re.compile(r"EA7W_BEGIN s_tid=\d+ roles=9 ep=\d+\s*$")
DONE_RE = re.compile(r"EA7W_DONE (?:spawned=(\d+) done=(\d+) sent=(\d+) bad=(\d+) result=(ok|fail)|result=fail reason=\w+)\s*$")
FRAGMENTS = ("EA7W", "RISCV_ECALL_WITNESS")
PARSERS = [("EA7W_FRESH", FRESH_RE), ("EA7W_RQ", RQ_RE), ("EA7W_IPC", IPC_RE), ("EA7W_SPIN", SPIN_RE),
           ("EA7W_BEGIN", BEGIN_RE), ("EA7W_DONE", DONE_RE), ("EA7W role=", U_RE),
           ("RISCV_ECALL_WITNESS_RESUME", K_RE)]
KEY_HI = 0xEA7C0DE5


def role_of_key(key):
    return (key >> 24) & 0xFF, key & 0xFFFFFF


def grade(lines, core_rc):
    fails = []
    kern, users, fresh, rq, ipc, spin, begins, dones = [], [], [], [], [], [], [], []
    for n, line in enumerate(lines, 1):
        if not any(f in line for f in FRAGMENTS):
            continue
        parsed = False
        for tag, rx in PARSERS:
            i = line.find(tag)
            if i < 0:
                continue
            m = rx.match(line[i:])
            if not m:
                continue
            parsed = True
            g = m.groups()
            if rx is K_RE:
                kern.append(dict(n=n, origin=g[0], arm=g[1], tid=int(g[2]), key=int(g[3], 16),
                                 sepc=int(g[4], 16), a0=int(g[5], 16), a3=int(g[6], 16),
                                 a7=int(g[7], 16)))
            elif rx is U_RE:
                users.append(dict(n=n, role=g[0], cycle=int(g[1]), key=int(g[2], 16), nr=int(g[3]),
                                  a0=int(g[4], 16), a3=int(g[7], 16), a7=int(g[8], 16),
                                  want_a3=int(g[9], 16), want_a7=int(g[10], 16), bad=int(g[11], 16),
                                  pc=int(g[12], 16)))
            elif rx is FRESH_RE:
                fresh.append((g[0], [int(x, 16) for x in g[1:]]))
            elif rx is RQ_RE:
                rq.append(g)
            elif rx is IPC_RE:
                ipc.append(g)
            elif rx is SPIN_RE:
                spin.append((int(g[0]), int(g[1], 16), int(g[2], 16)))
            elif rx is BEGIN_RE:
                begins.append(n)
            elif rx is DONE_RE:
                dones.append(g)
            break
        if not parsed:
            fails.append(f"line {n}: damaged witness marker: {line.strip()[:160]}")
    if core_rc != 0:
        fails.append(f"the standard core smoke rejected this boot (rc={core_rc})")
    if len(begins) != 1:
        fails.append(f"EA7W_BEGIN seen {len(begins)} times")
    if len(dones) != 1 or dones[0][4] != "ok":
        fails.append(f"EA7W_DONE must appear once with result=ok: {dones}")
    # Fresh entry registers.
    seen_roles = sorted(r for r, _ in fresh)
    if seen_roles != sorted(ROLES):
        fails.append(f"fresh entries {seen_roles}, want one per role {sorted(ROLES)}")
    for r, regs in fresh:
        if any(regs):
            fails.append(f"role {r}: a fresh thread started with non-zero a0..a7 {regs}")
    # Probes.
    by_key = {}
    for k in kern:
        if k["key"] >> 32 != KEY_HI:
            fails.append(f"line {k['n']}: kernel key without the witness prefix")
        by_key.setdefault(k["key"], []).append(k)
    ukeys = {}
    cov = dict(solo_switched=0, y_switch=0, y_same=0, w_parked=0, w_own=0, w_early=0, w_mismatch=0, rw_parked=0, r_blocked=0, spin_async=0,
               lost_a3=0, lost_a7=0, lost_other=0, async_in_probe=0)
    for u in users:
        if u["key"] in ukeys:
            fails.append(f"key 0x{u['key']:x} reported twice (lines {ukeys[u['key']]} and {u['n']})")
            continue
        ukeys[u["key"]] = u["n"]
        role, want = u["role"], NR.get(u["role"])
        if want is None or u["nr"] != want:
            fails.append(f"line {u['n']}: role {role} nr {u['nr']}")
            continue
        ri, cyc = role_of_key(u["key"])
        if ri >= len(ROLES) or ROLES[ri] != role or cyc != u["cycle"] or u["key"] >> 32 != KEY_HI:
            fails.append(f"line {u['n']}: key 0x{u['key']:x} does not name role {role} cycle {u['cycle']}")
        if u["bad"]:
            names = [n for b, n in [(3, "a3"), (4, "a4"), (5, "a5"), (6, "a6"), (7, "a7")] if u["bad"] >> b & 1]
            names += [f"s{2+i}" for i in range(6) if u["bad"] >> (8 + i) & 1]
            names += [f"t{3+i}" for i in range(4) if u["bad"] >> (14 + i) & 1]
            names += [n for b, n in [(18, "sp"), (19, "tp"), (20, "gp")] if u["bad"] >> b & 1]
            fails.append(f"{role} cycle {u['cycle']}: the ecall did not preserve {','.join(names)} "
                         f"(a3=0x{u['a3']:x} want 0x{u['want_a3']:x}, a7=0x{u['a7']:x} want 0x{u['want_a7']:x})")
            cov["lost_a3"] += u["bad"] >> 3 & 1
            cov["lost_a7"] += u["bad"] >> 7 & 1
            cov["lost_other"] += 1 if u["bad"] & ~((1 << 3) | (1 << 7)) else 0
        ok_result = {"Y0": {0}, "Y1": {0}, "Y2": {0}, "W": {0, 1}, "RW": {1}, "R": {0}}[role]
        if u["a0"] not in ok_result:
            fails.append(f"{role} cycle {u['cycle']}: result a0=0x{u['a0']:x}, want one of {sorted(ok_result)}")
        ks = by_key.get(u["key"], [])
        real = [k for k in ks if k["arm"] != "async"]
        cov["async_in_probe"] += len(ks) - len(real)
        if len(real) != 1:
            fails.append(f"{role} cycle {u['cycle']}: {len(real)} kernel resumes for key 0x{u['key']:x} (want exactly 1)")
            continue
        k = real[0]
        if k["sepc"] != u["pc"] + 4:
            fails.append(f"{role} cycle {u['cycle']}: resumed at 0x{k['sepc']:x}, not after its ecall at 0x{u['pc']:x}")
        if (k["a0"], k["a3"], k["a7"]) != (u["a0"], u["a3"], u["a7"]):
            fails.append(f"{role} cycle {u['cycle']}: the kernel wrote a0/a3/a7 "
                         f"{k['a0']:x}/{k['a3']:x}/{k['a7']:x}, ring 3 read {u['a0']:x}/{u['a3']:x}/{u['a7']:x}")
        own = k["arm"] == "same"
        if role.startswith("Y"):
            cov["y_same" if own else "y_switch"] += 1
            if role == "Y0" and u["cycle"] > 24 and not own:
                cov["solo_switched"] += 1
        elif role == "W":
            if u["a0"] == 1 and not own:
                cov["w_parked"] += 1
            if own:
                cov["w_own"] += 1
            if own and u["a0"] == 1:
                cov["w_early"] += 1
            if u["cycle"] % 4 == 0:
                if (u["a0"], own) != (0, True):
                    fails.append(f"W cycle {u['cycle']}: a deliberate mismatch returned a0={u['a0']} through arm {k['arm']}")
                else:
                    cov["w_mismatch"] += 1
        elif role == "RW" and u["a0"] == 1 and not own:
            cov["rw_parked"] += 1
        elif role == "R" and not own:
            cov["r_blocked"] += 1
    for role, n in CALLS.items():
        got = sorted(u["cycle"] for u in users if u["role"] == role)
        if got != list(range(1, n + 1)):
            fails.append(f"role {role}: cycles {got[:4]}..({len(got)}), want 1..{n} once each")
    spin_keys = {key for _, key, _ in spin}
    for key, ks in by_key.items():
        if key not in ukeys and key not in spin_keys:
            fails.append(f"kernel resume for key 0x{key:x} that no probe reported (line {ks[0]['n']})")
    # Requeue, IPC, spin.
    if len(rq) != RCALLS or any(g[1] != "1" or g[2] != "1" or g[3] != "1" for g in rq):
        fails.append(f"requeue rounds: {rq} (want {RCALLS} rounds requeued=1 woke=1 back=1)")
    if len(ipc) != CALLS["R"] or any(g[4] != "1" for g in ipc):
        fails.append(f"receive rounds: {ipc} (want every message received)")
    if len(spin) != PCALLS or any(bad for _, _, bad in spin):
        fails.append(f"register spins: {spin} (want {PCALLS}, every register intact)")
    for _, key, _ in spin:
        cov["spin_async"] += sum(1 for k in by_key.get(key, []) if k["arm"] == "async")
    for name, label in [("y_switch", "a Yield resumed by a switch"), ("y_same", "a Yield through its own trap"),
                        ("w_parked", "a FutexWait that genuinely parked"),
                        ("w_own", "a FutexWait answered through its own frame"),
                        ("w_early", "a FutexWait woken before it physically parked (1 through its own frame)"),
                        ("w_mismatch", "a FutexWait mismatch"),
                        ("rw_parked", "a requeued wait resumed after the target wake"),
                        ("r_blocked", "a receive resumed after blocking"),
                        ("spin_async", "a timer-preempted register spin")]:
        if cov[name] == 0:
            fails.append(f"coverage: no {label}")
    return fails, cov, len(users), len(fresh)


def fixture():
    """A synthetic passing boot (used by --self-test)."""
    out = ["EA7W_BEGIN s_tid=4 roles=9 ep=7"]
    for r in ROLES:
        out.append("USER_LOG tid=1 msg=EA7W_FRESH role=" + r + "".join(f" a{i}=0x0" for i in range(8)))
    pc = 0x40_1000
    for ri, role in enumerate(ROLES):
        if role not in CALLS:
            continue
        for c in range(1, CALLS[role] + 1):
            key = (KEY_HI << 32) | (ri << 24) | c
            a0 = {"W": 0 if c % 4 == 0 else 1, "RW": 1}.get(role, 0)
            arm = "same" if (role.startswith("Y") and (c % 2 or c > 24)) or (role == "W" and c % 2 == 0) else "switched"
            nr = NR[role]
            out.append(f"RISCV_ECALL_WITNESS_RESUME origin=bridge arm={arm} tid={10000+ri} key=0x{key:x} "
                       f"sepc=0x{pc+4:x} a0=0x{a0:x} a3=0x{0xa3+c:x} a7=0x{nr:x}")
            out.append(f"USER_LOG tid={10000+ri} msg=EA7W role={role} cycle={c} key=0x{key:x} nr={nr} a0=0x{a0:x} "
                       f"a1=0x0 a2=0x0 a3=0x{0xa3+c:x} a7=0x{nr:x} want_a3=0x{0xa3+c:x} want_a7=0x{nr:x} bad=0x0 pc=0x{pc:x}")
    for c in range(1, RCALLS + 1):
        out.append(f"USER_LOG tid=6 msg=EA7W_RQ cycle={c} requeued=1 woke=1 back=1")
    for c in range(1, CALLS["R"] + 1):
        out.append(f"USER_LOG tid=7 msg=EA7W_IPC cycle={c} a0=0x0 status=0x0 len=8 body_ok=1")
    for c in range(1, PCALLS + 1):
        key = (KEY_HI << 32) | (8 << 24) | c
        out.append(f"RISCV_ECALL_WITNESS_RESUME origin=bridge arm=async tid=10008 key=0x{key:x} sepc=0x40200c "
                   f"a0=0x1 a3=0x1 a7=0x1")
        out.append(f"USER_LOG tid=10008 msg=EA7W_SPIN cycle={c} key=0x{key:x} bad=0x0")
    out.append("USER_LOG tid=4 msg=EA7W_DONE spawned=9 done=9 sent=8 bad=0 result=ok")
    return out


def self_test():
    base = fixture()
    f, _, _, _ = grade(base, 0)
    assert not f, f
    def mut(fn):
        lines = list(base)
        fn(lines)
        return grade(lines, 0)[0]
    def first(lines, needle):
        return next(i for i, l in enumerate(lines) if needle in l)
    cases = {
        "core smoke rejected": lambda: grade(base, 1)[0],
        "a7 lost": lambda: mut(lambda L: L.__setitem__(first(L, "role=W cycle=1 "), L[first(L, "role=W cycle=1 ")].replace("bad=0x0", "bad=0x80"))),
        "kernel marker missing": lambda: mut(lambda L: L.pop(first(L, "RISCV_ECALL_WITNESS_RESUME origin=bridge arm=switched tid=10003"))),
        "kernel marker duplicated": lambda: mut(lambda L: L.insert(0, L[first(L, "arm=switched tid=10003")])),
        "wrong resume pc": lambda: mut(lambda L: L.__setitem__(first(L, "arm=switched tid=10003"), L[first(L, "arm=switched tid=10003")].replace("sepc=0x401004", "sepc=0x401008"))),
        "damaged marker": lambda: mut(lambda L: L.__setitem__(first(L, "role=Y0 cycle=2 "), L[first(L, "role=Y0 cycle=2 ")].replace("cycle=2 ", "cyc3le=2 "))),
        "damaged kernel marker": lambda: mut(lambda L: L.__setitem__(first(L, "arm=switched tid=10003"), L[first(L, "arm=switched tid=10003")].replace("origin=bridge", "orig4in=bridge"))),
        "kernel/user disagree": lambda: mut(lambda L: L.__setitem__(first(L, "arm=switched tid=10003"), L[first(L, "arm=switched tid=10003")].replace(" a7=0x9", " a7=0x0"))),
        "missing DONE": lambda: mut(lambda L: L.pop(first(L, "EA7W_DONE"))),
        "fresh a7 non-zero": lambda: mut(lambda L: L.__setitem__(first(L, "EA7W_FRESH role=Y1"), L[first(L, "EA7W_FRESH role=Y1")].replace("a7=0x0", "a7=0x9"))),
        "no switched yield": lambda: grade([l.replace("arm=switched tid=10000", "arm=same tid=10000").replace("arm=switched tid=10001", "arm=same tid=10001").replace("arm=switched tid=10002", "arm=same tid=10002") for l in base], 0)[0],
        "no async spin": lambda: grade([l.replace("arm=async", "arm=same") for l in base], 0)[0],
        "duplicate user cycle": lambda: mut(lambda L: L.append(L[first(L, "role=R cycle=3 ")])),
        "orphan kernel key": lambda: mut(lambda L: L.append(L[first(L, "arm=switched tid=10007")].replace("cycle", "").replace(f"key=0x{(KEY_HI << 32) | (7 << 24) | 1:x}", f"key=0x{(KEY_HI << 32) | (7 << 24) | 99:x}"))),
        "requeue round failed": lambda: mut(lambda L: L.__setitem__(first(L, "EA7W_RQ cycle=2"), "USER_LOG tid=6 msg=EA7W_RQ cycle=2 requeued=0 woke=0 back=1")),
        "spin register lost": lambda: mut(lambda L: L.__setitem__(first(L, "EA7W_SPIN cycle=2"), L[first(L, "EA7W_SPIN cycle=2")].replace("bad=0x0", "bad=0x80"))),
        "yield result wrong": lambda: mut(lambda L: [L.__setitem__(i, L[i].replace("a0=0x0", "a0=0x5")) for i in [first(L, "role=Y2 cycle=4 "), first(L, "arm=same tid=10002 key=0x" + f"{(KEY_HI << 32) | (2 << 24) | 3:x}")] ]),
    }
    expect = {"damaged marker": "damaged witness marker", "damaged kernel marker": "damaged witness marker"}
    for name, run in cases.items():
        got = run()
        assert got, f"self-test case '{name}' was not caught"
        if name in expect:
            assert any(expect[name] in g for g in got), f"self-test case '{name}': {got}"
    print(f"[riscv-ecall-grade] self-test ok ({len(cases)} refusals, 1 pass)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--smp", default="2")
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    lines = open(a.log, errors="replace").read().splitlines()
    fails, cov, probes, fresh = grade(lines, a.core_rc)
    for f in fails:
        print(f"[riscv-ecall-grade][fail] {f}")
    result = "fail" if fails else "ok"
    print(f"RISCV_ECALL_WITNESS_SEAL arch=riscv64 smp={a.smp} probes={probes} fresh={fresh} "
          f"yield_switched={cov['y_switch']} yield_same={cov['y_same']} wait_parked={cov['w_parked']} "
          f"wait_own={cov['w_own']} wait_early={cov['w_early']} wait_mismatch={cov['w_mismatch']} requeue_parked={cov['rw_parked']} recv_blocked={cov['r_blocked']} "
          f"spin_async={cov['spin_async']} solo_switched={cov['solo_switched']} async_in_probe={cov['async_in_probe']} lost_a3={cov['lost_a3']} "
          f"lost_a7={cov['lost_a7']} lost_other={cov['lost_other']} "
          f"core_smoke={'ok' if a.core_rc == 0 else f'rejected(rc={a.core_rc})'} result={result}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
