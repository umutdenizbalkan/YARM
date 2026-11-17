#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""FUTEX-SYNC-TIME1 — grade one live timed mutex / condition-variable witness boot.

Input: the normalized serial log of a boot whose kernel is built with `futex-time-witness` and
whose init is built with `futex-sync-witness`, booted with `yarm.futex_requeue_witness=1`. Init's
cell (roles W, H, A, X, C) runs `yarm_user_rt::futex_sync` over the real system calls and logs:

  FSW_OP  seq role op deadline now                    before each graded operation
  FSW_RET seq role result held owner_ok state_ok word now   after it (held: mutex word nonzero;
                                                       owner_ok: an exclusive owner slot was
                                                       taken under the mutex; state_ok: the two
                                                       protected counters agreed)
  FSW_H_UNLOCK seq woke, FSW_H_PARKED seq n ok, FSW_H_C1 parked1 parked2,
  FSW_C3_BCAST woken requeued calls fallback word, FSW_C3_RELEASE now, FSW_THREADS, FSW_BEGIN
  word seq, FSW_WAITER_DONE fails, FSW_DONE fails result, FSW_FAIL what

The kernel's FUTEX-TIME1 / FUTEX-ABS-BITSET1 markers record every TIMED wait (untimed waits have
none): FUTEX_TIME_W_ADMIT tid deadline now mask result, _REGISTER tid gen deadline now va mask,
_WAKE tid gen va mask, _TIMEOUT cpu tid gen now current va deadline mask, _LOST cpu tid gen.

Markers fail closed: a damaged marker fails the boot.

Rules:
  R1 every graded operation appears once, OP before RET, logged by its role's thread:
     L1 W, L2 W, L3 W, L3 A, L3b W, C1 W, C2 W, C3 A, C3 X, C3 C.
  R2 results: L1 acquired; L2 timed_out; L3 W timed_out; L3 A acquired; L3b W acquired;
     C1 satisfied; C2 timed_out; C3 A returned; C3 X timed_out; C3 C notified.
  R3 ownership: every return that holds the mutex (an acquired lock, every condition return)
     has held=1 owner_ok=1 state_ok=1; a timed-out lock has word=2 (CONTENDED kept: the owner's
     unlock still wakes) and held=0 by its caller (the helper owns it).
  R4 ONE DEADLINE: every kernel ADMIT of the operation's thread between its OP and RET names the
     operation's deadline (no retry extends or resets it). L1 (uncontended) has no ADMIT at all.
     C1 has at least two registered admissions (the re-wait after the spurious signal).
  R5 terminal owners:
     L2: a TIMEOUT of W on the mutex word, now >= deadline.
     L3: H_PARKED L3 n=2 ok=1; W's TIMEOUT on the mutex word before H_UNLOCK seq=L3; A's RET
         after that unlock (the parked untimed waiter still acquired after the timeout).
     L3b: W's cycle ended by a WAKE on the mutex word, no TIMEOUT.
     C1: H_C1 parked1=1 parked2=1; every C1 cycle of W ended by a WAKE (never a TIMEOUT).
     C2: a TIMEOUT of W on the sequence word.
     C3: H_PARKED C3 n=3 ok=1 before the broadcast; BCAST woken=1 requeued=2 fallback=0 on the
         mutex word; X's registration on the SEQUENCE word before the broadcast; X's TIMEOUT on
         the MUTEX word between BCAST and RELEASE (it timed out after its requeue), with its
         registered deadline; C's cycle ended by a WAKE on the mutex word after RELEASE; the three
         RETs after RELEASE.
  R6 FSW_WAITER_DONE fails=0, FSW_DONE result=ok, no FSW_FAIL, the core smoke clean.

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
OPS = [("L1", "W"), ("L2", "W"), ("L3", "W"), ("L3", "A"), ("L3b", "W"), ("C1", "W"),
       ("C2", "W"), ("C3", "A"), ("C3", "X"), ("C3", "C")]
RESULT = {("L1", "W"): "acquired", ("L2", "W"): "timed_out", ("L3", "W"): "timed_out",
          ("L3", "A"): "acquired", ("L3b", "W"): "acquired", ("C1", "W"): "satisfied",
          ("C2", "W"): "timed_out", ("C3", "A"): "returned", ("C3", "X"): "timed_out",
          ("C3", "C"): "notified"}
LOCK_TIMEOUTS = {("L2", "W"), ("L3", "W")}

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
}
U = {
    "BEGIN": {"s_tid": INT, "word": HEX, "seq": HEX},
    "THREADS": {"W": INT, "H": INT, "A": INT, "X": INT, "C": INT},
    "OP": {"seq": r"L1|L2|L3|L3b|C1|C2|C3", "role": "W|H|A|X|C", "op": r"[a-z_]+",
           "deadline": INT, "now": INT},
    "RET": {"seq": r"L1|L2|L3|L3b|C1|C2|C3", "role": "W|H|A|X|C", "result": r"[a-z_]+",
            "held": "[01]", "owner_ok": "[01]", "state_ok": "[01]", "word": "[012]", "now": INT},
    "H_UNLOCK": {"seq": r"L2|L3|L3b", "woke": INT},
    "H_PARKED": {"seq": r"L3|L3b|C3", "n": INT, "ok": "[01]"},
    "H_C1": {"parked1": "[01]", "parked2": "[01]"},
    "C3_BCAST": {"woken": INT, "requeued": INT, "calls": INT, "fallback": "[01]", "word": HEX},
    "C3_RELEASE": {"now": INT},
    "WAITER_DONE": {"fails": INT},
    "DONE": {"fails": INT, "result": "ok|fail"},
    "FAIL": {"what": r"\S+"},
}
FRAGMENTS = ("FUTEX_TIME_W", "UTEX_TIME", "TIME_W_", "msg=FSW_", "FSW_OP", "FSW_RET", "FSW_DONE")


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s,]+)", line))


def parse(line):
    for kind, (head, fields) in K.items():
        if re.match(rf"^{head} ", line):
            d = kv(line)
            for k, pat in fields.items():
                if k not in d or not re.fullmatch(pat, d[k]):
                    return "DAMAGED", f"{head}: field `{k}` missing or malformed"
            return kind, d
    if re.match(r"^FUTEX_TIME_W_(WINDOW|IN_WINDOW|BITSET_SKIP)", line):
        return None, None  # FUTEX-TIME1's own diagnostics; not graded here
    m = re.match(r"^USER_LOG tid=(\d+) msg=FSW_([A-Z0-9_]+?) (.*)$", line)
    if m:
        name = m.group(2)
        if name not in U:
            return "DAMAGED", f"FSW_{name}: unknown cell line"
        d = kv(m.group(3))
        for k, pat in U[name].items():
            if k not in d or not re.fullmatch(pat, d[k]):
                return "DAMAGED", f"FSW_{name}: field `{k}` missing or malformed"
        d["utid"] = m.group(1)
        return "U_" + name, d
    if any(f in line for f in FRAGMENTS):
        return "DAMAGED", "damaged witness marker"
    return None, None


def grade(lines, arch):
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

    def only(kind, **kw):
        return [e for e in ev if e["kind"] == kind and all(e.get(k) == v for k, v in kw.items())]

    def one(kind, **kw):
        xs = only(kind, **kw)
        if len(xs) != 1:
            fail(f"expected exactly one {kind} {kw or ''}, saw {len(xs)}")
            return None
        return xs[0]

    for e in only("U_FAIL"):
        fail(f"cell reported FSW_FAIL what={e['what']}")
    begin, th = one("U_BEGIN"), one("U_THREADS")
    done, wdone = one("U_DONE"), one("U_WAITER_DONE")
    if done and done["result"] != "ok":
        fail(f"FSW_DONE result={done['result']}")
    if wdone and wdone["fails"] != "0":
        fail(f"FSW_WAITER_DONE fails={wdone['fails']}")
    if not (begin and th):
        return fails, {}
    tid = {r: th[r] for r in "WHAXC"}
    mword, seqw = begin["word"], begin["seq"]
    facts = {}

    # ── R1–R4 per operation ──────────────────────────────────────────────────────────────
    spans = {}
    for seq, role in OPS:
        op = one("U_OP", seq=seq, role=role)
        rt = one("U_RET", seq=seq, role=role)
        if not (op and rt):
            continue
        if op["utid"] != tid[role] or rt["utid"] != tid[role]:
            fail(f"{seq} {role}: logged by the wrong thread")
        if not op["at"] < rt["at"]:
            fail(f"{seq} {role}: RET before OP")
            continue
        spans[(seq, role)] = (op, rt)
        want = RESULT[(seq, role)]
        if rt["result"] != want:
            fail(f"{seq} {role}: result {rt['result']}, the sequence requires {want}")
        if (seq, role) in LOCK_TIMEOUTS:
            if rt["word"] != "2":
                fail(f"{seq} {role}: a timed-out lock left word={rt['word']} (CONTENDED must stay)")
        elif (rt["held"], rt["owner_ok"], rt["state_ok"]) != ("1", "1", "1"):
            fail(f"{seq} {role}: returned without proven ownership: held={rt['held']} "
                 f"owner_ok={rt['owner_ok']} state_ok={rt['state_ok']}")
        t = tid[role]
        admits = [e for e in only("ADMIT", tid=t) if op["at"] < e["at"] < rt["at"]]
        if seq == "L1":
            if admits:
                fail("L1: an uncontended timed lock entered the kernel")
            continue
        if op["deadline"] == "0":
            continue  # an untimed operation (A's)
        if not admits:
            fail(f"{seq} {role}: no kernel admission of its timed wait")
        for a in admits:
            if a["deadline"] != op["deadline"]:
                fail(f"{seq} {role}: a retry was admitted with deadline {a['deadline']}, the "
                     f"operation's is {op['deadline']} (one deadline per operation)")
        facts[f"admits_{seq}_{role}"] = len(admits)

    def cycles(seq, role):
        if (seq, role) not in spans:
            return [], []
        op, rt = spans[(seq, role)]
        t = tid[role]
        regs = [e for e in only("REGISTER", tid=t) if op["at"] < e["at"] < rt["at"]]
        terms = [e for e in ev if e["kind"] in ("WAKE", "TIMEOUT") and e["tid"] == t
                 and any(e["gen"] == r["gen"] for r in regs)]
        return regs, terms

    def lines_of(kind, **kw):
        return only(kind, **kw)

    # ── R5 per sequence ──────────────────────────────────────────────────────────────────
    regs, terms = cycles("L2", "W")
    to = [e for e in terms if e["kind"] == "TIMEOUT"]
    if len(to) != 1 or to[0]["va"] != mword or int(to[0]["now"]) < int(to[0]["deadline"]):
        fail(f"L2: W's wait must end by one TIMEOUT on the mutex word: {[(e['kind'], e['va']) for e in terms]}")
    regs, terms = cycles("L3", "W")
    hp = one("U_H_PARKED", seq="L3", n="2")
    hu = one("U_H_UNLOCK", seq="L3")
    to = [e for e in terms if e["kind"] == "TIMEOUT"]
    if hp and hp["ok"] != "1":
        fail("L3: H could not confirm both waiters parked")
    if len(to) != 1 or to[0]["va"] != mword:
        fail(f"L3: W's wait must end by one TIMEOUT on the mutex word: {[(e['kind'], e['va']) for e in terms]}")
    elif hu and not to[0]["at"] < hu["at"]:
        fail("L3: W timed out after H unlocked (no timeout under the held mutex)")
    if hu and ("L3", "A") in spans and not hu["at"] < spans[("L3", "A")][1]["at"]:
        fail("L3: A returned before H's unlock")
    regs, terms = cycles("L3b", "W")
    if not terms or any(e["kind"] != "WAKE" or e["va"] != mword for e in terms):
        fail(f"L3b: W's waits must end by a WAKE on the mutex word: {[(e['kind'], e['va']) for e in terms]}")
    c1 = one("U_H_C1")
    if c1 and (c1["parked1"], c1["parked2"]) != ("1", "1"):
        fail(f"C1: the parks were not both confirmed: {c1}")
    regs, terms = cycles("C1", "W")
    if len(regs) < 2:
        fail(f"C1: {len(regs)} registered waits; the spurious signal must cause a re-wait")
    if any(e["kind"] != "WAKE" for e in terms) or len(terms) != len(regs):
        fail(f"C1: every C1 wait must end by a WAKE: {[e['kind'] for e in terms]}")
    facts["c1_waits"] = len(regs)
    regs, terms = cycles("C2", "W")
    to = [e for e in terms if e["kind"] == "TIMEOUT"]
    if len(to) != 1 or to[0]["va"] != seqw:
        fail(f"C2: W's wait must end by one TIMEOUT on the sequence word: {[(e['kind'], e['va']) for e in terms]}")
    hp3 = one("U_H_PARKED", seq="C3", n="3")
    bc = one("U_C3_BCAST")
    rel = one("U_C3_RELEASE")
    if hp3 and hp3["ok"] != "1":
        fail("C3: H could not confirm three parked waiters")
    if bc:
        if (bc["woken"], bc["requeued"], bc["fallback"]) != ("1", "2", "0"):
            fail(f"C3: broadcast woken={bc['woken']} requeued={bc['requeued']} "
                 f"fallback={bc['fallback']} (need 1 / 2 / 0)")
        if bc["word"] != mword:
            fail("C3: the broadcast names another mutex word")
        if hp3 and not hp3["at"] < bc["at"]:
            fail("C3: broadcast before the parks were confirmed")
    if bc and rel:
        regs, terms = cycles("C3", "X")
        if not regs or regs[0]["va"] != seqw or not regs[0]["at"] < bc["at"]:
            fail("C3: X did not register on the sequence word before the broadcast")
        to = [e for e in terms if e["kind"] == "TIMEOUT"]
        if len(to) != 1:
            fail(f"C3: X's wait must end by one TIMEOUT: {[e['kind'] for e in terms]}")
        else:
            x = to[0]
            if x["va"] != mword:
                fail(f"C3: X timed out on {x['va']}, not on the mutex word it was requeued to")
            if not bc["at"] < x["at"] < rel["at"]:
                fail("C3: X's timeout is not between the broadcast and the release")
            if regs and x["deadline"] != regs[0]["deadline"]:
                fail("C3: X's deadline changed across the requeue")
        regs, terms = cycles("C3", "C")
        if len(terms) != 1 or terms[0]["kind"] != "WAKE" or terms[0]["va"] != mword \
                or not rel["at"] < terms[0]["at"]:
            fail(f"C3: C's wait must end by a WAKE on the mutex word after the release: "
                 f"{[(e['kind'], e['va']) for e in terms]}")
        for role in "AXC":
            if ("C3", role) in spans and not rel["at"] < spans[("C3", role)][1]["at"]:
                fail(f"C3: {role} returned before the release (without the mutex)")
    facts["ops"] = len(spans)
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


# ── self-test ────────────────────────────────────────────────────────────────────────────────
def synth():
    T = {"W": "20", "H": "21", "A": "22", "X": "23", "C": "24"}
    MW, SQ = "0x5000", "0x5004"
    out = [f"USER_LOG tid=1 msg=FSW_BEGIN s_tid=1 word={MW} seq={SQ}",
           "USER_LOG tid=1 msg=FSW_THREADS W=20 H=21 A=22 X=23 C=24"]
    g = {"gen": 0}

    def u(role, msg):
        out.append(f"USER_LOG tid={T[role]} msg={msg}")

    def op(seq, role, opn, d):
        u(role, f"FSW_OP seq={seq} role={role} op={opn} deadline={d} now=1")

    def ret(seq, role, res, held=1, word=2):
        u(role, f"FSW_RET seq={seq} role={role} result={res} held={held} owner_ok=1 state_ok=1 word={word} now=2")

    def reg(role, d, va):
        g["gen"] += 1
        out.append(f"FUTEX_TIME_W_ADMIT tid={T[role]} deadline={d} now=5 mask=0xffffffff result=registered")
        out.append(f"FUTEX_TIME_W_REGISTER tid={T[role]} gen={g['gen']} deadline={d} now=5 va={va} mask=0xffffffff")
        return g["gen"]

    def wake(role, gen, va):
        out.append(f"FUTEX_TIME_W_WAKE tid={T[role]} gen={gen} va={va} mask=0xffffffff")

    def tmo(role, gen, va, d):
        out.append(f"FUTEX_TIME_W_TIMEOUT cpu=0 tid={T[role]} gen={gen} now={d + 1} current=0 va={va} deadline={d} mask=0xffffffff")

    op("L1", "W", "lock_until", 0)
    ret("L1", "W", "acquired", word=1)
    op("L2", "W", "lock_until", 400)
    gen = reg("W", 400, MW)
    tmo("W", gen, MW, 400)
    ret("L2", "W", "timed_out", held=1)
    u("H", "FSW_H_UNLOCK seq=L2 woke=0")
    op("L3", "A", "lock", 0)
    u("H", "FSW_H_PARKED seq=L3 n=1 ok=1")
    op("L3", "W", "lock_until", 1500)
    gen = reg("W", 1500, MW)
    u("H", "FSW_H_PARKED seq=L3 n=2 ok=1")
    tmo("W", gen, MW, 1500)
    ret("L3", "W", "timed_out", held=1)
    u("H", "FSW_H_UNLOCK seq=L3 woke=1")
    ret("L3", "A", "acquired")
    op("L3b", "W", "lock_until", 9000)
    gen = reg("W", 9000, MW)
    u("H", "FSW_H_PARKED seq=L3b n=1 ok=1")
    u("H", "FSW_H_UNLOCK seq=L3b woke=1")
    wake("W", gen, MW)
    ret("L3b", "W", "acquired")
    op("C1", "W", "wait_while_until", 20000)
    gen = reg("W", 20000, SQ)
    wake("W", gen, SQ)
    gen = reg("W", 20000, SQ)
    wake("W", gen, SQ)
    u("H", "FSW_H_C1 parked1=1 parked2=1")
    ret("C1", "W", "satisfied")
    op("C2", "W", "wait_until", 21000)
    gen = reg("W", 21000, SQ)
    tmo("W", gen, SQ, 21000)
    ret("C2", "W", "timed_out")
    op("C3", "A", "wait", 0)
    u("H", "FSW_H_PARKED seq=C3 n=1 ok=1")
    op("C3", "X", "wait_until", 30000)
    gx = reg("X", 30000, SQ)
    u("H", "FSW_H_PARKED seq=C3 n=2 ok=1")
    op("C3", "C", "wait_until", 90000)
    gc = reg("C", 90000, SQ)
    u("H", "FSW_H_PARKED seq=C3 n=3 ok=1")
    u("H", f"FSW_C3_BCAST woken=1 requeued=2 calls=1 fallback=0 word={MW}")
    tmo("X", gx, MW, 30000)
    u("H", "FSW_C3_RELEASE now=33000")
    wake("C", gc, MW)
    ret("C3", "C", "notified")
    ret("C3", "A", "returned")
    ret("C3", "X", "timed_out")
    u("W", "FSW_WAITER_DONE fails=0")
    out.append("USER_LOG tid=1 msg=FSW_DONE fails=0 result=ok")
    return out


def self_test():
    good = synth()
    for a in ARCHES:
        f, _ = grade(good, a)
        assert not f, f

    def must_fail(lines, why, needle):
        f, _ = grade(lines, "riscv64")
        assert f, f"must fail: {why}"
        assert any(needle in x for x in f), (why, needle, f)

    def edit(old, new, nth=0):
        hits = [i for i, l in enumerate(good) if old in l]
        assert len(hits) > nth, old
        out = list(good)
        out[hits[nth]] = out[hits[nth]].replace(old, new)
        return out

    def drop(needle, nth=0):
        hits = [i for i, l in enumerate(good) if needle in l]
        assert len(hits) > nth, needle
        return good[:hits[nth]] + good[hits[nth] + 1:]

    def move_after(needle, anchor):
        i = next(k for k, l in enumerate(good) if needle in l)
        rest = good[:i] + good[i + 1:]
        j = next(k for k, l in enumerate(rest) if anchor in l)
        return rest[:j + 1] + [good[i]] + rest[j + 1:]

    must_fail(edit("FUTEX_TIME_W_TIMEOUT cpu=0", "FUTEX_TIME_W_TIMEOUT cpu=x"), "damaged", "malformed")
    must_fail(edit("msg=FSW_RET seq=L1", "msg=FSW_RTE seq=L1"), "unknown line", "unknown cell line")
    must_fail(good + ["USER_LOG tid=20 msg=FSW_FAIL what=L2"], "cell fail", "FSW_FAIL")
    must_fail(edit("result=ok", "result=fail"), "done fail", "FSW_DONE result")
    must_fail(good + ["panicked at foo"], "fatal", "fatal")
    # results and ownership (NC: a condition timeout returning without the mutex)
    must_fail(edit("seq=C2 role=W result=timed_out held=1", "seq=C2 role=W result=timed_out held=0"),
              "condvar timeout without the mutex", "without proven ownership")
    must_fail(edit("seq=C3 role=X result=timed_out held=1 owner_ok=1", "seq=C3 role=X result=timed_out held=1 owner_ok=0"),
              "requeued timeout without ownership", "without proven ownership")
    must_fail(edit("seq=L2 role=W result=timed_out held=1 owner_ok=1 state_ok=1 word=2",
                   "seq=L2 role=W result=timed_out held=1 owner_ok=1 state_ok=1 word=1"),
              "CONTENDED dropped on timeout", "CONTENDED must stay")
    must_fail(edit("seq=L3b role=W result=acquired", "seq=L3b role=W result=timed_out"), "L3b", "requires acquired")
    must_fail(edit("seq=C1 role=W result=satisfied", "seq=C1 role=W result=timed_out"), "C1", "requires satisfied")
    # one deadline (NC: reset on retry)
    i = [k for k, l in enumerate(good) if "FUTEX_TIME_W_ADMIT tid=20 deadline=20000" in l][1]
    bad = list(good)
    bad[i] = bad[i].replace("deadline=20000", "deadline=20001")
    must_fail(bad, "C1 re-wait with a new deadline", "one deadline per operation")
    must_fail(edit("FUTEX_TIME_W_ADMIT tid=20 deadline=1500", "FUTEX_TIME_W_ADMIT tid=20 deadline=1600"),
              "L3 deadline drift", "one deadline per operation")
    l1 = next(k for k, l in enumerate(good) if "seq=L1 role=W op=" in l)
    must_fail(good[:l1 + 1] + ["FUTEX_TIME_W_ADMIT tid=20 deadline=0 now=5 mask=0xffffffff result=timed_out"] + good[l1 + 1:],
              "L1 entered the kernel", "uncontended")
    # C1 re-wait (NC: predicate recheck suppressed — a single wait)
    c1 = [k for k, l in enumerate(good) if "tid=20" in l and ("deadline=20000" in l or ("FUTEX_TIME_W_WAKE" in l and "va=0x5004" in l))]
    must_fail([l for k, l in enumerate(good) if k not in c1[3:6]], "C1 one wait", "re-wait")
    must_fail(edit("parked2=1", "parked2=0"), "C1 confirm", "not both confirmed")
    # terminal owners
    must_fail(edit("FUTEX_TIME_W_TIMEOUT cpu=0 tid=23", "FUTEX_TIME_W_WAKE tid=23"), "X woken", "X's wait must end by one TIMEOUT")
    must_fail([l.replace("va=0x5000", "va=0x5004") if l.startswith("FUTEX_TIME_W_TIMEOUT") and "tid=23" in l else l
               for l in good], "X timed out on the sequence", "not on the mutex word")
    must_fail(move_after("FUTEX_TIME_W_TIMEOUT cpu=0 tid=23", "FSW_C3_RELEASE"), "X timeout after release",
              "between the broadcast")
    must_fail(move_after("seq=C3 role=X result", "FSW_C3_BCAST"), "X returned under the broadcaster",
              "before the release")
    must_fail(edit("woken=1 requeued=2", "woken=3 requeued=0"), "wake-all", "need 1 / 2 / 0")
    must_fail(edit("fallback=0", "fallback=1"), "fallback", "need 1 / 2 / 0")
    must_fail(move_after("FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=2", "seq=L3 woke=1"), "L3 timeout after unlock",
              "after H unlocked")
    must_fail(move_after("seq=L3 role=A result", "seq=L3 n=2 ok=1"), "A before unlock", "before H's unlock")
    must_fail([l.replace("va=0x5000", "va=0x5004") if l.startswith("FUTEX_TIME_W_TIMEOUT") and "deadline=400 " in l else l
               for l in good], "L2 on another word", "L2:")
    must_fail(edit("FUTEX_TIME_W_TIMEOUT cpu=0 tid=20 gen=6", "FUTEX_TIME_W_WAKE tid=20 gen=6"), "C2 woken", "C2:")
    must_fail(drop("msg=FSW_RET seq=C3 role=C"), "C never returned", "U_RET")
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
        print("FUTEX_SYNC_WITNESS_GRADER_SELF_TEST result=ok")
        return 0
    if not a.arch or not a.log:
        ap.error("--arch and a log are required")
    with open(a.log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    fails, facts = grade(lines, a.arch)
    core_ok, reasons = core_verdict(a.core_rc, a.core_log)
    if not core_ok:
        fails.append(f"core smoke not clean (rc={a.core_rc}): {reasons[:5]}")
    if a.smp < 2:
        fails.append("the witness needs SMP >= 2")
    for f in fails:
        print(f"[futex-sync-witness][fail] {f}")
    fact_s = " ".join(f"{k}={v}" for k, v in facts.items())
    result = "ok" if not fails else "fail"
    print(f"FUTEX_SYNC_WITNESS_SEAL arch={a.arch} smp={a.smp} result={result} {fact_s}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
